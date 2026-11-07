//! The Lyrics panel: the playing song's lyrics (`crate::lyrics`), the line
//! being sung marked and kept in view. Squeezed to a row or two tall, it's
//! just the line being sung and how far through it the song is.

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

/// Below this many rows of room, the panel shows only the line being sung.
const COMPACT_ROWS: f32 = 3.0;

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
}

impl Default for LyricsState {
    fn default() -> Self {
        Self { reader: Latest::new("lyrics", lyrics::read), song: None, read: false, lyrics: None, shown: None }
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

    pub(super) fn lyrics_ui(&mut self, ui: &mut egui::Ui, view: &EngineView) {
        let lyrics = self.playing_lyrics(ui.ctx());
        let compact = ui.available_height() < ui.spacing().interact_size.y * COMPACT_ROWS;
        let Some(lyrics) = lyrics else {
            let text = match (&self.lyrics.song, self.lyrics.read) {
                (None, _) => "Nothing playing\nPlay a song to see its lyrics here",
                (Some(_), false) => "",
                (Some(_), true) if compact => "No lyrics",
                (Some(_), true) => "No lyrics for this song\nPut an .lrc file of the same name beside it, or add lyrics to its tags",
            };
            placeholder(ui, text);
            return;
        };
        // (Until the song sent is the one playing, the view's another's.)
        let offset = self.playing_facts().and_then(|f| f.number(Field::LyricsOffset)).unwrap_or(0.0);
        let position = if view.loads < self.song_loads { 0.0 } else { view.position - offset };
        let current = lyrics.current(position);
        if compact {
            compact_ui(ui, &lyrics, current, position, view.length - offset);
            return;
        }
        let playing = crate::theme::playing_color(ui.ctx());
        let scroll_to = (current != self.lyrics.shown).then_some(current).flatten();
        self.lyrics.shown = current;
        let mut seek = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height().min(ui.clip_rect().height()) / 3.0);
                for (i, line) in lyrics.lines.iter().enumerate() {
                    let text = if line.text.is_empty() { " " } else { &line.text };
                    let mut rich = egui::RichText::new(text).size(ui.style().text_styles[&egui::TextStyle::Body].size * 1.25);
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
    }
}

/// The line being sung, centered, over a bar of how far through it the
/// song is.
fn compact_ui(ui: &mut egui::Ui, lyrics: &Lyrics, current: Option<usize>, position: f64, length: f64) {
    let line = match current {
        Some(i) => lyrics.lines[i].text.as_str(),
        None if lyrics.timed => "♪",
        None => "These lyrics aren't timed",
    };
    let rect = ui.available_rect_before_wrap();
    let bar = 2.0;
    let text_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - bar - 2.0));
    let mut job = egui::text::LayoutJob::simple_singleline(line.to_string(), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().strong_text_color());
    job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width() - 8.0);
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(text_rect.center() - galley.size() / 2.0, galley, ui.visuals().strong_text_color());
    if let Some(i) = current {
        let done = lyrics.progress(i, position, length);
        let track = egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - bar), rect.max);
        ui.painter().rect_filled(track, 1.0, ui.visuals().faint_bg_color);
        let filled = egui::Rect::from_min_max(track.min, egui::pos2(track.min.x + track.width() * done, track.max.y));
        ui.painter().rect_filled(filled, 1.0, crate::theme::playing_color(ui.ctx()));
    }
    ui.allocate_rect(rect, egui::Sense::hover());
}
