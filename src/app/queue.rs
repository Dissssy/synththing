//! The play queue: songs to play after the one playing, before
//! whatever it was playing from (a playlist, an album, or the Songs list
//! with Autoplay) carries on. Play next puts songs at its front, Add to
//! queue at its end; a playlist's or album's songs go in in its order, or
//! shuffled while Shuffle is on. The queue lives as long as the app does.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{icon_button, toast, App};
use crate::{select_list, song_list};
use crate::playlist::{NowPlaying, Rng};

impl App {
    /// Queue `songs` (shuffled while Shuffle is on): next (at the front),
    /// or at the end.
    pub(super) fn enqueue(&mut self, songs: Vec<PathBuf>, next: bool) {
        let songs = if self.shuffle { shuffled(songs, |p| self.songs.together_of(p).map(|(id, at)| (id.to_string(), at)), &mut self.rng) } else { songs };
        if next {
            self.queue.splice(0..0, songs);
        } else {
            self.queue.extend(songs);
        }
    }

    /// Queue `songs`, as [`App::enqueue`], and say so.
    pub(super) fn queue_songs(&mut self, ctx: &egui::Context, songs: Vec<PathBuf>, next: bool) {
        if songs.is_empty() {
            return;
        }
        let count = songs.len();
        self.enqueue(songs, next);
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

    /// How many songs to pick ahead in what's playing: as many as the queue
    /// shows coming up, and at least the next (to preload it).
    pub(super) fn planned_count(&self) -> usize {
        self.config.coming_up.unwrap_or(crate::config::DEFAULT_COMING_UP).max(1)
    }

    /// The songs picked to play after the one playing, in what it's
    /// playing from (the list with Autoplay, an album, or a playlist), after
    /// the queue.
    fn planned(&self) -> Vec<PathBuf> {
        if let Some(playing) = &self.listed {
            return playing.upcoming.iter().map(|(at, _)| at.path.clone()).collect();
        }
        let Some(np) = &self.now_playing else { return Vec::new() };
        let entries = self.playlists.lists.get(np.list).map_or(&[][..], |l| &l.playlist.entries[..]);
        np.upcoming.iter().filter_map(|&i| entries.get(i)).map(|e| e.path.clone()).collect()
    }

    /// The Queue tab: the song playing, marked as playing, over what's
    /// coming up: the queue's songs, then (faded) what plays after them,
    /// filling out the queue to the Songs coming up preference. They're
    /// shown as the Songs list shows songs (its columns, without its search
    /// or sorting). A click plays a queued song now (taking it out);
    /// right-click to move queued songs to the top or take them out, and
    /// Delete takes the selected ones out. Songs drag in from Songs (or
    /// anywhere songs drag from), and queued rows drag to move.
    pub(super) fn queue_ui(&mut self, ui: &mut egui::Ui) {
        let queued = self.queue.len();
        let planned = self.planned();
        let after = self.config.coming_up.unwrap_or(crate::config::DEFAULT_COMING_UP).saturating_sub(queued).min(planned.len());
        let text = match (queued, after) {
            (1, 0) => "1 song".to_string(),
            (n, 0) => format!("{n} songs"),
            (n, m) => format!("{n} queued, {m} after"),
        };
        let mut clear = false;
        let mut view = self.config.songs_view.clone();
        let keys = song_list::column_keys(&view);
        let revision = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (&self.queue, &planned[..after], &self.current_song, self.songs_revision(), &keys).hash(&mut hasher);
            hasher.finish()
        };
        if self.queue_rows.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            let naming = super::naming(&self.config, &self.songs);
            let mut describe = |path: &PathBuf| (path.clone(), super::describe_song(&mut self.song_info, &naming, &self.songs, path, &keys));
            let playing = self.current_song.as_ref().map(&mut describe);
            let rows = self.queue.iter().chain(&planned[..after]).map(&mut describe).collect();
            self.queue_rows = Some((revision, QueueRows { playing, rows }));
            self.queue_selection.clear();
        }
        let Some((_, shown)) = &self.queue_rows else { return };
        let mut star = None;
        if let Some(playing) = &shown.playing {
            ui.label(egui::RichText::new("Now playing").weak());
            if song_list::playing_ui(ui, "queue", playing, &view) {
                star = Some((playing.0.clone(), !playing.1.plays.starred));
            }
            ui.add_space(ui.spacing().item_spacing.y);
        }
        select_list::toolbar(ui, |ui| { ui.label(egui::RichText::new("Up next").weak()); }, text, |ui| {
            clear = ui.add_enabled(queued > 0, icon_button("trash")).on_hover_text("Clear the queue").clicked();
        });
        if shown.rows.is_empty() {
            let hint = if self.mobile() {
                "Nothing queued.\nLong-press songs to play them next or add them to the queue."
            } else {
                "Nothing queued.\nRight-click songs to play them next or add them to the queue, or drag them here."
            };
            super::placeholder(ui, hint);
        }
        let rows = &shown.rows;
        let mut act = None;
        let cache = &mut self.song_info;
        let output = song_list::ordered_ui(
            ui,
            "queue",
            rows,
            queued,
            &mut self.queue_selection,
            &mut view,
            &mut |ui, path| super::song_info_widgets(ui, cache, path, false),
            &mut |ui, picked| {
                let paths = || picked.iter().filter_map(|&i| rows.get(i).map(|(path, _)| path.clone())).collect();
                if picked.iter().all(|&i| i < queued) {
                    if ui.button("Move to the top").clicked() {
                        act = Some(Act::Top(picked.to_vec()));
                    }
                    if ui.button("Take out of the queue").clicked() {
                        act = Some(Act::Out(picked.to_vec()));
                    }
                } else if let Some(next) = menu(ui) {
                    act = Some(Act::Queue(paths(), next));
                }
                if ui.button("Song info").clicked() {
                    act = Some(Act::Info(paths()));
                }
            },
        );
        if clear {
            self.queue.clear();
        }
        if output.view_changed {
            self.config.songs_view = view;
            if let Err(e) = self.config.save() {
                log::warn!("saving the config: {e}");
            }
        }
        if let Some(i) = output.play {
            act = Some(Act::Play(i));
        }
        if !output.delete.is_empty() {
            act = Some(Act::Out(output.delete));
        }
        if let Some((paths, at)) = output.insert {
            let at = at.min(self.queue.len());
            self.queue.splice(at..at, paths);
        }
        if let Some((rows, before)) = output.moved {
            let order = crate::playlist::order_after_move(self.queue.len(), &rows, before);
            let mut old: Vec<Option<PathBuf>> = std::mem::take(&mut self.queue).into_iter().map(Some).collect();
            self.queue = order.iter().filter_map(|&i| old[i].take()).collect();
        }
        if let Some((path, on)) = output.star.or(star) {
            self.tag_songs(super::tags::TagAction::Set(crate::song_store::STARRED.into(), on), &[path]);
        }
        match act {
            Some(Act::Play(i)) if i < self.queue.len() => {
                let path = self.queue.remove(i);
                self.request_queued(&path);
            }
            Some(Act::Top(rows)) => {
                let moved: Vec<PathBuf> = rows.iter().filter_map(|&i| self.queue.get(i).cloned()).collect();
                remove_rows(&mut self.queue, &rows);
                self.queue.splice(0..0, moved);
            }
            Some(Act::Out(rows)) => remove_rows(&mut self.queue, &rows),
            Some(Act::Queue(paths, next)) => self.queue_songs(ui.ctx(), paths, next),
            Some(Act::Info(paths)) => self.details_for = Some(paths),
            Some(Act::Play(_)) | None => {}
        }
    }
}

/// The Queue tab's rows as last described: the song playing, then the
/// queue's and what plays after them.
pub(super) struct QueueRows {
    playing: Option<(PathBuf, song_list::Described)>,
    rows: Vec<(PathBuf, song_list::Described)>,
}

/// What's asked of the Queue tab's rows (by their places in the queue).
enum Act {
    Play(usize),
    Top(Vec<usize>),
    Out(Vec<usize>),
    /// Songs coming up after the queue, put in it: next, or at its end.
    Queue(Vec<PathBuf>, bool),
    Info(Vec<PathBuf>),
}

/// Take the songs at `rows` out of `queue`.
fn remove_rows(queue: &mut Vec<PathBuf>, rows: &[usize]) {
    let mut i = 0;
    queue.retain(|_| {
        let keep = !rows.contains(&i);
        i += 1;
        keep
    });
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
