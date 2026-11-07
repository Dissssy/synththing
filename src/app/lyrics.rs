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
    reader: Latest<(), lyrics::Job, Option<Lyrics>>,
    /// The song asked for (with its lyrics at LRCLIB, if it was looked
    /// up there), and its lyrics once read (`None` inside while reading or
    /// when it has none: `read` says which).
    song: Option<(PathBuf, Option<i64>)>,
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
        let lrclib = self.current_song.as_deref().and_then(|p| self.songs.get(p)?.online.as_ref()?.lyrics);
        let song = self.current_song.clone().map(|path| (path, lrclib));
        let online = self.fetches_lyrics();
        let state = &mut self.lyrics;
        if state.song != song {
            state.song = song;
            state.read = false;
            state.lyrics = None;
            if let Some((path, lrclib)) = &state.song {
                state.reader.ask((), lyrics::Job { path: path.clone(), lrclib: *lrclib, online }, ctx);
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
        let size_error = lines.map(|n| f32::from(n) * line_height + BAR_ROOM - ui.available_height());
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
        // (Lyrics without times: where they are is a guess, shown as one.)
        let guessed = !lyrics.timed;
        let current = lyrics.current(position).or_else(|| lyrics.estimate(position, view.length - offset));
        if let Some(n) = lines {
            lines_ui(ui, rect, &lyrics, n, line_height, current, guessed, position, view.length - offset);
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
                    let font = egui::FontId::proportional(text_size(ui));
                    let color = match current {
                        Some(c) if c == i => ui.visuals().strong_text_color(),
                        Some(c) if c > i => ui.visuals().weak_text_color(),
                        _ => ui.visuals().text_color(),
                    };
                    // (The line being sung: its words sung so far, else all of it.)
                    let sung = if current == Some(i) && !guessed { line.sung(position).unwrap_or(line.text.len()) } else { 0 };
                    let mut job = sung_job(text, sung, font, playing, color);
                    job.wrap.max_width = ui.available_width();
                    job.halign = egui::Align::Center;
                    let label = egui::Label::new(job).wrap();
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
                    if current == Some(i) && !guessed {
                        let (room, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_ROOM), egui::Sense::hover());
                        progress_bar(ui, room.center_top() + egui::vec2(0.0, 1.0), lyrics.progress(i, position, view.length - offset));
                    }
                }
                ui.add_space(ui.clip_rect().height() / 2.0);
                ui.weak(lyrics.source.label());
                if !lyrics.timed {
                    ui.weak("These lyrics have no times: where the song is in them is a guess");
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

/// A line's text, its first `sung` bytes in `sung_color` and the rest in
/// `color`.
fn sung_job(text: &str, sung: usize, font: egui::FontId, sung_color: egui::Color32, color: egui::Color32) -> egui::text::LayoutJob {
    let sung = (0..=sung.min(text.len())).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    let mut job = egui::text::LayoutJob::default();
    for (part, color) in [(&text[..sung], sung_color), (&text[sung..], color)] {
        if !part.is_empty() {
            job.append(part, 0.0, egui::TextFormat::simple(font.clone(), color));
        }
    }
    job
}

/// The most lines line mode shows.
pub const MAX_LINES: u8 = 5;

/// The height of the progress bar under the line being sung, and the room
/// it takes.
const BAR: f32 = 3.0;
const BAR_ROOM: f32 = BAR + 2.0;

/// How far through the line being sung the song is (`done`, 0 to 1): a bar
/// centered under `top`.
fn progress_bar(ui: &egui::Ui, top: egui::Pos2, done: f32) {
    let width = (ui.max_rect().width() * 0.6).min(400.0);
    let track = egui::Rect::from_min_size(egui::pos2(top.x - width / 2.0, top.y), egui::vec2(width, BAR));
    ui.painter().rect_filled(track, 1.5, ui.visuals().faint_bg_color);
    ui.painter().rect_filled(egui::Rect::from_min_size(track.min, egui::vec2(width * done, BAR)), 1.5, crate::theme::playing_color(ui.ctx()));
}

fn text_size(ui: &egui::Ui) -> f32 {
    ui.style().text_styles[&egui::TextStyle::Body].size * 1.25
}

/// The room a line of lyrics takes in line mode.
fn line_height(ui: &egui::Ui) -> f32 {
    ui.fonts_mut(|f| f.row_height(&egui::FontId::proportional(text_size(ui)))) + 4.0
}

/// Line mode: `n` lines, the one being sung in the middle over a bar of
/// how far through it the song is, centered in `rect`.
#[allow(clippy::too_many_arguments)]
fn lines_ui(ui: &mut egui::Ui, rect: egui::Rect, lyrics: &Lyrics, n: u8, line_height: f32, current: Option<usize>, guessed: bool, position: f64, length: f64) {
    let n = usize::from(n);
    let playing = crate::theme::playing_color(ui.ctx());
    let font = egui::FontId::proportional(text_size(ui));
    let block = n as f32 * line_height + BAR_ROOM;
    let top = rect.center().y - block / 2.0;
    // (Before the first line, it's next, under where the sung one goes.)
    let middle = (n - 1) / 2;
    let first = current.map_or(-1, |c| c as isize) - middle as isize;
    let painter = ui.painter_at(rect);
    for row in 0..n {
        let index = first + row as isize;
        let line = usize::try_from(index).ok().and_then(|i| lyrics.lines.get(i));
        let weak = ui.visuals().weak_text_color();
        let (text, sung, color): (std::borrow::Cow<str>, _, _) = match line {
            // (A guess, marked as one: not lit up like a line being sung.)
            Some(line) if Some(index as usize) == current && guessed => (format!("≈ {}", line.text).into(), 0, ui.visuals().text_color()),
            Some(line) if Some(index as usize) == current => {
                (line.text.as_str().into(), line.sung(position).unwrap_or(line.text.len()), ui.visuals().strong_text_color())
            }
            Some(line) => (line.text.as_str().into(), 0, weak),
            None if row == middle => ("♪".into(), 0, weak),
            None => continue,
        };
        let mut job = sung_job(&text, sung, font.clone(), playing, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 16.0);
        let galley = painter.layout_job(job);
        // (The bar sits under the middle row, pushing the rows after down.)
        let below = if row > middle { BAR_ROOM } else { 0.0 };
        let center = egui::pos2(rect.center().x, top + (row as f32 + 0.5) * line_height + below);
        painter.galley(center - galley.size() / 2.0, galley, weak);
    }
    if let Some(i) = current.filter(|_| !guessed) {
        progress_bar(ui, egui::pos2(rect.center().x, top + (middle + 1) as f32 * line_height + 1.0), lyrics.progress(i, position, length));
    }
    ui.allocate_rect(rect, egui::Sense::hover());
}
