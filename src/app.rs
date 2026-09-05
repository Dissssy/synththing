//! TUI state, rendering and input handling.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use rustysynth::{MidiFile, SoundFont};

use crate::config::{nice_name, Config};
use crate::engine::Engine;
use crate::fft::FastFourierTransform;
use crate::filebrowser::{FileBrowser, Outcome};
use crate::visualizer::{SampleTap, VisualizerController};

const VISUALIZER_WIDTH: usize = 800;
const VISUALIZER_HEIGHT: usize = 400;

/// Which panel currently takes navigation keys.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Browser,
    SoundFonts,
}

impl Focus {
    fn toggled(self) -> Self {
        match self {
            Focus::Browser => Focus::SoundFonts,
            Focus::SoundFonts => Focus::Browser,
        }
    }
}

pub struct App {
    engine: Arc<Mutex<Engine>>,
    config: Config,
    /// Always-visible song file browser. Picking a `.mid` plays it immediately.
    browser: FileBrowser,
    sf_state: ListState,
    active_sf: Option<usize>,
    focus: Focus,
    /// Popup browser, open only while adding a soundfont to the retained list.
    sf_modal: Option<FileBrowser>,
    visualizer: VisualizerController<FastFourierTransform>,
    status: String,
    should_quit: bool,
}

impl App {
    pub fn new(engine: Arc<Mutex<Engine>>, config: Config, tap: SampleTap) -> Self {
        let browser = FileBrowser::new("songs", config.browse_start_dir(), &["mid", "midi"]);
        let mut sf_state = ListState::default();
        if !config.soundfonts.is_empty() {
            sf_state.select(Some(0));
        }
        Self {
            engine,
            config,
            browser,
            sf_state,
            active_sf: None,
            focus: Focus::Browser,
            sf_modal: None,
            visualizer: VisualizerController::new(
                tap,
                VISUALIZER_WIDTH,
                VISUALIZER_HEIGHT,
                "synththing — visualizer",
            ),
            status: "↑↓ browse · ⏎ play a .mid or open a folder · / fuzzy search · Tab/o switch panel"
                .to_string(),
            should_quit: false,
        }
    }

    /// Load the first existing soundfont from the retained list, so there is
    /// sound the moment a song is chosen.
    pub fn autoload_first_soundfont(&mut self) {
        if let Some(idx) = self.config.soundfonts.iter().position(|p| p.exists()) {
            self.sf_state.select(Some(idx));
            self.activate_soundfont(idx);
        }
    }

    /// Force the visualizer window open or closed, e.g. from a `--visualizer`
    /// startup flag. A no-op if it's already in that state.
    pub fn set_visualizer_open(&mut self, open: bool) {
        if self.visualizer.is_open() != open {
            self.visualizer.toggle();
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key);
            }
        }
        Ok(())
    }

    // --- actions -------------------------------------------------------

    fn play_path(&mut self, path: PathBuf) {
        let name = nice_name(&path);
        match load_midi(&path) {
            Ok(midi) => {
                self.engine.lock().unwrap().load_midi(midi, name.clone());
                self.browser.set_active(Some(path));
                self.status = if self.active_sf.is_some() {
                    format!("Playing: {name}")
                } else {
                    format!("Loaded {name} — pick a soundfont to hear it.")
                };
            }
            Err(e) => self.status = format!("Failed to read '{name}': {e}"),
        }
    }

    fn activate_soundfont(&mut self, idx: usize) {
        let Some(path) = self.config.soundfonts.get(idx).cloned() else {
            return;
        };
        if !path.exists() {
            self.status = format!("File is missing: {}", path.display());
            return;
        }
        let name = nice_name(&path);
        match probe_soundfont(&path) {
            Ok(SoundFontProbe::Playable(soundfont)) => {
                let result = self
                    .engine
                    .lock()
                    .unwrap()
                    .set_soundfont(soundfont, name.clone());
                match result {
                    Ok(()) => {
                        self.active_sf = Some(idx);
                        self.status = format!("Soundfont loaded: {name}");
                    }
                    Err(e) => self.status = format!("Soundfont error: {e}"),
                }
            }
            Ok(SoundFontProbe::Unplayable(reason)) => {
                self.status = format!("Can't load '{name}': {reason}");
            }
            Ok(SoundFontProbe::NotSoundFont) => {
                self.status = format!("'{name}' is not a SoundFont file (no RIFF/sfbk header).");
            }
            Err(e) => self.status = format!("Failed to read '{name}': {e}"),
        }
    }

    fn add_soundfont(&mut self, path: PathBuf) {
        let name = nice_name(&path);

        // Only refuse files that aren't SoundFont containers at all. A valid
        // container that this build of rustysynth can't render yet (e.g. a
        // compressed SF3) is still added to the list, with a note.
        let note = match probe_soundfont(&path) {
            Ok(SoundFontProbe::Playable(_)) => None,
            Ok(SoundFontProbe::Unplayable(reason)) => Some(reason),
            Ok(SoundFontProbe::NotSoundFont) => {
                self.status = format!("'{name}' is not a SoundFont file (no RIFF/sfbk header).");
                return;
            }
            Err(e) => {
                self.status = format!("Failed to read '{name}': {e}");
                return;
            }
        };

        if !self.config.add_soundfont(&path) {
            self.status = "That soundfont is already in the list.".to_string();
            return;
        }
        let save_err = self.config.save().err();
        if self.sf_state.selected().is_none() {
            self.sf_state.select(Some(0));
        }

        self.status = match (note, save_err) {
            (Some(reason), _) => format!("Added '{name}', but {reason}"),
            (None, Some(e)) => format!("Added '{name}', but failed to save the list: {e}"),
            (None, None) => format!("Added soundfont: {name}"),
        };
    }

    fn remove_selected_soundfont(&mut self) {
        let Some(idx) = self.sf_state.selected() else {
            return;
        };
        if idx >= self.config.soundfonts.len() {
            return;
        }
        let name = nice_name(&self.config.soundfonts[idx]);
        self.config.remove_soundfont(idx);
        self.status = match self.config.save() {
            Err(e) => format!("Removed {name}, but failed to save list: {e}"),
            Ok(()) => format!("Removed soundfont: {name}"),
        };
        self.active_sf = shift_active(self.active_sf, idx);
        fix_selection(&mut self.sf_state, self.config.soundfonts.len(), idx);
    }

    fn move_sf(&mut self, delta: isize) {
        let len = self.config.soundfonts.len();
        if len == 0 {
            return;
        }
        let current = self.sf_state.selected().unwrap_or(0) as isize;
        self.sf_state
            .select(Some((current + delta).rem_euclid(len as isize) as usize));
    }

    fn open_soundfont_browser(&mut self) {
        self.sf_modal = Some(FileBrowser::new(
            "Add SoundFont (.sf2)",
            self.config.browse_start_dir(),
            &["sf2"],
        ));
    }

    // --- input -------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        if self.sf_modal.is_some() {
            self.on_sf_modal_key(key);
            return;
        }

        // While the song browser is mid `/` search, it owns the keyboard: any
        // letter is query text, so none of the single-key shortcuts below
        // (space, o, a, r, ...) may intercept it first. Esc there cancels the
        // search rather than quitting — only Ctrl+C stays a hard override.
        if self.focus == Focus::Browser && self.browser.is_searching() {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.should_quit = true;
                return;
            }
            if let Outcome::Picked(path) = self.browser.on_key(key) {
                self.play_path(path);
            }
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => self.should_quit = true,
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,

            KeyCode::Char(' ') => self.engine.lock().unwrap().toggle_pause(),
            KeyCode::Left => self.engine.lock().unwrap().seek_relative(-5.0),
            KeyCode::Right => self.engine.lock().unwrap().seek_relative(5.0),
            KeyCode::Char(',') => self.engine.lock().unwrap().seek_relative(-30.0),
            KeyCode::Char('.') => self.engine.lock().unwrap().seek_relative(30.0),
            KeyCode::Char('r') | KeyCode::Home => self.engine.lock().unwrap().seek(0.0),

            KeyCode::Char('[') => self.engine.lock().unwrap().nudge_speed(-0.05),
            KeyCode::Char(']') => self.engine.lock().unwrap().nudge_speed(0.05),
            KeyCode::Char('=') | KeyCode::Char('0') => self.engine.lock().unwrap().set_speed(1.0),

            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('o') => self.focus = self.focus.toggled(),
            KeyCode::Char('a') => self.open_soundfont_browser(),
            KeyCode::Char('v') => {
                self.visualizer.toggle();
                self.status = if self.visualizer.is_open() {
                    "Visualizer opened — press v again or close its window to stop.".to_string()
                } else {
                    "Visualizer closed.".to_string()
                };
            }

            _ => match self.focus {
                Focus::Browser => match self.browser.on_key(key) {
                    Outcome::Picked(path) => self.play_path(path),
                    Outcome::Pending | Outcome::Cancelled => {}
                },
                Focus::SoundFonts => self.on_sf_key(key),
            },
        }
    }

    fn on_sf_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.move_sf(-1),
            KeyCode::Down => self.move_sf(1),
            KeyCode::PageUp => self.move_sf(-10),
            KeyCode::PageDown => self.move_sf(10),
            KeyCode::Enter => {
                if let Some(idx) = self.sf_state.selected() {
                    self.activate_soundfont(idx);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => self.remove_selected_soundfont(),
            _ => {}
        }
    }

    fn on_sf_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = &mut self.sf_modal else {
            return;
        };
        match modal.on_key(key) {
            Outcome::Pending => {}
            Outcome::Cancelled => self.sf_modal = None,
            Outcome::Picked(path) => {
                self.sf_modal = None;
                self.add_soundfont(path);
            }
        }
    }

    // --- rendering --------------------------------------------------

    fn draw(&mut self, frame: &mut Frame) {
        let view = self.engine.lock().unwrap().view();
        let area = frame.area();

        let rows = Layout::vertical([
            Constraint::Length(1), // title
            Constraint::Length(2), // now playing
            Constraint::Length(3), // progress gauge
            Constraint::Min(4),    // browser + soundfonts
            Constraint::Length(2), // status
            Constraint::Length(2), // help
        ])
        .split(area);

        frame.render_widget(
            Paragraph::new(Line::from("synththing — MIDI player").bold()),
            rows[0],
        );

        let midi = view
            .midi_name
            .clone()
            .unwrap_or_else(|| "(no song loaded)".to_string());
        let soundfont = view
            .soundfont_name
            .clone()
            .unwrap_or_else(|| "(no soundfont)".to_string());
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![Span::styled("Song:      ", dim()), Span::raw(midi)]),
                Line::from(vec![Span::styled("SoundFont: ", dim()), Span::raw(soundfont)]),
            ]),
            rows[1],
        );

        let ratio = if view.length > 0.0 {
            (view.position / view.length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let state = if !view.has_midi {
            "no song"
        } else if !view.has_soundfont {
            "no soundfont"
        } else if view.finished {
            "done"
        } else if view.paused {
            "paused"
        } else {
            "playing"
        };
        let label = format!(
            "{} / {}   x{:.2}   {}",
            fmt_time(view.position),
            fmt_time(view.length),
            view.speed,
            state
        );
        frame.render_widget(
            Gauge::default()
                .block(Block::bordered().title(" progress "))
                .ratio(ratio)
                .label(label),
            rows[2],
        );

        let columns =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(rows[3]);
        self.browser
            .render_inline(frame, columns[0], self.focus == Focus::Browser);
        render_list(
            frame,
            columns[1],
            "soundfonts",
            list_items(&self.config.soundfonts, self.active_sf),
            &mut self.sf_state,
            self.focus == Focus::SoundFonts,
        );

        frame.render_widget(
            Paragraph::new(Line::from(self.status.clone()))
                .style(dim())
                .wrap(Wrap { trim: true }),
            rows[4],
        );

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(
                    "Space play/pause   ←/→ seek 5s   ,/. seek 30s   r restart   [ / ] speed   = reset speed",
                ),
                Line::from(
                    "Tab/o switch panel   ↑↓ move   ⏎ play / open folder   ⌫ up a dir   / search (Esc to end)   h hidden   a add soundfont   d remove   v visualizer   q quit",
                ),
            ])
            .style(dim())
            .wrap(Wrap { trim: true }),
            rows[5],
        );

        if let Some(modal) = &mut self.sf_modal {
            modal.render(frame, area);
        }
    }
}

fn list_items(paths: &[PathBuf], active: Option<usize>) -> Vec<ListItem<'static>> {
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let marker = if Some(i) == active { "● " } else { "  " };
            let mut label = format!("{marker}{}", nice_name(path));
            if !path.exists() {
                label.push_str("   (missing)");
            }
            ListItem::new(label)
        })
        .collect()
}

fn render_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: Vec<ListItem<'static>>,
    state: &mut ListState,
    focused: bool,
) {
    let (border_style, title_line, highlight_style, symbol) = if focused {
        (
            Style::new(),
            format!(" ▶ {title} "),
            Style::new().add_modifier(Modifier::REVERSED),
            "» ",
        )
    } else {
        (
            dim(),
            format!("   {title} "),
            dim().add_modifier(Modifier::BOLD),
            "  ",
        )
    };
    let list = List::new(items)
        .block(
            Block::bordered()
                .border_style(border_style)
                .title(title_line),
        )
        .highlight_style(highlight_style)
        .highlight_symbol(symbol);
    frame.render_stateful_widget(list, area, state);
}

/// After removing item `removed`, adjust an "active" index that pointed into the
/// same list.
fn shift_active(active: Option<usize>, removed: usize) -> Option<usize> {
    match active {
        Some(a) if a == removed => None,
        Some(a) if a > removed => Some(a - 1),
        other => other,
    }
}

/// Keep a list's selection in range after its length changed.
fn fix_selection(state: &mut ListState, len: usize, near: usize) {
    if len == 0 {
        state.select(None);
    } else {
        state.select(Some(near.min(len - 1)));
    }
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

enum SoundFontProbe {
    /// rustysynth parsed it and can render with it.
    Playable(Arc<SoundFont>),
    /// A real SoundFont container, but this build of rustysynth can't use it.
    Unplayable(String),
    /// Not a RIFF/sfbk file at all.
    NotSoundFont,
}

fn probe_soundfont(path: &Path) -> Result<SoundFontProbe> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"sfbk" {
        return Ok(SoundFontProbe::NotSoundFont);
    }
    match SoundFont::new(&mut bytes.as_slice()) {
        Ok(soundfont) => Ok(SoundFontProbe::Playable(Arc::new(soundfont))),
        Err(err) => Ok(SoundFontProbe::Unplayable(describe_soundfont_error(
            &err.to_string(),
            &bytes,
        ))),
    }
}

/// Turn a rustysynth parse failure into something actionable. The common case
/// for "valid" soundfonts that fail here is a compressed SF3 (Ogg/FLAC sample
/// data), which rustysynth does not support — FluidSynth does, which is why it
/// plays elsewhere.
fn describe_soundfont_error(base: &str, bytes: &[u8]) -> String {
    if contains_bytes(bytes, b"OggS") || contains_bytes(bytes, b"fLaC") {
        "it's a compressed SF3 soundfont. rustysynth only plays uncompressed SF2 — \
         convert it with Polyphone (File > Save as... > .sf2) and add that file."
            .to_string()
    } else {
        format!("rustysynth rejected it ({base}).")
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8; 4]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn load_midi(path: &Path) -> Result<Arc<MidiFile>> {
    let mut file = File::open(path)?;
    let midi = MidiFile::new(&mut file).map_err(|e| anyhow!("{e}"))?;
    Ok(Arc::new(midi))
}
