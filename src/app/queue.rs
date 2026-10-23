//! The play queue (Up next): songs to play after the one playing, before
//! whatever it was playing from (a playlist, an album, or the Songs list
//! with Autoplay) carries on. Play next puts songs at its front, Add to
//! queue at its end; a playlist's or album's songs go in in its order, or
//! shuffled while Shuffle is on. The queue lives as long as the app does.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{icon_button, toast, App};
use crate::playlist::{NowPlaying, Rng};

impl App {
    /// Queue `songs`: next (at the front), or at the end.
    pub(super) fn queue_songs(&mut self, ctx: &egui::Context, songs: Vec<PathBuf>, next: bool) {
        if songs.is_empty() {
            return;
        }
        let songs = if self.shuffle { shuffled(songs, |p| self.songs.together_of(p).map(|(id, at)| (id.to_string(), at)), &mut self.rng) } else { songs };
        let count = songs.len();
        if next {
            self.queue.splice(0..0, songs);
        } else {
            self.queue.extend(songs);
        }
        let what = if count == 1 { "1 song".to_string() } else { format!("{count} songs") };
        toast::show(ctx, if next { format!("Playing {what} next") } else { format!("Added {what} to the queue") });
    }

    /// Play the queue's first song, if there is one; whether there was.
    pub(super) fn play_queued(&mut self) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        let path = self.queue.remove(0);
        self.request_queued(&path);
        true
    }

    /// Previous, on a queued song: back to the song it was queued after
    /// (the one where its playlist or list is), the queued one going back
    /// to the front of the queue. Whether there was such a song.
    pub(super) fn back_from_queue(&mut self) -> bool {
        let Some(song) = self.current_song.clone().filter(|_| self.playing_queued) else { return false };
        if let Some(playing) = &self.listed {
            let path = playing.at.path.clone();
            self.queue.insert(0, song);
            self.request_play(&path, None, None, false);
        } else if let Some(np) = self.now_playing.clone() {
            self.queue.insert(0, song);
            self.start_now_playing(np, false);
        } else {
            return false;
        }
        true
    }

    /// The playlist position playing now: none while a queued song plays
    /// (its playlist carries on after the queue).
    pub(super) fn playing_entry(&self) -> Option<&NowPlaying> {
        self.now_playing.as_ref().filter(|_| !self.playing_queued)
    }

    /// The controls' Up next button: the queue in a popup, each song with
    /// a button to take it out, and Clear.
    pub(super) fn queue_button_ui(&mut self, ui: &mut egui::Ui) {
        let count = self.queue.len();
        let hover = match count {
            0 => "Up next: nothing queued (right-click songs to play them next or add them to the queue)".to_string(),
            1 => "Up next: 1 song".to_string(),
            n => format!("Up next: {n} songs"),
        };
        let button = ui.add(icon_button("list-numbers").selected(count > 0)).on_hover_text(hover);
        let mut out = None;
        let mut clear = false;
        egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(240.0);
            ui.horizontal(|ui| {
                ui.strong("Up next");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clear = ui.add_enabled(count > 0, egui::Button::new("Clear")).clicked();
                });
            });
            if count == 0 {
                ui.weak("Nothing queued. Right-click songs to play them next or add them to the queue.");
                return;
            }
            let queue = std::mem::take(&mut self.queue);
            egui::ScrollArea::vertical().max_height(320.0).show_rows(ui, ui.spacing().interact_size.y, queue.len(), |ui, range| {
                for i in range {
                    ui.horizontal(|ui| {
                        if ui.small_button(super::icon("x")).on_hover_text("Take it out").clicked() {
                            out = Some(i);
                        }
                        ui.add(egui::Label::new(self.song_title(&queue[i])).truncate());
                    });
                }
            });
            self.queue = queue;
        });
        if let Some(i) = out {
            self.queue.remove(i);
        }
        if clear {
            self.queue.clear();
        }
    }
}

/// Play next and Add to queue, in a song's (or several songs') menu:
/// `Some(true)` for next, `Some(false)` for the end.
pub(super) fn menu(ui: &mut egui::Ui) -> Option<bool> {
    if ui.button("Play next").clicked() {
        return Some(true);
    }
    ui.button("Add to queue").clicked().then_some(false)
}

/// `songs` in a random order, but the songs of one set that plays
/// together (`together`: its ID and where in it a song is) kept together,
/// in their order, where its first song is.
fn shuffled(songs: Vec<PathBuf>, together: impl Fn(&Path) -> Option<(String, usize)>, rng: &mut Rng) -> Vec<PathBuf> {
    let mut units: Vec<(Option<String>, Vec<(usize, PathBuf)>)> = Vec::new();
    for path in songs {
        match together(&path) {
            Some((id, at)) => match units.iter_mut().find(|(set, _)| set.as_deref() == Some(id.as_str())) {
                Some((_, unit)) => unit.push((at, path)),
                None => units.push((Some(id), vec![(at, path)])),
            },
            None => units.push((None, vec![(0, path)])),
        }
    }
    for i in (1..units.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        units.swap(i, j);
    }
    units
        .into_iter()
        .flat_map(|(_, mut unit)| {
            unit.sort_by_key(|(at, _)| *at);
            unit.into_iter().map(|(_, path)| path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffling_keeps_every_song_and_sets_together_in_order() {
        let songs: Vec<PathBuf> = ["a", "b", "set-2", "c", "set-1", "d"].iter().map(PathBuf::from).collect();
        let together = |p: &Path| {
            let name = p.to_str().unwrap();
            name.strip_prefix("set-").map(|at| ("set".to_string(), at.parse().unwrap()))
        };
        let mut rng = Rng::from_time();
        for _ in 0..20 {
            let out = shuffled(songs.clone(), together, &mut rng);
            let mut sorted = out.clone();
            sorted.sort();
            let mut expected = songs.clone();
            expected.sort();
            assert_eq!(sorted, expected);
            let first = out.iter().position(|p| p == Path::new("set-1")).unwrap();
            assert_eq!(out[first + 1], PathBuf::from("set-2"));
        }
    }
}
