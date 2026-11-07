//! The Lyrics panel: the playing song's lyrics (`crate::lyrics`), the line
//! being sung marked and kept in view. In line mode (its right-click menu)
//! it's just a few lines around the one being sung, and in the dock exactly
//! that tall (`App::fit_lyrics`).

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use super::App;
use super::widgets::placeholder;
use crate::audio::AudioCommand;
use crate::engine::EngineView;
use crate::fields::Field;
use crate::latest::Latest;
use crate::lyrics::{self, Lyrics};

/// The playing song's lyrics, read on a thread of their own.
pub(super) struct LyricsState {
    reader: Latest<(), PathBuf, Option<Lyrics>>,
    /// The song asked for, and its lyrics once read (`None` inside while
    /// reading or when it has none: `read` says which).
    song: Option<PathBuf>,
    read: bool,
    lyrics: Option<Arc<Lyrics>>,
    /// The line last scrolled to.
    shown: Option<usize>,
    /// How much taller than its tab line mode wants the dock's Lyrics
    /// (`App::fit_lyrics`).
    pub(super) size_error: Option<f32>,
}

impl Default for LyricsState {
    fn default() -> Self {
        Self { reader: Latest::new("lyrics", lyrics::read), song: None, read: false, lyrics: None, shown: None, size_error: None }
    }
}

impl App {
    /// The playing song's lyrics, asked for when the song changes.
    fn playing_lyrics(&mut self, ctx: &egui::Context) -> Option<Arc<Lyrics>> {
        let state = &mut self.lyrics;
        for (_, lyrics) in state.reader.take() {
            state.read = true;
            state.lyrics = lyrics.map(Arc::new);
            state.shown = None;
        }
        if state.song != self.current_song {
            state.song = self.current_song.clone();
            state.read = false;
            state.lyrics = None;
            if let Some(path) = &state.song {
                state.reader.ask((), path.clone(), ctx);
            }
        }
        state.lyrics.clone()
    }

    /// The panel, filling `ui`. In line mode, how much taller than it is
    /// it wants to be (points; negative for shorter), for the dock to fit.
    pub(super) fn lyrics_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) -> Option<f32> {
        let lyrics = self.playing_lyrics(ui.ctx());
        let lines = self.config.lyrics_lines.map(|n| n.clamp(1, MAX_LINES));
        let line_height = line_height(ui);
        let size_error = lines.map(|n| f32::from(n) * line_height + if n == 1 { BAR + 2.0 } else { 0.0 } - ui.available_height());
        let rect = ui.available_rect_before_wrap();
        // (Line mode, and back, from the panel's right-click menu.)
        ui.interact(rect, ui.scope_id().with("lyrics_menu"), egui::Sense::click()).context_menu(|ui| self.lyrics_menu_ui(ui));
        let Some(lyrics) = lyrics else {
            let text = match (&self.lyrics.song, self.lyrics.read) {
                (None, _) if lines.is_some() => "Nothing playing",
                (None, _) => "Nothing playing\nPlay a song to see its lyrics here",
                (Some(_), false) => "",
                (Some(_), true) if lines.is_some() => "No lyrics",
                (Some(_), true) => "No lyrics for this song\nPut an .lrc file of the same name beside it, or add lyrics to its tags",
            };
            placeholder(ui, text);
            return size_error;
        };
        // (Until the song sent is the one playing, the view's another's.)
        let offset = self.playing_facts().and_then(|f| f.number(Field::LyricsOffset)).unwrap_or(0.0);
        let position = if view.loads < self.song_loads { 0.0 } else { view.position - offset };
        let current = lyrics.current(position);
        if let Some(n) = lines {
            lines_ui(ui, rect, &lyrics, n, line_height, current, position, view.length - offset);
            return size_error;
        }
        let playing = crate::theme::playing_color(ui.ctx());
        let scroll_to = (current != self.lyrics.shown).then_some(current).flatten();
        self.lyrics.shown = current;
        let mut seek = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.clip_rect().height() / 3.0);
                for (i, line) in lyrics.lines.iter().enumerate() {
                    let text = if line.text.is_empty() { " " } else { &line.text };
                    let mut rich = egui::RichText::new(text).size(text_size(ui));
                    rich = match current {
                        Some(c) if c == i => rich.strong().color(playing),
                        Some(c) if c > i => rich.weak(),
                        _ => rich,
                    };
                    let label = egui::Label::new(rich).wrap();
                    let response = match line.at {
                        Some(at) => {
                            let response = ui.add(label.sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
                            if response.clicked() {
                                seek = Some(at + offset);
                            }
                            response.on_hover_text(format!("Play from {}", crate::song_info::format_length(at + offset)))
                        }
                        None => ui.add(label),
                    };
                    if scroll_to == Some(i) {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                }
                ui.add_space(ui.clip_rect().height() / 2.0);
                ui.weak(lyrics.source.label());
                if !lyrics.timed {
                    ui.weak("These lyrics have no times, so they don't follow the song");
                }
            });
        });
        if let Some(at) = seek {
            self.send(AudioCommand::Seek(at.max(0.0)));
        }
        None
    }

    fn lyrics_menu_ui(&mut self, ui: &mut egui::Ui) {
        let mut lines = self.config.lyrics_lines;
        ui.radio_value(&mut lines, None, "All the lyrics").on_hover_text("Free size, scrolling with the song");
        for n in 1..=MAX_LINES {
            let label = if n == 1 { "1 line".to_string() } else { format!("{n} lines") };
            ui.radio_value(&mut lines, Some(n), label).on_hover_text("Exactly this tall, the line being sung in the middle");
        }
        if lines != self.config.lyrics_lines {
            self.config.lyrics_lines = lines;
            self.save_config();
            ui.close();
        }
    }
}

/// The most lines line mode shows.
pub const MAX_LINES: u8 = 5;

/// The height of line 1's progress bar.
const BAR: f32 = 3.0;

fn text_size(ui: &egui::Ui) -> f32 {
    ui.style().text_styles[&egui::TextStyle::Body].size * 1.25
}

/// The room a line of lyrics takes in line mode.
fn line_height(ui: &egui::Ui) -> f32 {
    ui.fonts_mut(|f| f.row_height(&egui::FontId::proportional(text_size(ui)))) + 4.0
}

/// Line mode: `n` lines, the one being sung in the middle (over a bar of
/// how far through it the song is, for one line), centered in `rect`.
#[allow(clippy::too_many_arguments)]
fn lines_ui(ui: &mut egui::Ui, rect: egui::Rect, lyrics: &Lyrics, n: u8, line_height: f32, current: Option<usize>, position: f64, length: f64) {
    let n = usize::from(n);
    let playing = crate::theme::playing_color(ui.ctx());
    let font = egui::FontId::proportional(text_size(ui));
    let block = n as f32 * line_height + if n == 1 { BAR + 2.0 } else { 0.0 };
    let top = rect.center().y - block / 2.0;
    // (Before the first line, it's next, under where the sung one goes.)
    let middle = (n - 1) / 2;
    let first = current.map_or(-1, |c| c as isize) - middle as isize;
    let painter = ui.painter_at(rect);
    for row in 0..n {
        let index = first + row as isize;
        let line = usize::try_from(index).ok().and_then(|i| lyrics.lines.get(i));
        let (text, color) = match line {
            Some(line) if Some(index as usize) == current => (line.text.as_str(), playing),
            Some(line) => (line.text.as_str(), ui.visuals().weak_text_color()),
            None if row == middle && lyrics.timed => ("♪", ui.visuals().weak_text_color()),
            None if row == middle => ("These lyrics aren't timed", ui.visuals().weak_text_color()),
            None => continue,
        };
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font.clone(), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 16.0);
        let galley = painter.layout_job(job);
        let center = egui::pos2(rect.center().x, top + (row as f32 + 0.5) * line_height);
        painter.galley(center - galley.size() / 2.0, galley, color);
    }
    if n == 1
        && let Some(i) = current
    {
        let done = lyrics.progress(i, position, length);
        let width = (rect.width() * 0.6).min(400.0);
        let track = egui::Rect::from_min_size(egui::pos2(rect.center().x - width / 2.0, top + line_height + 1.0), egui::vec2(width, BAR));
        painter.rect_filled(track, 1.5, ui.visuals().faint_bg_color);
        painter.rect_filled(egui::Rect::from_min_size(track.min, egui::vec2(width * done, BAR)), 1.5, playing);
    }
    ui.allocate_rect(rect, egui::Sense::hover());
}
