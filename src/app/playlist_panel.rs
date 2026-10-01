//! The Playlists section and everything playlist-driven on the `App` side:
//! advancing tracks, Next/Previous, the loop/shuffle interplay with the
//! engine, drag-and-drop into and within a playlist, and files dropped onto
//! the window from the OS. The data model and the pure next/previous logic
//! are in `crate::playlist`.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{is_midi_path, App, DraggedSoundfont, PREVIOUS_RESTARTS_AFTER_SECS, SONG_EXTENSIONS};
use crate::audio::AudioCommand;
use crate::config::nice_name;
use crate::engine::EngineView;
use crate::filebrowser::DraggedFile;
use crate::layout::Section;
use crate::playlist::{self, LoopMode, NowPlaying, PlaylistEntry};

/// Drag-and-drop payload for a playlist row being dragged to reorder it.
pub(super) struct DraggedEntry {
    pub list: usize,
    pub idx: usize,
    pub path: PathBuf,
}

/// Something dropped onto a playlist row this frame.
enum RowDrop {
    /// A song from the browser, to insert at this position.
    Song(PathBuf, usize),
    /// Another row of the same playlist, moved to this position.
    Move(usize, usize),
    /// A soundfont, to become this row's override.
    Soundfont(PathBuf, usize),
}

/// A row's right-click menu choice, or a click on one of its own controls.
enum RowAction {
    Play(usize),
    Remove(usize),
    ClearSoundfont(usize),
    UseSelectedSoundfont(usize),
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
    fn play_entry(&mut self, list: usize, entry: usize) {
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
        self.now_playing = self.now_playing.take().and_then(|mut np| {
            np.list = playlist::index_after_remove(np.list, list)?;
            Some(np)
        });
        self.viewed_playlist = self
            .viewed_playlist
            .and_then(|v| playlist::index_after_remove(v, list))
            .or_else(|| (!self.playlists.lists.is_empty()).then_some(0));
    }

    fn insert_songs(&mut self, list: usize, at: usize, songs: Vec<PathBuf>) {
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

    fn move_entry(&mut self, list: usize, from: usize, to: usize) {
        let Some(stored) = self.playlists.lists.get_mut(list) else {
            return;
        };
        let len = stored.playlist.entries.len();
        if from >= len || from == to {
            return;
        }
        let to = to.min(len - 1);
        let entry = stored.playlist.entries.remove(from);
        stored.playlist.entries.insert(to, entry);
        if let Some(np) = &mut self.now_playing
            && np.list == list
        {
            np.entry = playlist::index_after_move(np.entry, from, to);
            np.history.iter_mut().for_each(|h| *h = playlist::index_after_move(*h, from, to));
        }
        self.plan_upcoming();
        self.save_playlist(list);
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
        match row_drop {
            Some(RowDrop::Song(path, at)) => self.insert_songs(list, at, vec![path]),
            Some(RowDrop::Move(from, before)) => {
                // `before` is an insertion point in the current list; removing
                // `from` first shifts everything after it up by one.
                let to = if before > from { before - 1 } else { before };
                self.move_entry(list, from, to);
            }
            Some(RowDrop::Soundfont(sf, idx)) => self.set_entry_soundfont(list, idx, Some(sf)),
            None => {}
        }
        match action {
            Some(RowAction::Play(idx)) => self.play_entry(list, idx),
            Some(RowAction::Remove(idx)) => self.remove_entry(list, idx),
            Some(RowAction::ClearSoundfont(idx)) => self.set_entry_soundfont(list, idx, None),
            Some(RowAction::UseSelectedSoundfont(idx)) => {
                let selected = self.active_sf.and_then(|i| self.config.soundfonts.get(i)).cloned();
                match selected {
                    Some(sf) => self.set_entry_soundfont(list, idx, Some(sf)),
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

        let missing = entries.iter().filter(|e| e.missing).count();
        let summary = match missing {
            0 => format!("{} song(s)", entries.len()),
            n => format!("{} song(s), {n} missing", entries.len()),
        };
        ui.weak(summary);
        ui.separator();

        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("playlist_rows")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, entries.len(), |ui, range| {
                for idx in range {
                    let entry = &entries[idx];
                    let row = ui.horizontal(|ui| {
                        ui.weak(format!("{:>3}", idx + 1));
                        let mut label = nice_name(&entry.path);
                        if entry.missing {
                            label.push_str("  (missing)");
                        }
                        let response = ui
                            .add(
                                egui::Button::selectable(playing == Some(idx), label)
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text(entry.path.display().to_string());
                        if response.hovered() {
                            hovered = Some((entry.path.clone(), entry.soundfont.clone()));
                        }
                        response.dnd_set_drag_payload(DraggedEntry {
                            list,
                            idx,
                            path: entry.path.clone(),
                        });
                        if response.clicked() {
                            action = Some(RowAction::Play(idx));
                        }
                        response.context_menu(|ui| {
                            if ui.button("Play").clicked() {
                                action = Some(RowAction::Play(idx));
                            }
                            if is_midi_path(&entry.path) {
                                if ui
                                    .add_enabled(selected_sf, egui::Button::new("Use selected soundfont for this"))
                                    .clicked()
                                {
                                    action = Some(RowAction::UseSelectedSoundfont(idx));
                                }
                                if entry.soundfont.is_some() && ui.button("Clear soundfont override").clicked() {
                                    action = Some(RowAction::ClearSoundfont(idx));
                                }
                            }
                            if ui.button("Show in Songs").clicked() {
                                action = Some(RowAction::RevealInSongs(idx));
                            }
                            if ui.button("Remove from playlist").clicked() {
                                action = Some(RowAction::Remove(idx));
                            }
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("x").on_hover_text("Remove from playlist").clicked() {
                                action = Some(RowAction::Remove(idx));
                            }
                            if let Some(sf) = &entry.soundfont
                                && ui
                                    .small_button(format!("SF: {}", nice_name(sf)))
                                    .on_hover_text(format!(
                                        "Plays with this soundfont instead of the selected one.\n{}\nClick to clear.",
                                        sf.display()
                                    ))
                                    .clicked()
                            {
                                action = Some(RowAction::ClearSoundfont(idx));
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
            return Some(RowDrop::Move(entry.idx, insert_at));
        }
    } else if egui::DragAndDrop::has_payload_of_type::<DraggedSoundfont>(ctx) && is_midi {
        ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
        if released && let Some(sf) = egui::DragAndDrop::take_payload::<DraggedSoundfont>(ctx) {
            return Some(RowDrop::Soundfont(sf.0.clone(), idx));
        }
    }
    None
}
