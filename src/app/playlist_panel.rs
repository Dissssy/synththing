//! The Playlists section and everything playlist-driven on the `App` side:
//! advancing tracks, Next/Previous, the loop/shuffle interplay with the
//! engine, drag-and-drop into and within a playlist, and files dropped onto
//! the window from the OS. The data model and the pure next/previous logic
//! are in `crate::playlist`.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{
    is_midi_path, listed_song_extensions, song_info_widgets, App, DraggedSoundfont, PREVIOUS_RESTARTS_AFTER_SECS,
    SONG_EXTENSIONS,
};
use crate::audio::AudioCommand;
use crate::config::nice_name;
use crate::engine::EngineView;
use crate::song_list::DraggedFiles;
use crate::layout::Section;
use crate::playlist::{self, LoopMode, NowPlaying, PlaylistEntry};
use crate::select_list;
use crate::song_info::format_length;

/// Drag-and-drop payload for playlist rows being dragged to reorder them:
/// the selection if the dragged row is part of it, else just that row.
pub(super) struct DraggedEntry {
    pub list: usize,
    pub indices: Vec<usize>,
    /// The row grabbed's name, for the label following the pointer.
    pub label: String,
}

/// Something dropped onto a playlist row this frame.
enum RowDrop {
    /// Songs (or folders of songs) from the browser, to insert at this
    /// position.
    Songs(Vec<PathBuf>, usize),
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
    Remove(Vec<usize>),
    ClearSoundfont(Vec<usize>),
    UseCurrentSoundfont(Vec<usize>),
    Tag(super::tags::TagAction, Vec<PathBuf>),
}

impl App {
    // --- playback ----------------------------------------------------------

    /// Per-frame playlist upkeep: start the next track when the current one
    /// finishes, and keep the engine's own looping in step with the loop
    /// mode.
    pub(super) fn playlist_tick(&mut self, view: &EngineView) {
        // (A recording the script prepares moves on itself, at each take's end.)
        if self.listed.as_ref().is_some_and(|p| p.planned != self.song_list.generation()) {
            // The list changed: what comes next may have too.
            self.plan_listed();
        }
        if self.has_next() && !self.recording.holds_advance() {
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
            LoopMode::All => !self.has_next(),
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
        if let Some(stored) = self.playlists.lists.get(np.list) {
            self.playlist_positions.insert(stored.id.clone(), np.entry);
        }
        self.now_playing = Some(np.clone());
        self.listed = None;
        self.plan_upcoming();
        self.request_play(&entry.path, entry.soundfont.as_deref(), Some(np), skip_on_failure);
    }

    fn pick_next(&mut self, np: &NowPlaying) -> Option<usize> {
        let entries = self.playlists.lists.get(np.list).map_or(&[][..], |l| &l.playlist.entries[..]);
        let only_starred = self.config.only_starred;
        let playable: Vec<bool> = entries.iter().map(|e| !only_starred || self.songs.is_starred(&e.path)).collect();
        let random = self.rng.next();
        playlist::next_entry(&playable, np.entry, self.loop_mode, self.shuffle, &np.history, random)
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
        if self.listed.is_some() {
            self.listed_next();
            return;
        }
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
        if self.listed.is_some() {
            if restart || !self.listed_previous() {
                self.send(AudioCommand::Seek(0.0));
            }
            return;
        }
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

    pub(super) fn save_playlist(&mut self, list: usize) {
        if let Err(e) = self.playlists.save(list) {
            self.status = format!("Couldn't save playlist: {e}");
        }
        self.playlists_changed();
    }

    pub(super) fn create_playlist(&mut self, name: &str, songs: Vec<PathBuf>) {
        match self.playlists.create(name, songs) {
            Ok(idx) => {
                self.playlists_changed();
                self.viewed_playlist = Some(idx);
                self.playlist_rename = None;
                self.confirm_delete_playlist = false;
                self.playlist_selection.clear();
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
        self.playlists_changed();
        self.status = format!("Deleted playlist: {name}");
        self.playlist_selection.clear();
        self.now_playing = self.now_playing.take().and_then(|mut np| {
            np.list = playlist::index_after_remove(np.list, list)?;
            Some(np)
        });
        self.viewed_playlist = self.viewed_playlist.and_then(|v| playlist::index_after_remove(v, list));
    }

    /// Add `songs` to the end of playlist `list`, or a new playlist of
    /// them (`None`): the touch layout's "Add to playlist", which has no
    /// dragging.
    pub(super) fn add_to_playlist(&mut self, list: Option<usize>, songs: Vec<PathBuf>) {
        let count = songs.len();
        let name = match list {
            Some(list) => {
                let end = self.playlists.lists.get(list).map_or(0, |l| l.playlist.entries.len());
                self.insert_songs(list, end, songs);
                self.playlists.lists.get(list).map(|l| l.playlist.name.clone()).unwrap_or_default()
            }
            None => {
                let name = self.unique_playlist_name("New playlist");
                self.create_playlist(&name, songs);
                name
            }
        };
        self.status = format!("Added {count} song{} to {name}.", if count == 1 { "" } else { "s" });
    }

    /// Insert `songs` (the library's) at `at`.
    pub(super) fn insert_songs(&mut self, list: usize, at: usize, songs: Vec<PathBuf>) {
        if songs.is_empty() {
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
            self.playlist_selection.remap(&positions);
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
        let mut soundfonts = Vec::new();
        for path in dropped {
            if has_ext(&path, &["sf2", "sf3"]) {
                soundfonts.push(path);
            } else if path.is_dir() {
                songs.extend(crate::song_store::songs_under(&path, listed_song_extensions(&self.config)));
            } else if has_ext(&path, SONG_EXTENSIONS) {
                songs.push(path);
            }
        }
        if !soundfonts.is_empty() {
            self.add_soundfonts(soundfonts);
        }
        if songs.is_empty() {
            return;
        }
        // Into the library first, for the playlist; just played from
        // where it is, with none showing.
        let shown = self.groupings.shown_id() == super::groupings::PLAYLISTS && (self.mobile() || self.is_open(Section::Groupings));
        match self.viewed_playlist.filter(|_| shown) {
            Some(list) => {
                let at = self.playlists.lists.get(list).map_or(0, |l| l.playlist.entries.len());
                let files = songs.into_iter().map(|p| (p, None)).collect();
                crate::import::songs(files, SONG_EXTENSIONS, crate::import::Then::Insert { list, at });
            }
            None => self.play_path(songs.swap_remove(0)),
        }
    }

    /// Imported songs (`paths`, the library's, each with its playlist
    /// soundfont) are in: put them where they were headed.
    pub(super) fn after_import(&mut self, then: crate::import::Then, paths: Vec<PathBuf>, soundfonts: Vec<Option<PathBuf>>) {
        let list = match then {
            crate::import::Then::Keep => return,
            crate::import::Then::Insert { list, at } if list < self.playlists.lists.len() => {
                let count = paths.len();
                self.insert_songs(list, at, paths);
                self.status = format!("Added {count} song{} to the playlist.", if count == 1 { "" } else { "s" });
                return;
            }
            crate::import::Then::Insert { .. } => return,
            crate::import::Then::NewPlaylist { name } => {
                let name = self.unique_playlist_name(&name);
                self.create_playlist(&name, paths);
                self.status = format!("Imported the playlist \"{name}\".");
                self.viewed_playlist
            }
        };
        if let Some(list) = list
            && let Some(stored) = self.playlists.lists.get_mut(list)
        {
            for (entry, soundfont) in stored.playlist.entries.iter_mut().zip(soundfonts) {
                entry.soundfont = soundfont;
            }
            self.save_playlist(list);
        }
    }

    // --- UI ----------------------------------------------------------------

    /// Playlist `list`'s page (in Groupings): its header, then its
    /// entries.
    pub(super) fn playlist_panel_ui(&mut self, ui: &mut egui::Ui, list: usize) {
        let panel_rect = ui.max_rect();

        self.playlist_header_ui(ui, list);
        // (Back, or Delete.)
        if self.viewed_playlist != Some(list) {
            return;
        }
        let (row_drop, action) = self.playlist_rows_ui(ui, list);

        // Rows take a drop first; anything released elsewhere over the
        // panel appends.
        let released_here =
            ui.rect_contains_pointer(panel_rect) && ui.input(|i| i.pointer.any_released());
        if row_drop.is_none() && released_here {
            let smart = self.playlists.lists[list].playlist.smart.is_some();
            if smart && egui::DragAndDrop::take_payload::<DraggedFiles>(ui.ctx()).is_some() {
                self.status = "A smart playlist's rules pick its songs.".to_string();
            } else if let Some(files) = egui::DragAndDrop::take_payload::<DraggedFiles>(ui.ctx()) {
                let at = self.playlists.lists[list].playlist.entries.len();
                self.insert_songs(list, at, files.paths.clone());
            } else if egui::DragAndDrop::take_payload::<DraggedSoundfont>(ui.ctx()).is_some() {
                self.status = "Drop a soundfont onto a MIDI entry to override its soundfont.".to_string();
            }
        }

        match row_drop {
            Some(RowDrop::Songs(paths, at)) => self.insert_songs(list, at, paths),
            Some(RowDrop::Move(indices, before)) => self.move_entries(list, &indices, before),
            Some(RowDrop::Soundfont(sf, idx)) => {
                let rows = self.playlist_selection.rows_for(idx);
                self.set_entries_soundfont(list, &rows, Some(sf));
            }
            None => {}
        }
        match action {
            Some(RowAction::Play(idx)) => self.play_entry(list, idx),
            Some(RowAction::Remove(rows)) => self.remove_entries(list, &rows),
            Some(RowAction::Tag(tag, paths)) => self.tag_songs(tag, &paths),
            Some(RowAction::ClearSoundfont(rows)) => self.set_entries_soundfont(list, &rows, None),
            Some(RowAction::UseCurrentSoundfont(rows)) => {
                let selected = self.active_sf.and_then(|i| self.config.soundfonts.get(i)).cloned();
                match selected {
                    Some(sf) => self.set_entries_soundfont(list, &rows, Some(sf)),
                    None => self.status = "No soundfont in use.".to_string(),
                }
            }
            None => {}
        }
    }

    /// Play playlist `list` from the top (from anywhere, shuffling).
    pub(super) fn play_playlist(&mut self, list: usize) {
        let len = self.playlists.lists.get(list).map_or(0, |l| l.playlist.entries.len());
        if len == 0 {
            return;
        }
        let start = if self.shuffle { (self.rng.next() % len as u64) as usize } else { 0 };
        self.play_entry(list, start);
    }

    /// One row: Back and the playlist's name (a text box in its place
    /// while renaming; a phone's top bar has both), then play-from-the-top,
    /// continue, rename, export and delete.
    fn playlist_header_ui(&mut self, ui: &mut egui::Ui, list: usize) {
        let phone = self.mobile();
        let mut back = false;
        let (mut edit_rules, mut freeze) = (None, None);
        ui.horizontal_wrapped(|ui| {
            if !phone && ui.add(super::icon_button("arrow-left")).on_hover_text("Back to Playlists").clicked() {
                back = true;
            }
            if let Some(name) = &mut self.playlist_rename {
                let response = ui.add(egui::TextEdit::singleline(name).desired_width(160.0));
                let commit = response.lost_focus() || ui.add(super::icon_button("check")).on_hover_text("Rename").clicked();
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
            } else if !phone {
                let playlist = &self.playlists.lists[list].playlist;
                let name = match playlist.smart {
                    Some(_) => format!("{} {}", super::icon(super::smart::SMART_ICON), playlist.name),
                    None => playlist.name.clone(),
                };
                ui.label(egui::RichText::new(name).strong());
            }

            let empty = self.playlists.lists[list].playlist.entries.is_empty();
            if ui.add_enabled(!empty, super::icon_button("play")).on_hover_text("Play it from the top").clicked() {
                self.play_playlist(list);
            }
            // Where it was left, unless that's playing now.
            let stored = &self.playlists.lists[list];
            let playing = self.now_playing.as_ref().is_some_and(|np| np.list == list);
            let left_at = self.playlist_positions.get(&stored.id).copied().filter(|&e| e < stored.playlist.entries.len() && !playing);
            if let Some(entry) = left_at {
                let path = stored.playlist.entries[entry].path.clone();
                let name = self.song_title(&path);
                if ui.add(super::icon_button("fast-forward")).on_hover_text(format!("Continue from {name}")).clicked() {
                    self.play_entry(list, entry);
                }
            }

            if self.playlists.lists[list].playlist.smart.is_some() {
                let (icon, hover) = if super::smart::rules_editable(ui.ctx()) { ("sliders-horizontal", "Edit rules") } else { ("eye", "View rules") };
                if ui.add(super::icon_button(icon)).on_hover_text(hover).clicked() {
                    edit_rules = Some(list);
                }
                if ui.add(super::icon_button("copy")).on_hover_text("Save as playlist: a copy of the songs on it now, which stays as it is").clicked() {
                    freeze = Some(list);
                }
            }
            if self.playlist_rename.is_none()
                && ui.add(super::icon_button("pencil-simple")).on_hover_text("Rename").clicked()
            {
                self.playlist_rename = Some(self.playlists.lists[list].playlist.name.clone());
            }
            #[cfg(not(target_os = "android"))]
            if ui
                .add(super::icon_button("export"))
                .on_hover_text("Export .m3u...: save as an .m3u8 playlist other players can open")
                .clicked()
            {
                self.export_m3u(list);
            }
            let response = if self.confirm_delete_playlist {
                ui.add(egui::Button::new(egui::RichText::new("Really delete?").color(ui.visuals().error_fg_color)))
            } else {
                ui.add(super::icon_button("trash")).on_hover_text("Delete this playlist")
            };
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
        if let Some(list) = edit_rules {
            self.edit_playlist_rules(ui.ctx(), Some(list));
        }
        if let Some(list) = freeze {
            self.freeze_playlist(list);
        }
        if back {
            self.open_playlist(None);
        }
    }

    /// +: a new playlist, blank or imported from an .m3u.
    pub(super) fn new_playlist_menu_ui(&mut self, ui: &mut egui::Ui) {
        egui::containers::menu::MenuButton::from_button(super::icon_button("plus")).ui(ui, |ui| {
            if ui.button("Blank").clicked() {
                let name = self.unique_playlist_name("New playlist");
                self.create_playlist(&name, Vec::new());
                self.playlist_rename = Some(name);
            }
            if super::smart::rules_editable(ui.ctx()) && ui.button("Smart playlist...").on_hover_text("Songs picked by rules, kept up to date as the library changes").clicked() {
                self.edit_playlist_rules(ui.ctx(), None);
            }
            #[cfg(not(target_os = "android"))]
            if ui.button("Import .m3u...").clicked() {
                self.import_m3u();
            }
        })
        .0
        .on_hover_text("New playlist");
    }

    /// The entries of playlist `list`: click to play, drag to reorder, drop
    /// songs between rows or soundfonts onto MIDI rows, right-click for more.
    fn playlist_rows_ui(&mut self, ui: &mut egui::Ui, list: usize) -> (Option<RowDrop>, Option<RowAction>) {
        let mut row_drop = None;
        let mut action = None;
        let entries = &self.playlists.lists[list].playlist.entries;
        // (A smart playlist's songs are its rules': not moved, dropped on
        // or removed by hand, nor given soundfonts.)
        let smart = self.playlists.lists[list].playlist.smart.is_some();
        let playing = self.now_playing.as_ref().filter(|np| np.list == list).map(|np| np.entry);
        let selected_sf = self.active_sf.and_then(|i| self.config.soundfonts.get(i)).is_some();
        let selection = &mut self.playlist_selection;
        let cache = &mut self.song_info;
        let naming = super::naming(&self.config, &self.songs);
        let info_open = &mut self.details_for;
        let store = &self.songs;
        let touch = select_list::touch(ui.ctx());

        // Total time from what's been read so far.
        let (mut total, mut unknown) = (0.0, 0usize);
        for entry in entries {
            match cache.get(&entry.path).and_then(|i| i.meta.length) {
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

        let output = select_list::show(ui, "playlist_rows", entries, selection, playing, |ui, row, entry| {
            let idx = row.idx;
            if !smart && row.response.drag_started() {
                let label = super::title_of(cache, &naming, &entry.path);
                row.response.dnd_set_drag_payload(DraggedEntry { list, indices: row.rows(), label });
            }
            row.response.context_menu(|ui| {
                let rows = row.rows();
                let count = if rows.len() > 1 { format!(" ({})", rows.len()) } else { String::new() };
                if ui.button("Play").clicked() {
                    action = Some(RowAction::Play(idx));
                }
                if rows.len() == 1 && !entry.missing && ui.button("Song info").clicked() {
                    *info_open = Some(vec![entry.path.clone()]);
                }
                if !smart && (is_midi_path(&entry.path) || rows.len() > 1) {
                    if ui
                        .add_enabled(selected_sf, egui::Button::new(format!("Use current soundfont{count}")))
                        .clicked()
                    {
                        action = Some(RowAction::UseCurrentSoundfont(rows.clone()));
                    }
                    if ui.button(format!("Clear soundfont override{count}")).clicked() {
                        action = Some(RowAction::ClearSoundfont(rows.clone()));
                    }
                }
                let paths: Vec<PathBuf> = rows.iter().filter_map(|&i| entries.get(i)).filter(|e| !e.missing).map(|e| e.path.clone()).collect();
                if !paths.is_empty()
                    && let Some(tag) = super::tags::tags_menu(ui, store, &paths)
                {
                    action = Some(RowAction::Tag(tag, paths));
                }
                if !smart && ui.button(format!("Remove from playlist{count}")).clicked() {
                    action = Some(RowAction::Remove(rows));
                }
            });

            if select_list::star(ui, store.is_starred(&entry.path)) && !entry.missing {
                action = Some(RowAction::Tag(super::tags::TagAction::Set(crate::song_store::STARRED.into(), !store.is_starred(&entry.path)), vec![entry.path.clone()]));
            }
            let tags = store.tags_of(&entry.path);
            let tags = tags.iter().map(|(_, tag)| (tag.name.as_str(), tag.color));
            let mut label = super::title_of(cache, &naming, &entry.path);
            let scroll = select_list::scrolls(row, playing == Some(idx));
            if touch {
                // Under the name: the entry's number, length and anything
                // else to know of it.
                let info = cache.get(&entry.path);
                let mut detail = vec![(idx + 1).to_string(), super::length_text(info.as_deref())];
                if let Some(sf) = &entry.soundfont {
                    detail.push(format!("SF: {}", nice_name(sf)));
                }
                if info.is_some_and(|i| i.is_heavy()) {
                    detail.push("a very large MIDI file".to_string());
                }
                if entry.missing {
                    detail.push("missing".to_string());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    select_list::two_lines(ui, tags, label, Some(detail.join(" · ")), scroll);
                });
            } else {
                ui.weak(format!("{:>3}", idx + 1));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    song_info_widgets(ui, cache, &entry.path, true);
                    if let Some(sf) = &entry.soundfont
                        && ui
                            .small_button(format!("SF: {}", nice_name(sf)))
                            .on_hover_text(format!(
                                "Plays with this soundfont instead of the current one.\n{}\nClick to clear.",
                                sf.display()
                            ))
                            .clicked()
                    {
                        action = Some(RowAction::ClearSoundfont(vec![idx]));
                    }
                    select_list::chips(ui, tags);
                    if entry.missing {
                        label.push_str("  (missing)");
                    }
                    select_list::label(ui, label, Some(format!("{}\nDouble-click to play", entry.path.display())), scroll);
                });
            }

            if !smart && let Some(drop) = row_drop_ui(ui, &row.response, list, idx, is_midi_path(&entry.path)) {
                row_drop = Some(drop);
            }
        });
        if let Some(idx) = output.activated {
            action = Some(RowAction::Play(idx));
        }
        if output.delete && !smart {
            action = Some(RowAction::Remove(self.playlist_selection.rows().to_vec()));
        }
        let hovered = output.hovered.map(|i| (entries[i].path.clone(), entries[i].soundfont.clone()));

        self.hovered_playlist_entry = hovered;
        (row_drop, action)
    }

    /// New playlist from an .m3u/.m3u8 file (named after it).
    #[cfg(not(target_os = "android"))]
    fn import_m3u(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("M3U playlist", &["m3u", "m3u8"])
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
        let missing = entries.iter().filter(|e| e.missing).count();
        let files: Vec<(PathBuf, Option<PathBuf>)> =
            entries.into_iter().filter(|e| !e.missing).map(|e| (e.path, e.soundfont)).collect();
        if files.is_empty() {
            self.status = format!("No songs found in {}.", nice_name(&path));
            return;
        }
        crate::import::songs(files, SONG_EXTENSIONS, crate::import::Then::NewPlaylist { name: nice_name(&path) });
        self.status = match missing {
            0 => format!("Importing {}...", nice_name(&path)),
            n => format!("Importing {} ({n} of its songs weren't found)...", nice_name(&path)),
        };
    }

    /// Save playlist `list` as an .m3u8 file.
    #[cfg(not(target_os = "android"))]
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
        let naming = super::naming(&self.config, &self.songs);
        let text = playlist::to_m3u(&stored.playlist, |p| {
            (super::title_of(cache, &naming, p), cache.get(p).and_then(|i| i.meta.length))
        });
        self.status = match std::fs::write(&path, text) {
            Ok(()) => format!("Exported to {}.", path.display()),
            Err(e) => format!("Couldn't write {}: {e}", path.display()),
        };
    }

    pub(super) fn unique_playlist_name(&self, base: &str) -> String {
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
    if egui::DragAndDrop::has_payload_of_type::<DraggedFiles>(ctx) {
        ui.painter().hline(rect.x_range(), line_y, stroke);
        if released && let Some(files) = egui::DragAndDrop::take_payload::<DraggedFiles>(ctx) {
            return Some(RowDrop::Songs(files.paths.clone(), insert_at));
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
