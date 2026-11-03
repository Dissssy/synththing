//! The Songs panel and the library around it: adding, copying and removing
//! songs, missing files, imports.

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::frame_timing;
use crate::config::{self, Config};
use crate::song_list;
use crate::song_store::{self, SongStore, Stored};
use crate::select_list;
use crate::playlist::Library;

use super::{App, SONG_EXTENSIONS, autoplay, describe_song, file_manager, icon_button, naming, queue, song_info_widgets, sync, tags, toast, wishlist};

impl App {
    /// The revision of what the songs are listed from: it changes whenever
    /// the songs, what's been read of them, or how they're named do.
    pub(super) fn songs_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.songs.revision(), self.song_info.revision(), &self.config.title_patterns, &self.config.group_ignore, self.config.sort_with_articles)
            .hash(&mut hasher);
        hasher.finish()
    }

    pub(super) fn songs_ui(&mut self, ui: &mut egui::Ui) {
        if self.old_import.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Bringing your songs into the library...");
            });
            return;
        }
        let extensions = SONG_EXTENSIONS;
        // Only the songs a ruleset leaves out, when asked.
        let missing_only = self.songs_missing_only;
        let (missing_revision, missing) = self.missing_info();
        let any_missing = !missing.is_empty();
        let only: Option<std::collections::HashSet<&str>> = missing_only.then(|| missing.iter().flat_map(|m| m.keys.iter().map(String::as_str)).collect());
        let mut missing_clicked = false;
        // What the list's made from: it's made again when this changes.
        let revision = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (self.songs_revision(), self.wishlist.list.revision(), missing_only.then_some(missing_revision)).hash(&mut hasher);
            hasher.finish()
        };
        let selected = self.song_list.selected();
        let mut info_for: Option<Vec<PathBuf>> = None;
        #[cfg_attr(target_os = "android", expect(unused_mut, reason = "phones have no Song info button here"))]
        let mut info_clicked = false;
        let can_undo = self.last_edit.is_some();
        let mut undo_clicked = false;
        #[cfg_attr(target_os = "android", expect(unused_mut, reason = "phones have no Clean up button here"))]
        let mut cleanup_clicked = false;
        let moving = self.songs_moving();
        let autoplay = self.autoplay;
        let mut autoplay_clicked = false;
        let mut tools = |ui: &mut egui::Ui| {
            #[cfg(target_os = "android")]
            if ui.add(icon_button("plus")).on_hover_text("Import songs: copy them into synththing's library").clicked() {
                crate::import::start(crate::import::Kind::Songs, SONG_EXTENSIONS);
            }
            #[cfg(not(target_os = "android"))]
            egui::containers::menu::MenuButton::from_button(icon_button("plus"))
                .config(crate::menu::config(ui.ctx()))
                .ui(ui, |ui| {
                    if ui.button("Songs...").on_hover_text("Copy songs into synththing's library").clicked() {
                        crate::import::start(crate::import::Kind::Songs, SONG_EXTENSIONS);
                    }
                    if ui.button("A folder...").on_hover_text("Every song in a folder and in the folders inside it").clicked() {
                        crate::import::start_folder(SONG_EXTENSIONS);
                    }
                })
                .0
                .on_hover_text("Import songs");
            #[cfg(not(target_os = "android"))]
            if ui
                .add_enabled(!selected.is_empty(), icon_button("info"))
                .on_hover_text("Song info: see and edit its details (several songs' at once)")
                .on_disabled_hover_text("Song info: select songs")
                .clicked()
            {
                info_clicked = true;
            }
            #[cfg(not(target_os = "android"))]
            if ui.add(icon_button("broom")).on_hover_text("Clean up: suggestions for your songs' details, like splitting several artists").clicked() {
                cleanup_clicked = true;
            }
            if can_undo && ui.add(icon_button("arrow-u-up-left")).on_hover_text("Undo the last edit of songs' details").clicked() {
                undo_clicked = true;
            }
            if (any_missing || missing_only)
                && ui
                    .add(icon_button("warning-circle").selected(missing_only))
                    .on_hover_text("Missing info: show only the songs a ruleset leaves out, as they don't say what it splits by (Preferences > Library lists them)")
                    .clicked()
            {
                missing_clicked = true;
            }
            sync::songs_moving_ui(ui, moving);
            autoplay_clicked |= autoplay::button_ui(ui, autoplay);
        };
        // (Both the names and the trailing widgets read the song info, one
        // after the other.)
        let cache = std::cell::RefCell::new(&mut self.song_info);
        let naming = naming(&self.config, &self.songs);
        let mut view = self.config.songs_view.clone();
        #[cfg(not(target_os = "android"))]
        let mut save_copy = None;
        let mut remove = Vec::new();
        let mut tag = None;
        let mut together: Option<(bool, Vec<PathBuf>)> = None;
        let mut queued: Option<(Vec<PathBuf>, bool)> = None;
        let store = &self.songs;
        let mut add_to: Option<(Option<usize>, Vec<PathBuf>)> = None;
        // With no dragging (touch), songs go onto playlists from their menu.
        let touch = select_list::touch(ui.ctx());
        // (Not the smart ones: their rules say what's on them.)
        let playlists: Vec<(usize, String)> = if touch {
            self.playlists.lists.iter().enumerate().filter(|(_, l)| l.playlist.smart.is_none()).map(|(i, l)| (i, l.playlist.name.clone())).collect()
        } else {
            Vec::new()
        };
        let mut wish = None;
        let (kept, wishes) = (&self.lookup.kept, &self.wishlist.list);
        let online = self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        let mut menu = |ui: &mut egui::Ui, paths: &[PathBuf]| {
            if let Some(next) = queue::menu(ui) {
                queued = Some((paths.to_vec(), next));
            }
            if let [path] = paths
                && let Some(key) = store.key(path)
                && let Some(album) = wishlist::album_wish_menu(ui, store, kept, wishes, online, &[key])
            {
                wish = Some(album);
            }
            if touch {
                ui.menu_button("Add to playlist", |ui| {
                    for (i, name) in &playlists {
                        if ui.button(name).clicked() {
                            add_to = Some((Some(*i), paths.to_vec()));
                        }
                    }
                    if ui.button("New playlist").clicked() {
                        add_to = Some((None, paths.to_vec()));
                    }
                });
            }
            #[cfg(not(target_os = "android"))]
            if let [path] = paths
                && ui.button("Save a copy as...").on_hover_text("Write the song to a file, under its own name").clicked()
            {
                save_copy = Some(path.clone());
            }
            #[cfg(not(target_os = "android"))]
            if let [path] = paths {
                file_manager::show_in_folder_button(ui, path);
            }
            if ui.button("Song info").clicked() {
                info_for = Some(paths.to_vec());
            }
            if let Some(action) = tags::tags_menu(ui, store, paths) {
                tag = Some((action, paths.to_vec()));
            }
            if paths.len() > 1 && ui.button("Play together").on_hover_text("Play them in this order, as one song when shuffling").clicked() {
                together = Some((true, paths.to_vec()));
            }
            if paths.iter().any(|p| store.together_of(p).is_some()) && ui.button("Stop playing together").clicked() {
                together = Some((false, paths.to_vec()));
            }
            if ui.button("Remove from library...").on_hover_text("Delete synththing's copy on this device").clicked() {
                remove = paths.to_vec();
            }
        };
        let output = self.song_list.ui(
            ui,
            &mut Listed { store, wishes: &wishes.wishes, extensions, only: only.as_ref() },
            revision,
            &mut view,
            &self.config.group_ignore,
            self.config.sort_with_articles,
            &mut tools,
            &mut |path, keys| describe_song(&mut cache.borrow_mut(), &naming, &self.songs, path, keys),
            &mut |ui, path| song_info_widgets(ui, &mut cache.borrow_mut(), path, false),
            &mut menu,
        );
        if output.view_changed {
            self.config.songs_view = view;
            if let Err(e) = self.config.save() {
                log::warn!("saving the config: {e}");
            }
        }
        if !output.delete.is_empty() {
            remove = output.delete;
        }
        if let Some((path, on)) = output.star {
            tag = Some((tags::TagAction::Set(song_store::STARRED.into(), on), vec![path]));
        }
        if let Some(at) = output.play {
            self.play_listed(at);
        }
        if let Some((paths, next)) = queued {
            self.queue_songs(ui.ctx(), paths, next);
        }
        if let Some(wish) = wish {
            self.wish_for_album(ui.ctx(), wish);
        }
        if let Some(query) = output.save_search {
            self.smart_playlist_from_search(ui.ctx(), &query);
        }
        if autoplay_clicked {
            self.set_autoplay(self.autoplay.cycle());
            toast::show(ui.ctx(), autoplay::text(self.autoplay));
        }
        if undo_clicked {
            self.undo_edit();
        }
        if cleanup_clicked {
            self.cleanup = Some(Default::default());
        }
        if missing_clicked {
            self.songs_missing_only = !missing_only;
            toast::show(ui.ctx(), if missing_only { "Missing info: off" } else { "Missing info: only songs missing it" });
        }
        if let Some(paths) = info_for.or(info_clicked.then_some(selected)) {
            self.details_for = Some(paths);
        }
        #[cfg(not(target_os = "android"))]
        if let Some(path) = save_copy {
            self.save_song_copy(&path);
        }
        if !remove.is_empty() {
            self.remove_songs = remove;
        }
        if let Some((list, paths)) = add_to {
            self.add_to_playlist(list, paths);
        }
        if let Some((action, paths)) = tag {
            self.tag_songs(action, &paths);
        }
        if let Some((on, paths)) = together {
            let keys: Vec<String> = paths.iter().filter_map(|p| self.songs.key(p)).collect();
            if on {
                self.songs.set_together(&crate::library::new_nonce(), keys);
            } else {
                self.songs.leave_together(&keys);
            }
            self.songs_changed();
        }
    }

    /// Save a copy of the library's song at `path`, under its own name.
    #[cfg(not(target_os = "android"))]
    pub(super) fn save_song_copy(&mut self, path: &Path) {
        let name = naming(&self.config, &self.songs).song(path, None).file_name.to_string();
        let Some(to) = rfd::FileDialog::new().set_file_name(&name).save_file() else { return };
        self.status = match std::fs::copy(path, &to) {
            Ok(_) => format!("Saved a copy as {}.", to.display()),
            Err(e) => format!("Couldn't save {}: {e}", to.display()),
        };
    }

    /// Put songs now in the store into the library: every way a song comes
    /// in goes through here, so playlists waiting on one stop showing it
    /// missing.
    pub(super) fn add_to_library(&mut self, songs: impl IntoIterator<Item = Stored>) {
        let mut added = std::collections::BTreeSet::new();
        for stored in songs {
            let path = self.songs.path(&stored.key);
            self.song_info.forget(&path);
            added.insert(path);
            self.songs.add(stored);
        }
        // (Only the entries for these songs can have come back: no need to
        // look for every entry's file.)
        for stored in &mut self.playlists.lists {
            for entry in &mut stored.playlist.entries {
                if added.contains(&entry.path) {
                    entry.missing = false;
                }
            }
        }
        self.songs_changed();
    }

    /// Mark playlist entries missing, or not, as their songs leave or come
    /// into the library.
    pub(super) fn refresh_missing(&mut self) {
        let _timing = frame_timing::scope("refresh_missing");
        for stored in &mut self.playlists.lists {
            for entry in &mut stored.playlist.entries {
                entry.missing = !entry.path.exists();
            }
        }
        self.songs_changed();
    }

    /// Asked to remove songs from the library: are you sure?
    pub(super) fn remove_songs_ui(&mut self, ctx: &egui::Context) {
        if self.remove_songs.is_empty() {
            return;
        }
        let what = match self.remove_songs.as_slice() {
            [one] => format!("\"{}\"", self.song_title(&one.clone())),
            more => format!("{} songs", more.len()),
        };
        let mut close = false;
        let mut remove = false;
        let synced = self.syncs_library();
        let everywhere = &mut self.remove_everywhere;
        let modal = egui::Modal::new(egui::Id::unique("remove_songs")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading("Remove from library?");
            if synced {
                ui.checkbox(everywhere, "Remove from all my devices");
            }
            if synced && *everywhere {
                ui.label(format!(
                    "This deletes synththing's copy of {what} on this device and on all your other devices. \
                     Playlists keep them, shown as unavailable. Importing one again brings it back everywhere."
                ));
            } else {
                ui.label(format!(
                    "This deletes synththing's copy of {what} on this device. Playlists keep them, shown as unavailable here. \
                     Your other devices keep theirs, and this one won't fetch them from them again."
                ));
            }
            ui.horizontal(|ui| {
                remove = ui.button("Remove").clicked();
                close = ui.button("Cancel").clicked();
            });
        });
        if remove {
            let paths = std::mem::take(&mut self.remove_songs);
            let everywhere = synced && self.remove_everywhere;
            for path in &paths {
                let removed = match self.songs.key(path) {
                    Some(key) if everywhere => self.songs.remove_everywhere(&key),
                    _ => self.songs.remove(path),
                };
                if let Err(e) = removed {
                    self.status = format!("Couldn't remove a song: {e:#}");
                }
            }
            self.refresh_missing();
            self.song_list.clear_selection();
            if self.status.is_empty() || !self.status.starts_with("Couldn't") {
                let from = if everywhere { "all your devices" } else { "the library" };
                self.status = format!("Removed {what} from {from}.");
            }
        } else if close || modal.should_close() {
            self.remove_songs.clear();
        }
    }

    /// Imports that have finished (`import.rs`): songs join the library
    /// (and go where they were headed: a playlist), soundfonts join the
    /// list. What was picked but isn't of a type synththing takes is said,
    /// and audio files with audio files turned off bring an offer to turn
    /// them on.
    pub(super) fn poll_imports(&mut self, ctx: &egui::Context) {
        // (Waiting for the one-time import, which writes the library's
        // index last: until then, nothing else may.)
        if self.old_import.is_some() {
            return;
        }
        for done in crate::import::take_finished() {
            let imported = match done.result {
                Ok(imported) => imported,
                Err(e) => {
                    self.status = format!("Couldn't import: {e}");
                    continue;
                }
            };
            let mut notes = Vec::new();
            match imported.skipped.as_slice() {
                [] => {}
                [one] => notes.push(format!("{one} isn't a file synththing can play.")),
                more => notes.push(format!("{} aren't files synththing can play.", more.join(", "))),
            }
            if let Some(e) = imported.failed.first() {
                log::warn!("importing: {:?}", imported.failed);
                notes.push(format!("{} couldn't be imported ({e}).", imported.failed.len()));
            }
            let n = imported.songs.len() + imported.soundfonts.len() + imported.images.len();
            if n == 0 {
                if !notes.is_empty() {
                    self.status = format!("Couldn't import: {}", notes.join(" "));
                }
                continue;
            }
            match done.kind {
                crate::import::Kind::Songs => {
                    let mut paths = Vec::new();
                    let mut soundfonts = Vec::new();
                    let mut stored = Vec::new();
                    for song in imported.songs {
                        paths.push(self.songs.path(&song.stored.key));
                        soundfonts.push(song.soundfont);
                        stored.push(song.stored);
                    }
                    self.add_to_library(stored);
                    let plural = if n == 1 { "" } else { "s" };
                    self.status = format!("Imported {n} song{plural}.");
                    self.after_import(ctx, done.then, paths, soundfonts);
                }
                crate::import::Kind::Soundfonts => self.add_soundfonts(imported.soundfonts),
                crate::import::Kind::Image => {
                    if let Some(image) = imported.images.into_iter().next() {
                        self.picture_picked(image);
                    }
                }
            }
            if !notes.is_empty() {
                self.status = format!("{} {}", self.status, notes.join(" "));
            }
        }
    }

    /// The one-time import of the songs from before the library, once it's
    /// done: its songs join the library (with what's been read of them),
    /// and the playlists, now naming them, are read again.
    pub(super) fn poll_old_import(&mut self) {
        let Some(done) = self.old_import.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.old_import = None;
        let count = done.moved.len();
        for (old, new) in &done.moved {
            self.song_info.rename(old, new);
        }
        self.songs = SongStore::open(self.songs.dir().to_path_buf());
        let (playlists, errors) = Library::load(
            config::playlists_dir().unwrap_or_else(|_| PathBuf::from("playlists")),
            self.songs.dir().to_path_buf(),
        );
        self.playlists = playlists;
        self.viewed_playlist = None;
        self.now_playing = None;
        let mut status = match count {
            0 => String::new(),
            1 => "Brought 1 song into the library.".to_string(),
            n => format!("Brought {n} songs into the library."),
        };
        if done.dropped > 0 {
            status.push_str(&format!(" {} playlist entries were left out: their files weren't there any more.", done.dropped));
        }
        if let Some(e) = done.failed.first().or(errors.first()) {
            log::warn!("bringing the old songs in: {:?} {:?}", done.failed, errors);
            status.push_str(&format!(" Some couldn't be brought in ({e})."));
        }
        if !status.is_empty() {
            self.status = status.trim().to_string();
        }
    }
}

pub(super) fn start_old_import(dir: PathBuf) -> std::sync::mpsc::Receiver<song_store::OldImport> {
    let folders: Vec<PathBuf> = if cfg!(target_os = "android") {
        vec![dir.clone()]
    } else {
        let starter = crate::starter::old_songs_dir();
        let old = Config::old_songs_folder().or_else(|| starter.parent().map(Path::to_path_buf));
        old.into_iter().filter(|d| d.is_dir()).collect()
    };
    let playlists = config::playlists_dir().unwrap_or_else(|_| PathBuf::from("playlists"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(song_store::import_old(&folders, &dir, &playlists, SONG_EXTENSIONS));
    });
    rx
}

/// The Songs list's songs: the library's (with `extensions`, and only
/// those in `only` when given), as a query's filters keep them.
pub(super) struct Listed<'a> {
    store: &'a SongStore,
    wishes: &'a crate::wishlist::Wishes,
    extensions: &'a [&'a str],
    only: Option<&'a std::collections::HashSet<&'a str>>,
}

impl song_list::Source for Listed<'_> {
    fn songs(&mut self, query: &str) -> (Vec<PathBuf>, crate::search::Search) {
        let search = crate::search::parse(query, self.store.tags());
        let songs = {
            let matches = search.rules.matcher(self.store, self.wishes, crate::settings_sync::now());
            self.store
                .songs()
                .filter(|(path, song)| song_store::has_extension(path, self.extensions) && song_store::key_in(self.store.dir(), path).is_some_and(|key| matches(key, song)))
                .map(|(path, _)| path)
                .filter(|p| self.only.is_none_or(|only| song_store::key_in(self.store.dir(), p).is_some_and(|k| only.contains(k))))
                .collect()
        };
        (songs, search)
    }

    fn values(&mut self, field: crate::fields::Field, query: &str) -> Vec<String> {
        let rules = crate::search::parse(query, self.store.tags()).rules;
        let matches = rules.matcher(self.store, self.wishes, crate::settings_sync::now());
        let songs = self.store.songs().filter(|(path, song)| song_store::has_extension(path, self.extensions) && song_store::key_in(self.store.dir(), path).is_some_and(|key| matches(key, song))).map(|(_, song)| song);
        if field == crate::fields::Field::Wish {
            // (Those the songs kept fill.)
            let index = crate::wishlist::Index::new(self.wishes);
            let filled: std::collections::BTreeSet<&str> = songs.flat_map(|song| index.fills(song)).map(|id| index.name(id)).collect();
            return filled.into_iter().map(str::to_string).collect();
        }
        if field == crate::fields::Field::Tag {
            let used: std::collections::BTreeSet<&str> = songs.flat_map(|song| song.tags.iter().map(String::as_str)).collect();
            return used.into_iter().filter_map(|id| self.store.tags().get(id)).map(|t| t.name.clone()).collect();
        }
        // (Each once, however it's spelled: as most songs spell it.)
        let mut spellings: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for song in songs {
            for value in crate::fields::Facts::of(song).items(field) {
                spellings.entry(crate::fields::group_key(&value, "")).or_default().push(value.into_owned());
            }
        }
        spellings.values().filter_map(|s| crate::fields::best_spelling(s.iter().map(String::as_str)).map(str::to_string)).collect()
    }
}
