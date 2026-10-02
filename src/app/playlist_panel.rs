//! The Playlists section and everything playlist-driven on the `App` side:
//! advancing tracks, Next/Previous, the loop/shuffle interplay with the
//! engine, drag-and-drop into and within a playlist, and files dropped onto
//! the window from the OS. The data model and the pure next/previous logic
//! are in `crate::playlist`.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{is_midi_path, song_info_widgets, App, DraggedSoundfont, PREVIOUS_RESTARTS_AFTER_SECS, SONG_EXTENSIONS};
use crate::audio::AudioCommand;
use crate::config::nice_name;
use crate::engine::EngineView;
use crate::filebrowser::DraggedFile;
use crate::layout::Section;
use crate::playlist::{self, LoopMode, NowPlaying, PlaylistEntry};
use crate::song_info::format_length;

/// Drag-and-drop payload for playlist rows being dragged to reorder them:
/// the selection if the dragged row is part of it, else just that row.
pub(super) struct DraggedEntry {
    pub list: usize,
    pub indices: Vec<usize>,
    /// The row grabbed, for the label following the pointer.
    pub path: PathBuf,
}

/// Something dropped onto a playlist row this frame.
enum RowDrop {
    /// A song (or a folder of songs) from the browser, to insert at this
    /// position.
    Song(PathBuf, usize),
    /// Rows of the same playlist, moved to just before this position.
    Move(Vec<usize>, usize),
    /// A soundfont, to become the override of this row (and the rest of
    /// the selection, if this row is selected).
    Soundfont(PathBuf, usize),
}

/// A click on a row or one of its controls, or a right-click menu choice.
/// The `Vec` ones apply to the selection when used on a selected row.
enum RowAction {
    Play(usize),
    Select(usize),
    Remove(Vec<usize>),
    ClearSoundfont(Vec<usize>),
    UseSelectedSoundfont(Vec<usize>),
    RevealInSongs(usize),
}

impl App {
    // --- playback ----------------------------------------------------------

    /// Per-frame playlist upkeep: start the next track when the current one
    /// finishes, and keep the engine's own looping in step with the loop
    /// mode.
    pub(super) fn playlist_tick(&mut self, view: &EngineView) {
        if self.now_playing.is_some() {
            if !view.finished {
                self.advance_armed = true;
            } else if self.advance_armed {
                self.advance_armed = false;
                self.next_track();
            }
        }
        self.sync_engine_loop();
    }

    /// The engine loops the current track gaplessly by itself; that's what
    /// "Loop: One" wants, and "Loop: All" too when there's no playlist to
    /// wrap around. Looping a *playlist* is the app's job instead (see
    /// `next_track`), so the engine must not loop the track then.
    fn sync_engine_loop(&mut self) {
        let wanted = match self.loop_mode {
            LoopMode::Off => false,
            LoopMode::One => true,
            LoopMode::All => self.now_playing.is_none(),
        };
        if wanted != self.engine_loop {
            self.engine_loop = wanted;
            self.send(AudioCommand::SetLoopEnabled(wanted));
        }
    }

    /// Start entry `entry` of playlist `list` as a fresh run through it
    /// (shuffle history restarts from here).
    pub(super) fn play_entry(&mut self, list: usize, entry: usize) {
        self.start_now_playing(NowPlaying::new(list, entry), false);
    }

    /// Make `np` the current playlist position and start loading its song
    /// (it plays once loaded; see `loading.rs`). Also decides what comes
    /// after it, so that can preload meanwhile.
    fn start_now_playing(&mut self, np: NowPlaying, skip_on_failure: bool) {
        let Some(entry) = self
            .playlists
            .lists
            .get(np.list)
            .and_then(|l| l.playlist.entries.get(np.entry))
            .cloned()
        else {
            return;
        };
        self.now_playing = Some(np.clone());
        self.plan_upcoming();
        self.request_play(&entry.path, entry.soundfont.as_deref(), Some(np), skip_on_failure);
    }

    fn pick_next(&mut self, np: &NowPlaying) -> Option<usize> {
        let len = self.playlists.lists.get(np.list).map_or(0, |l| l.playlist.entries.len());
        let random = self.rng.next();
        playlist::next_entry(len, np.entry, self.loop_mode, self.shuffle, &np.history, random)
    }

    /// Decide now which entry follows the current one (randomly, if
    /// shuffling), so it can be preloaded and Next/auto-advance go exactly
    /// there. Re-decided whenever the playlist or loop/shuffle mode changes.
    pub(super) fn plan_upcoming(&mut self) {
        let Some(np) = self.now_playing.clone() else {
            return;
        };
        let upcoming = self.pick_next(&np);
        if let Some(np) = &mut self.now_playing {
            np.upcoming = upcoming;
        }
    }

    /// Advance to the next playlist entry (the one already planned, see
    /// `plan_upcoming`). One that fails to load is skipped, at most one pass
    /// through the list (see `loading.rs`).
    pub(super) fn next_track(&mut self) {
        let Some(mut np) = self.now_playing.clone() else {
            return;
        };
        let len = self.playlists.lists.get(np.list).map_or(0, |l| l.playlist.entries.len());
        let next = match np.upcoming.filter(|&u| u < len) {
            Some(next) => Some(next),
            None => self.pick_next(&np),
        };
        let Some(next) = next else {
            // Ran off the end: stay on the last track, finished, rather than
            // leaving the playlist.
            self.status = "End of playlist.".to_string();
            return;
        };
        if self.shuffle {
            if np.history.len() >= len {
                // Every entry's had its turn: this pick starts a new pass.
                np.history.clear();
            }
            np.history.push(next);
        } else {
            // History only matters for shuffle; don't let it grow forever
            // on a looping playlist.
            np.history = vec![next];
        }
        np.entry = next;
        np.upcoming = None;
        self.start_now_playing(np, true);
    }

    /// "Previous": restart the track if it's been playing a little while
    /// (or there's no playlist / nothing before it), else go back one.
    pub(super) fn previous_track(&mut self, position: f64) {
        let restart = position > PREVIOUS_RESTARTS_AFTER_SECS;
        let target = self.now_playing.as_ref().filter(|_| !restart).and_then(|np| {
            let len = self.playlists.lists.get(np.list)?.playlist.entries.len();
            playlist::previous_entry(len, np.entry, self.loop_mode, self.shuffle, &np.history)
        });
        match (target, self.now_playing.clone()) {
            (Some(prev), Some(mut np)) => {
                if self.shuffle {
                    np.history.pop();
                } else {
                    np.history = vec![prev];
                }
                np.entry = prev;
                np.upcoming = None;
                self.start_now_playing(np, false);
            }
            _ => self.send(AudioCommand::Seek(0.0)),
        }
    }

    // --- editing -----------------------------------------------------------

    fn save_playlist(&mut self, list: usize) {
        if let Err(e) = self.playlists.save(list) {
            self.status = format!("Couldn't save playlist: {e}");
        }
    }

    fn create_playlist(&mut self, name: &str, songs: Vec<PathBuf>) {
        match self.playlists.create(name, songs) {
            Ok(idx) => {
                self.viewed_playlist = Some(idx);
                self.playlist_rename = None;
                self.confirm_delete_playlist = false;
                self.playlist_selection.clear();
                self.selection_anchor = None;
            }
            Err(e) => self.status = format!("Couldn't create playlist: {e}"),
        }
    }

    fn delete_playlist(&mut self, list: usize) {
        let name = self.playlists.lists.get(list).map(|l| l.playlist.name.clone()).unwrap_or_default();
        if let Err(e) = self.playlists.delete(list) {
            self.status = format!("Couldn't delete playlist: {e}");
            return;
        }
        self.status = format!("Deleted playlist: {name}");
        self.playlist_selection.clear();
        self.selection_anchor = None;
        self.now_playing = self.now_playing.take().and_then(|mut np| {
            np.list = playlist::index_after_remove(np.list, list)?;
            Some(np)
        });
        self.viewed_playlist = self
            .viewed_playlist
            .and_then(|v| playlist::index_after_remove(v, list))
            .or_else(|| (!self.playlists.lists.is_empty()).then_some(0));
    }

    /// Insert `songs` at `at`; a folder among them adds the songs directly
    /// in it.
    fn insert_songs(&mut self, list: usize, at: usize, songs: Vec<PathBuf>) {
        let songs: Vec<PathBuf> = songs
            .into_iter()
            .flat_map(|p| if p.is_dir() { playlist::songs_in_folder(&p, SONG_EXTENSIONS) } else { vec![p] })
            .collect();
        if songs.is_empty() {
            self.status = "No songs directly in that folder.".to_string();
            return;
        }
        self.playlist_selection.clear();
        let Some(stored) = self.playlists.lists.get_mut(list) else {
            return;
        };
        let at = at.min(stored.playlist.entries.len());
        let count = songs.len();
        stored
            .playlist
            .entries
            .splice(at..at, songs.into_iter().map(PlaylistEntry::new));
        if let Some(np) = &mut self.now_playing
            && np.list == list
        {
            let shift = |i: usize| if i >= at { i + count } else { i };
            np.entry = shift(np.entry);
            np.history.iter_mut().for_each(|h| *h = shift(*h));
        }
        self.plan_upcoming();
        self.save_playlist(list);
    }

    fn remove_entry(&mut self, list: usize, idx: usize) {
        let Some(stored) = self.playlists.lists.get_mut(list) else {
            return;
        };
        if idx >= stored.playlist.entries.len() {
            return;
        }
        stored.playlist.entries.remove(idx);
        if let Some(np) = &mut self.now_playing
            && np.list == list
        {
            match playlist::index_after_remove(np.entry, idx) {
                // The playing track was removed: it keeps playing, just no
                // longer as part of the playlist.
                None => self.now_playing = None,
                Some(entry) => {
                    np.entry = entry;
                    np.history = np
                        .history
                        .iter()
                        .filter_map(|&h| playlist::index_after_remove(h, idx))
                        .collect();
                }
            }
        }
        self.plan_upcoming();
        self.save_playlist(list);
    }

    /// Move the entries at `indices` (keeping their order) to sit together
    /// just before position `before` of the current list.
    fn move_entries(&mut self, list: usize, indices: &[usize], before: usize) {
        let Some(stored) = self.playlists.lists.get_mut(list) else {
            return;
        };
        let order = playlist::order_after_move(stored.playlist.entries.len(), indices, before);
        if order.iter().enumerate().all(|(new, &old)| new == old) {
            return;
        }
        let positions = playlist::new_positions(&order);
        let mut old: Vec<Option<PlaylistEntry>> =
            std::mem::take(&mut stored.playlist.entries).into_iter().map(Some).collect();
        stored.playlist.entries = order.iter().filter_map(|&i| old[i].take()).collect();
        if let Some(np) = &mut self.now_playing
            && np.list == list
        {
            np.entry = positions[np.entry];
            np.history.iter_mut().for_each(|h| *h = positions.get(*h).copied().unwrap_or(*h));
        }
        if self.viewed_playlist == Some(list) {
            let mut selection: Vec<usize> = self.playlist_selection.iter().filter_map(|&i| positions.get(i).copied()).collect();
            selection.sort_unstable();
            self.playlist_selection = selection;
            self.selection_anchor = self.selection_anchor.and_then(|i| positions.get(i).copied());
        }
        self.plan_upcoming();
        self.save_playlist(list);
    }

    /// Remove several entries (highest first, so indices stay valid).
    fn remove_entries(&mut self, list: usize, indices: &[usize]) {
        let mut indices = indices.to_vec();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        for idx in indices {
            self.remove_entry(list, idx);
        }
        self.playlist_selection.clear();
        self.selection_anchor = None;
    }

    /// Set (or clear) the soundfont override of several entries; plain
    /// audio ones are skipped when setting.
    fn set_entries_soundfont(&mut self, list: usize, indices: &[usize], soundfont: Option<PathBuf>) {
        let midi: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| {
                soundfont.is_none()
                    || self.playlists.lists.get(list).and_then(|l| l.playlist.entries.get(i)).is_some_and(|e| is_midi_path(&e.path))
            })
            .collect();
        if midi.is_empty() && soundfont.is_some() {
            self.status = "Soundfont overrides only apply to MIDI files.".to_string();
            return;
        }
        for idx in midi {
            self.set_entry_soundfont(list, idx, soundfont.clone());
        }
    }

    /// Update the selection for a click on row `idx`, by the modifier keys
    /// held: plain selects just it, Ctrl toggles it, Shift extends a range
    /// from the last plain/Ctrl click.
    fn click_select(&mut self, idx: usize, modifiers: egui::Modifiers) {
        if modifiers.shift
            && let Some(anchor) = self.selection_anchor
        {
            let (lo, hi) = (anchor.min(idx), anchor.max(idx));
            if !modifiers.command {
                self.playlist_selection.clear();
            }
            self.playlist_selection.extend(lo..=hi);
            self.playlist_selection.sort_unstable();
            self.playlist_selection.dedup();
            return;
        }
        if modifiers.command {
            match self.playlist_selection.binary_search(&idx) {
                Ok(pos) => {
                    self.playlist_selection.remove(pos);
                }
                Err(pos) => self.playlist_selection.insert(pos, idx),
            }
        } else {
            self.playlist_selection = vec![idx];
        }
        self.selection_anchor = Some(idx);
    }

    /// The rows an action on row `idx` should apply to: the whole selection
    /// if `idx` is part of it, otherwise just `idx`.
    fn rows_for(&self, idx: usize) -> Vec<usize> {
        if self.playlist_selection.contains(&idx) { self.playlist_selection.clone() } else { vec![idx] }
    }

    fn set_entry_soundfont(&mut self, list: usize, idx: usize, soundfont: Option<PathBuf>) {
        let Some(entry) = self
            .playlists
            .lists
            .get_mut(list)
            .and_then(|l| l.playlist.entries.get_mut(idx))
        else {
            return;
        };
        if soundfont.is_some() && !is_midi_path(&entry.path) {
            self.status = "Soundfont overrides only apply to MIDI files.".to_string();
            return;
        }
        entry.soundfont = soundfont.clone();
        self.save_playlist(list);

        // Changing the override of the entry that's playing right now
        // swaps its soundfont live, same as picking one in the list does.
        let playing_this = self.now_playing.as_ref().is_some_and(|np| np.list == list && np.entry == idx)
            && self.pending_play.is_none();
        if playing_this {
            self.refresh_playing_soundfont(soundfont.as_deref());
        }
    }

    /// Files dragged onto the window from the OS file manager: soundfonts
    /// join the Soundfonts list; songs go into the playlist on screen (or
    /// the first one just plays, when no playlist is showing).
    pub(super) fn handle_os_file_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let has_ext = |p: &Path, exts: &[&str]| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        };
        let mut songs = Vec::new();
        for path in dropped {
            if has_ext(&path, &["sf2", "sf3"]) {
                self.add_soundfont(path);
            } else if path.is_dir() {
                songs.extend(playlist::songs_in_folder(&path, SONG_EXTENSIONS));
            } else if has_ext(&path, SONG_EXTENSIONS) {
                songs.push(path);
            }
        }
        if songs.is_empty() {
            return;
        }
        match self.viewed_playlist.filter(|_| self.is_open(Section::Playlists)) {
            Some(list) => {
                let at = self.playlists.lists.get(list).map_or(0, |l| l.playlist.entries.len());
                let count = songs.len();
                self.insert_songs(list, at, songs);
                self.status = format!("Added {count} song(s) to the playlist.");
            }
            None => self.play_path(songs.swap_remove(0)),
        }
    }

    // --- UI ----------------------------------------------------------------

    pub(super) fn playlist_panel_ui(&mut self, ui: &mut egui::Ui) {
        let panel_rect = ui.max_rect();

        ui.horizontal(|ui| {
            ui.menu_button("New playlist", |ui| {
                if ui.button("Blank").clicked() {
                    let name = self.unique_playlist_name("New playlist");
                    self.create_playlist(&name, Vec::new());
                    self.playlist_rename = Some(name);
                }
                let folder = self.browser.cwd().to_path_buf();
                let folder_name = folder
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Folder".to_string());
                if ui
                    .button(format!("From current folder ({folder_name})"))
                    .on_hover_text("Every song directly in the folder the Songs browser is showing")
                    .clicked()
                {
                    let songs = playlist::songs_in_folder(&folder, SONG_EXTENSIONS);
                    if songs.is_empty() {
                        self.status = format!("No songs directly in {folder_name}.");
                    } else {
                        let name = self.unique_playlist_name(&folder_name);
                        self.create_playlist(&name, songs);
                    }
                }
                if ui.button("Import .m3u...").clicked() {
                    self.import_m3u();
                }
            });
        });

        if self.playlists.lists.is_empty() {
            ui.weak("No playlists yet. Make one with New, or drag a song from Songs into this panel.");
        } else {
            self.playlist_header_ui(ui);
        }

        let mut row_drop = None;
        let mut action = None;
        if let Some(list) = self.viewed_playlist.filter(|&l| l < self.playlists.lists.len()) {
            (row_drop, action) = self.playlist_rows_ui(ui, list);
        }

        // Rows take a drop first; anything released elsewhere over the
        // panel appends (creating a playlist if there's none to append to).
        let released_here =
            ui.rect_contains_pointer(panel_rect) && ui.input(|i| i.pointer.any_released());
        if row_drop.is_none() && released_here {
            if let Some(file) = egui::DragAndDrop::take_payload::<DraggedFile>(ui.ctx()) {
                let list = match self.viewed_playlist.filter(|&l| l < self.playlists.lists.len()) {
                    Some(list) => list,
                    None => {
                        let name = self.unique_playlist_name("New playlist");
                        self.create_playlist(&name, Vec::new());
                        self.viewed_playlist.unwrap_or(0)
                    }
                };
                let at = self.playlists.lists.get(list).map_or(0, |l| l.playlist.entries.len());
                self.insert_songs(list, at, vec![file.0.clone()]);
            } else if egui::DragAndDrop::take_payload::<DraggedSoundfont>(ui.ctx()).is_some() {
                self.status = "Drop a soundfont onto a MIDI entry to override its soundfont.".to_string();
            }
        }

        let Some(list) = self.viewed_playlist else {
            return;
        };

        // Keys, while the pointer is over the list and nothing's being
        // typed: Ctrl+A selects all, Delete removes the selection.
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if !typing && ui.rect_contains_pointer(panel_rect) {
            let (select_all, delete) = ui.input(|i| {
                (i.modifiers.command && i.key_pressed(egui::Key::A), i.key_pressed(egui::Key::Delete))
            });
            if select_all {
                let len = self.playlists.lists.get(list).map_or(0, |l| l.playlist.entries.len());
                self.playlist_selection = (0..len).collect();
            }
            if delete && !self.playlist_selection.is_empty() {
                let selection = self.playlist_selection.clone();
                self.remove_entries(list, &selection);
            }
        }

        match row_drop {
            Some(RowDrop::Song(path, at)) => self.insert_songs(list, at, vec![path]),
            Some(RowDrop::Move(indices, before)) => self.move_entries(list, &indices, before),
            Some(RowDrop::Soundfont(sf, idx)) => {
                let rows = self.rows_for(idx);
                self.set_entries_soundfont(list, &rows, Some(sf));
            }
            None => {}
        }
        match action {
            Some(RowAction::Play(idx)) => self.play_entry(list, idx),
            Some(RowAction::Select(idx)) => {
                let modifiers = ui.input(|i| i.modifiers);
                self.click_select(idx, modifiers);
            }
            Some(RowAction::Remove(rows)) => self.remove_entries(list, &rows),
            Some(RowAction::ClearSoundfont(rows)) => self.set_entries_soundfont(list, &rows, None),
            Some(RowAction::UseSelectedSoundfont(rows)) => {
                let selected = self.active_sf.and_then(|i| self.config.soundfonts.get(i)).cloned();
                match selected {
                    Some(sf) => self.set_entries_soundfont(list, &rows, Some(sf)),
                    None => self.status = "No soundfont selected.".to_string(),
                }
            }
            Some(RowAction::RevealInSongs(idx)) => {
                let dir = self.playlists.lists[list].playlist.entries[idx].path.parent().map(Path::to_path_buf);
                if let Some(dir) = dir {
                    self.browser.navigate_to(dir);
                    self.set_open(Section::Songs, true);
                }
            }
            None => {}
        }
    }

    /// Playlist picker, rename/delete, and play-from-the-top.
    fn playlist_header_ui(&mut self, ui: &mut egui::Ui) {
        let viewed = self.viewed_playlist.filter(|&l| l < self.playlists.lists.len());
        let mut pick = None;
        ui.horizontal(|ui| {
            let current = viewed
                .map(|l| self.playlists.lists[l].playlist.name.clone())
                .unwrap_or_else(|| "(pick one)".to_string());
            egui::ComboBox::from_id_salt("playlist_picker")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, stored) in self.playlists.lists.iter().enumerate() {
                        let playing = self.now_playing.as_ref().is_some_and(|np| np.list == i);
                        let label = if playing {
                            format!("{} (playing)", stored.playlist.name)
                        } else {
                            stored.playlist.name.clone()
                        };
                        if ui.selectable_label(viewed == Some(i), label).clicked() {
                            pick = Some(i);
                        }
                    }
                });
        });
        if let Some(i) = pick {
            self.viewed_playlist = Some(i);
            self.playlist_rename = None;
            self.confirm_delete_playlist = false;
            self.playlist_selection.clear();
            self.selection_anchor = None;
        }

        let Some(list) = viewed else {
            return;
        };
        ui.horizontal(|ui| {
            let empty = self.playlists.lists[list].playlist.entries.is_empty();
            if ui.add_enabled(!empty, egui::Button::new("Play")).clicked() {
                let start = if self.shuffle {
                    (self.rng.next() % self.playlists.lists[list].playlist.entries.len() as u64) as usize
                } else {
                    0
                };
                self.play_entry(list, start);
            }

            if let Some(name) = &mut self.playlist_rename {
                let response = ui.add(egui::TextEdit::singleline(name).desired_width(140.0));
                let commit = response.lost_focus() || ui.button("OK").clicked();
                if commit {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        self.playlists.lists[list].playlist.name = name;
                        self.save_playlist(list);
                    }
                    self.playlist_rename = None;
                } else if !response.has_focus() && !response.lost_focus() {
                    response.request_focus();
                }
            } else if ui.button("Rename").clicked() {
                self.playlist_rename = Some(self.playlists.lists[list].playlist.name.clone());
            }

            if ui.button("Export .m3u...").on_hover_text("Save as an .m3u8 playlist other players can open").clicked() {
                self.export_m3u(list);
            }

            let delete_label = if self.confirm_delete_playlist { "Really delete?" } else { "Delete" };
            let response = ui.button(delete_label);
            if response.clicked() {
                if self.confirm_delete_playlist {
                    self.confirm_delete_playlist = false;
                    self.delete_playlist(list);
                } else {
                    self.confirm_delete_playlist = true;
                }
            } else if self.confirm_delete_playlist && ui.input(|i| i.pointer.any_click()) {
                // Any click elsewhere backs out of the confirmation.
                self.confirm_delete_playlist = false;
            }
        });
    }

    /// The entries of playlist `list`: click to play, drag to reorder, drop
    /// songs between rows or soundfonts onto MIDI rows, right-click for more.
    fn playlist_rows_ui(&mut self, ui: &mut egui::Ui, list: usize) -> (Option<RowDrop>, Option<RowAction>) {
        let mut row_drop = None;
        let mut action = None;
        let mut hovered = None;
        let entries = &self.playlists.lists[list].playlist.entries;
        let playing = self.now_playing.as_ref().filter(|np| np.list == list).map(|np| np.entry);
        let selected_sf = self.active_sf.and_then(|i| self.config.soundfonts.get(i)).is_some();
        let selection = &self.playlist_selection;
        let cache = &mut self.song_info;
        let info_open = &mut self.song_info_open;

        // Total time from what's been read so far.
        let (mut total, mut unknown) = (0.0, 0usize);
        for entry in entries {
            match cache.get(&entry.path).and_then(|i| i.length) {
                Some(length) => total += length,
                None => unknown += 1,
            }
        }
        let missing = entries.iter().filter(|e| e.missing).count();
        let mut summary = format!("{} song{}, {}", entries.len(), if entries.len() == 1 { "" } else { "s" }, format_length(total));
        if unknown > 0 {
            summary.push_str(&format!(" (+{unknown} of unknown length)"));
        }
        if missing > 0 {
            summary.push_str(&format!(", {missing} missing"));
        }
        if !selection.is_empty() {
            summary.push_str(&format!(", {} selected", selection.len()));
        }
        ui.weak(summary);
        ui.separator();

        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("playlist_rows")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, entries.len(), |ui, range| {
                for idx in range {
                    let entry = &entries[idx];
                    let is_selected = selection.binary_search(&idx).is_ok();
                    let rows = || if is_selected { selection.clone() } else { vec![idx] };
                    let row = ui.horizontal(|ui| {
                        // The playing entry is marked in the number column;
                        // the highlight is the selection.
                        if playing == Some(idx) {
                            ui.strong(" > ").on_hover_text("Playing");
                        } else {
                            ui.weak(format!("{:>3}", idx + 1));
                        }
                        let mut label = nice_name(&entry.path);
                        if entry.missing {
                            label.push_str("  (missing)");
                        }
                        let response = ui
                            .add(egui::Button::selectable(is_selected, label).sense(egui::Sense::click_and_drag()))
                            .on_hover_text(format!("{}\nDouble-click to play", entry.path.display()));
                        if response.hovered() {
                            hovered = Some((entry.path.clone(), entry.soundfont.clone()));
                        }
                        response.dnd_set_drag_payload(DraggedEntry { list, indices: rows(), path: entry.path.clone() });
                        if response.double_clicked() {
                            action = Some(RowAction::Play(idx));
                        } else if response.clicked() {
                            action = Some(RowAction::Select(idx));
                        }
                        response.context_menu(|ui| {
                            let rows = rows();
                            let count = if rows.len() > 1 { format!(" ({})", rows.len()) } else { String::new() };
                            if ui.button("Play").clicked() {
                                action = Some(RowAction::Play(idx));
                            }
                            if is_midi_path(&entry.path) || rows.len() > 1 {
                                if ui
                                    .add_enabled(selected_sf, egui::Button::new(format!("Use selected soundfont{count}")))
                                    .clicked()
                                {
                                    action = Some(RowAction::UseSelectedSoundfont(rows.clone()));
                                }
                                if ui.button(format!("Clear soundfont override{count}")).clicked() {
                                    action = Some(RowAction::ClearSoundfont(rows.clone()));
                                }
                            }
                            if ui.button("Show in Songs").clicked() {
                                action = Some(RowAction::RevealInSongs(idx));
                            }
                            if ui.button(format!("Remove from playlist{count}")).clicked() {
                                action = Some(RowAction::Remove(rows));
                            }
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("x").on_hover_text("Remove from playlist").clicked() {
                                action = Some(RowAction::Remove(vec![idx]));
                            }
                            song_info_widgets(ui, cache, &entry.path, info_open);
                            if let Some(sf) = &entry.soundfont
                                && ui
                                    .small_button(format!("SF: {}", nice_name(sf)))
                                    .on_hover_text(format!(
                                        "Plays with this soundfont instead of the selected one.\n{}\nClick to clear.",
                                        sf.display()
                                    ))
                                    .clicked()
                            {
                                action = Some(RowAction::ClearSoundfont(vec![idx]));
                            }
                        });
                    });

                    if let Some(drop) = row_drop_ui(ui, &row.response, list, idx, is_midi_path(&entry.path)) {
                        row_drop = Some(drop);
                    }
                }
            });

        self.hovered_playlist_entry = hovered;
        (row_drop, action)
    }

    /// New playlist from an .m3u/.m3u8 file (named after it).
    fn import_m3u(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("M3U playlist", &["m3u", "m3u8"])
            .set_directory(self.config.browse_start_dir())
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                self.status = format!("Couldn't read {}: {e}", path.display());
                return;
            }
        };
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let entries = playlist::from_m3u(&text, &base);
        if entries.is_empty() {
            self.status = format!("No songs found in {}.", nice_name(&path));
            return;
        }
        let name = self.unique_playlist_name(&nice_name(&path));
        let songs: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        self.create_playlist(&name, songs);
        if let Some(list) = self.viewed_playlist
            && let Some(stored) = self.playlists.lists.get_mut(list)
        {
            for (entry, imported) in stored.playlist.entries.iter_mut().zip(&entries) {
                entry.soundfont = imported.soundfont.clone();
            }
            self.save_playlist(list);
        }
        let missing = entries.iter().filter(|e| e.missing).count();
        self.status = match missing {
            0 => format!("Imported {} songs as \"{name}\".", entries.len()),
            n => format!("Imported {} songs as \"{name}\" ({n} not found).", entries.len()),
        };
    }

    /// Save playlist `list` as an .m3u8 file.
    fn export_m3u(&mut self, list: usize) {
        let Some(stored) = self.playlists.lists.get(list) else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("M3U playlist", &["m3u8", "m3u"])
            .set_file_name(format!("{}.m3u8", stored.playlist.name))
            .save_file()
        else {
            return;
        };
        let cache = &mut self.song_info;
        let text = playlist::to_m3u(&stored.playlist, |p| cache.get(p).and_then(|i| i.length));
        self.status = match std::fs::write(&path, text) {
            Ok(()) => format!("Exported to {}.", path.display()),
            Err(e) => format!("Couldn't write {}: {e}", path.display()),
        };
    }

    fn unique_playlist_name(&self, base: &str) -> String {
        let taken = |name: &str| self.playlists.lists.iter().any(|l| l.playlist.name == name);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base} {n}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
    }
}

/// Highlight a row while something droppable hovers it, an insertion line
/// above/below for songs and reordering, an outline for a soundfont onto a
/// MIDI row, and report the drop on release.
fn row_drop_ui(ui: &egui::Ui, row: &egui::Response, list: usize, idx: usize, is_midi: bool) -> Option<RowDrop> {
    if !row.contains_pointer() {
        return None;
    }
    let rect = row.rect;
    let pointer_y = ui.input(|i| i.pointer.hover_pos()).map_or(rect.center().y, |p| p.y);
    let insert_at = if pointer_y < rect.center().y { idx } else { idx + 1 };
    let line_y = if insert_at == idx { rect.top() } else { rect.bottom() };
    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);

    let ctx = ui.ctx();
    let released = ui.input(|i| i.pointer.any_released());
    if egui::DragAndDrop::has_payload_of_type::<DraggedFile>(ctx) {
        ui.painter().hline(rect.x_range(), line_y, stroke);
        if released && let Some(file) = egui::DragAndDrop::take_payload::<DraggedFile>(ctx) {
            return Some(RowDrop::Song(file.0.clone(), insert_at));
        }
    } else if let Some(entry) = egui::DragAndDrop::payload::<DraggedEntry>(ctx)
        && entry.list == list
    {
        ui.painter().hline(rect.x_range(), line_y, stroke);
        if released {
            egui::DragAndDrop::clear_payload(ctx);
            return Some(RowDrop::Move(entry.indices.clone(), insert_at));
        }
    } else if egui::DragAndDrop::has_payload_of_type::<DraggedSoundfont>(ctx) && is_midi {
        ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
        if released && let Some(sf) = egui::DragAndDrop::take_payload::<DraggedSoundfont>(ctx) {
            return Some(RowDrop::Soundfont(sf.0.clone(), idx));
        }
    }
    None
}
