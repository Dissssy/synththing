//! Songs in the Library (docs/SERVER.md): a song's own part of its details
//! (its credits and facts, Download), downloading songs into the `Library`
//! folder (in the song browser's default folder, else beside the starter
//! songs), each with a `.song.json` saying where it came from, the
//! Downloaded songs tab, and Publish song... (the song browser's
//! right-click menu). Searching, encores, reports and deleting are the
//! Library's own (`library.rs`), the same for songs as for scripts.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use super::library::host;
use super::{info_icon, App};
use crate::library::songs::{self, SongSource, SongUpload};
use crate::library::{self, client, Info, Receipt, ScriptSummary};

enum Event {
    Info(String, Result<Info, String>),
    Downloaded(String, Box<ScriptSummary>, Result<client::SongDownload, String>),
    /// An upload finished: the server, its key, the file, the upload, the result.
    Published(String, String, PathBuf, Box<SongUpload>, Result<Receipt, String>),
}

/// What's typed into Publish song... so far.
struct SongDraft {
    file: PathBuf,
    name: String,
    composer: String,
    arranger: String,
    from: &'static str,
    from_title: String,
    rights: &'static str,
    license: String,
    /// A script on the server it's made for: its ID and name.
    made_for: Option<(String, String)>,
    description: String,
    tags: String,
    author_name: String,
    server: String,
    slug: String,
    anonymous: bool,
    /// Published as a new song, even though it was published before.
    as_new: bool,
    /// Where it was published before, if it was.
    published: Option<SongSource>,
    busy: bool,
    error: Option<String>,
    /// Published: where (the server, its ID), and what to say about it.
    done: Option<(String, String, String)>,
}

pub struct SongLibraryState {
    events: (Sender<Event>, Receiver<Event>),
    publish: Option<SongDraft>,
    /// Songs saved from libraries: their file, and where they came from.
    downloaded: Vec<(PathBuf, SongSource)>,
    /// When `downloaded` was last read from the folder.
    scanned: Option<Instant>,
    /// A download is under way.
    busy: bool,
    /// Delete was clicked once (for this file); the next click confirms.
    confirm_delete: Option<PathBuf>,
}

impl Default for SongLibraryState {
    fn default() -> Self {
        Self {
            events: mpsc::channel(),
            publish: None,
            downloaded: Vec::new(),
            scanned: None,
            busy: false,
            confirm_delete: None,
        }
    }
}

impl SongLibraryState {
    pub fn open(&self) -> bool {
        self.publish.is_some()
    }
}

/// How often the Library folder is read again, while it's on screen.
const RESCAN_EVERY: Duration = Duration::from_secs(2);

fn is_midi(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"))
}

impl App {
    fn spawn_songs(&self, job: impl FnOnce() -> Event + Send + 'static) {
        let tx = self.song_library.events.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
    }

    /// Where songs from libraries are saved: `Library` in the song
    /// browser's default folder, or beside the starter songs.
    pub(super) fn library_songs_dir(&self) -> PathBuf {
        let base = self.config.browse_dir.clone().filter(|d| d.is_dir()).unwrap_or_else(|| {
            let starter = crate::starter::songs_dir();
            starter.parent().map(Path::to_path_buf).unwrap_or(starter)
        });
        base.join("Library")
    }

    /// Read the Library folder again (if it's been a while).
    fn rescan_downloaded(&mut self, force: bool) {
        if !force && self.song_library.scanned.is_some_and(|at| at.elapsed() < RESCAN_EVERY) {
            return;
        }
        let dir = self.library_songs_dir();
        let mut found: Vec<(PathBuf, SongSource)> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| is_midi(p))
                    .filter_map(|p| SongSource::read(&p).map(|s| (p, s)))
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by_key(|(_, s)| s.name.to_lowercase());
        self.song_library.downloaded = found;
        self.song_library.scanned = Some(Instant::now());
    }

    /// The downloaded copy of a song on `server`: its file and version.
    fn downloaded_copy(&mut self, server: &str, id: &str) -> Option<(PathBuf, u32)> {
        self.rescan_downloaded(false);
        self.song_library
            .downloaded
            .iter()
            .find(|(_, s)| s.id == id && library::resolve_server(&s.server) == server)
            .map(|(p, s)| (p.clone(), s.version))
    }

    /// Per frame: requests that finished.
    pub(super) fn poll_song_library(&mut self) {
        while let Ok(event) = self.song_library.events.1.try_recv() {
            match event {
                Event::Info(server, result) => {
                    if let Ok(info) = result {
                        self.note_server_info(server, info);
                    }
                }
                Event::Downloaded(server, summary, result) => {
                    self.song_library.busy = false;
                    match result {
                        Ok(download) => self.save_download(&server, &summary, download),
                        Err(e) => self.status = format!("Couldn't download {}: {e}", summary.name),
                    }
                }
                Event::Published(server, key, file, upload, result) => self.song_published(server, key, file, *upload, result),
            }
        }
    }

    /// Write a downloaded song into the Library folder (over the copy
    /// downloaded before, if there is one), noting where it came from.
    fn save_download(&mut self, server: &str, summary: &ScriptSummary, download: client::SongDownload) {
        let dir = self.library_songs_dir();
        let path = match self.downloaded_copy(server, &summary.id) {
            Some((path, _)) => path,
            None => {
                let stem = library::file_stem_for(&summary.name);
                let mut path = dir.join(format!("{stem}.mid"));
                let mut n = 2;
                while path.exists() {
                    path = dir.join(format!("{stem} ({n}).mid"));
                    n += 1;
                }
                path
            }
        };
        let source = SongSource {
            server: server.to_string(),
            id: summary.id.clone(),
            version: download.version,
            sha256: download.sha256.clone(),
            name: summary.name.clone(),
            author_name: summary.author_name.clone(),
            author_id: summary.author_id.clone(),
            slug: summary.slug.clone(),
            from: summary.category.clone(),
            song: summary.song.clone().unwrap_or_default(),
            receipt: None,
        };
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, &download.bytes))
            .and_then(|()| source.write(&path));
        match written {
            Ok(()) => {
                log::info!("downloaded song {} from {server} as {}", summary.id, path.display());
                self.rescan_downloaded(true);
                self.status = format!("Downloaded \"{}\": it's in Downloaded songs.", summary.name);
            }
            Err(e) => self.status = format!("Couldn't save {}: {e}", path.display()),
        }
    }

    /// A song's Download (or Play, once it's downloaded), in its details.
    pub(super) fn song_actions_ui(&mut self, ui: &mut egui::Ui, server: &str, summary: &ScriptSummary) {
        let mut download = false;
        match self.downloaded_copy(server, &summary.id) {
            Some((path, version)) => {
                if version >= summary.version {
                    ui.label("Downloaded");
                } else {
                    ui.label(format!("Version {version} downloaded; {} is out", summary.version));
                    download = ui.add_enabled(!self.song_library.busy, egui::Button::new("Download it")).clicked();
                }
                if ui.button("Play").clicked() {
                    self.play_path(path.clone());
                }
                if ui.button("Show in folder").clicked() {
                    super::open_in_file_manager(&path);
                }
            }
            None => {
                download = ui
                    .add_enabled(!self.song_library.busy, egui::Button::new("Download"))
                    .on_hover_text(format!("Save it in {}", self.library_songs_dir().display()))
                    .clicked();
            }
        }
        if !download {
            return;
        }
        let Some(key) = self.config.library.pinned.get(server).cloned() else {
            self.status = "That server's key isn't known yet: search again.".to_string();
            return;
        };
        self.song_library.busy = true;
        self.status = format!("Downloading \"{}\"...", summary.name);
        let (server, summary) = (server.to_string(), Box::new(summary.clone()));
        self.spawn_songs(move || {
            let result = client::details(&server, &summary.id).and_then(|details| {
                let Some(version) = details.newest_for(library::supported_version()) else {
                    let needs = details.newest().map(|v| v.min_app_version.clone()).unwrap_or_default();
                    return Err(format!("it needs synththing {needs} or newer"));
                };
                client::download_song(&server, &summary.id, Some(version.version), &key)
            });
            Event::Downloaded(server, summary, result)
        });
    }

    /// The Downloaded songs tab: songs saved from libraries.
    pub(super) fn downloaded_songs_ui(&mut self, ui: &mut egui::Ui) {
        self.rescan_downloaded(false);
        let dir = self.library_songs_dir();
        let mut open_library = false;
        ui.horizontal_wrapped(|ui| {
            ui.weak(dir.display().to_string());
            if dir.is_dir() && ui.small_button("Open folder").clicked() {
                super::open_in_file_manager(&dir);
            }
        });
        ui.separator();
        if self.song_library.downloaded.is_empty() {
            ui.weak("No songs downloaded yet.");
            open_library = ui.button("Find some in the Library").clicked();
        }
        let mut play = None;
        let mut delete = None;
        egui::ScrollArea::vertical().id_salt("downloaded_songs").auto_shrink([false, false]).show(ui, |ui| {
            for (path, source) in &self.song_library.downloaded {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.strong(&source.name);
                        let from = match source.song.from_title.trim() {
                            "" => songs::source_title(&source.from).to_string(),
                            title => format!("{} ({title})", songs::source_title(&source.from)),
                        };
                        ui.weak(format!("{} · {from}", source.song.composer));
                        let rights = match source.song.license.trim() {
                            "" => songs::rights_title(&source.song.rights).to_string(),
                            license => format!("{}: {license}", songs::rights_title(&source.song.rights)),
                        };
                        ui.weak(format!("{rights} · from {}", host(&library::resolve_server(&source.server))));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let confirming = self.song_library.confirm_delete.as_ref() == Some(path);
                        if ui.button(if confirming { "Click again to delete" } else { "Delete" }).clicked() {
                            delete = Some((path.clone(), confirming));
                        }
                        if ui.button("Show in folder").clicked() {
                            super::open_in_file_manager(path);
                        }
                        if ui.button("Play").clicked() {
                            play = Some(path.clone());
                        }
                    });
                });
                ui.separator();
            }
        });
        if let Some(path) = play {
            self.play_path(path);
        }
        match delete {
            Some((path, true)) => {
                self.song_library.confirm_delete = None;
                let removed = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(songs::source_path(&path));
                self.status = match removed {
                    Ok(()) => format!("Deleted {}.", path.display()),
                    Err(e) => format!("Couldn't delete {}: {e}", path.display()),
                };
                self.rescan_downloaded(true);
            }
            Some((path, false)) => self.song_library.confirm_delete = Some(path),
            None => {}
        }
        if open_library {
            self.set_open(crate::layout::Section::Library, true);
            self.library_show_songs(true);
        }
    }

    /// Publish song... for a MIDI file.
    pub(super) fn open_publish_song(&mut self, file: PathBuf) {
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Couldn't read {}: {e}", file.display());
                return;
            }
        };
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("Song").to_string();
        let suggested = songs::suggest(&bytes, &stem);
        let servers = self.config.library.enabled_servers();
        // (Which of them take songs isn't known until each has been asked.)
        for server in servers.iter().filter(|s| self.library_server_info(s).is_none()) {
            let server = server.clone();
            self.spawn_songs(move || Event::Info(server.clone(), client::info(&server)));
        }
        let published = SongSource::read(&file).filter(|s| s.receipt.is_some());
        let my_id = self.library.my_id();
        let server = published
            .as_ref()
            .map(|p| library::resolve_server(&p.server))
            .filter(|s| servers.contains(s))
            .or_else(|| servers.iter().find(|s| self.library_server_info(s).is_some_and(|i| i.songs)).cloned())
            .or_else(|| servers.first().cloned())
            .unwrap_or_default();
        let (name, composer, arranger, from, from_title) = match &published {
            Some(p) => (
                p.name.clone(),
                p.song.composer.clone(),
                p.song.arranger.clone(),
                songs::SOURCES.iter().find(|(s, _)| *s == p.from).map_or("other", |(s, _)| *s),
                p.song.from_title.clone(),
            ),
            None => (suggested.title, suggested.composer, suggested.arranger, "other", String::new()),
        };
        let rights = published
            .as_ref()
            .and_then(|p| songs::RIGHTS.iter().find(|(r, _)| *r == p.song.rights))
            .map_or("own", |(r, _)| *r);
        self.song_library.publish = Some(SongDraft {
            slug: published
                .as_ref()
                .filter(|p| p.author_id.is_some() && p.author_id == my_id)
                .and_then(|p| p.slug.clone())
                .unwrap_or_else(|| library::slugify(&name)),
            name,
            composer,
            arranger,
            from,
            from_title,
            rights,
            license: published.as_ref().map(|p| p.song.license.clone()).unwrap_or_default(),
            made_for: None,
            description: String::new(),
            tags: String::new(),
            author_name: self.config.library.author_name.clone(),
            server,
            anonymous: false,
            as_new: false,
            published,
            busy: false,
            error: None,
            done: None,
            file,
        });
    }

    /// The Publish song... window.
    pub(super) fn publish_song_ui(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.song_library.publish.take() else { return };
        let mut close = false;
        if let Some((server, id, said)) = draft.done.clone() {
            let mut show = false;
            let response = egui::Modal::new(egui::Id::new("publish_song")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.heading("Published");
                ui.label(said.as_str());
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    close = ui.button("Close").clicked();
                    show = ui.button("Open in Library").clicked();
                });
            });
            if show {
                self.set_open(crate::layout::Section::Library, true);
                self.library_show_songs(true);
                self.library_open_script(server, id);
            } else if !(close || response.should_close()) {
                self.song_library.publish = Some(draft);
            }
            return;
        }
        let servers: Vec<String> = self.config.library.enabled_servers();
        let taking: Vec<String> =
            servers.iter().filter(|s| self.library_server_info(s).is_some_and(|i| i.songs)).cloned().collect();
        let asking = servers.iter().any(|s| self.library_server_info(s).is_none());
        if !taking.contains(&draft.server)
            && let Some(first) = taking.first()
        {
            draft.server = first.clone();
        }
        let info = self.library_server_info(&draft.server).cloned();
        let signed_only = info.as_ref().is_some_and(|i| i.mode == "signed");
        if signed_only {
            draft.anonymous = false;
        }
        let my_id = self.library.my_id();
        let updating = draft
            .published
            .as_ref()
            .filter(|p| library::resolve_server(&p.server) == draft.server && p.author_id.is_some() && p.author_id == my_id)
            .and_then(|p| p.slug.clone())
            .filter(|_| !draft.anonymous && !draft.as_new);
        let scripts = self.library_scripts_on(&draft.server);
        let mut send = false;
        let response = egui::Modal::new(egui::Id::new("publish_song")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(if updating.is_some() { "Publish a new version of a song" } else { "Publish a song" });
            ui.weak(draft.file.display().to_string());
            if taking.is_empty() {
                ui.add_space(6.0);
                if asking {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Asking your library servers whether they take songs...");
                    });
                } else {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "None of your library servers take songs: each server's owner decides whether it does.",
                    );
                }
            }
            ui.add_space(6.0);
            egui::Grid::new("publish_song_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Composer");
                ui.add(egui::TextEdit::singleline(&mut draft.composer).hint_text("who wrote it, or the artist").desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Arranged by");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.arranger)
                        .hint_text("who made the MIDI file (you, if empty)")
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
                ui.label("From");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("publish_song_from").selected_text(songs::source_title(draft.from)).show_ui(
                        ui,
                        |ui| {
                            for (source, title) in songs::SOURCES {
                                ui.selectable_value(&mut draft.from, source, title);
                            }
                        },
                    );
                    if !matches!(draft.from, "original" | "classical" | "traditional") {
                        ui.add(egui::TextEdit::singleline(&mut draft.from_title).hint_text("which (the game, film, album)").desired_width(f32::INFINITY));
                    }
                });
                ui.end_row();
                ui.label("Your right to share it");
                ui.vertical(|ui| {
                    for (right, title) in songs::RIGHTS {
                        ui.radio_value(&mut draft.rights, right, title);
                    }
                    if draft.rights == "licensed" {
                        ui.add(egui::TextEdit::singleline(&mut draft.license).hint_text("the license: CC BY 4.0, CC0, ...").desired_width(f32::INFINITY));
                    }
                });
                ui.end_row();
                ui.label("Made for");
                let chosen = draft.made_for.as_ref().map_or("Nothing in particular", |(_, name)| name.as_str()).to_string();
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("publish_song_made_for").selected_text(chosen).show_ui(ui, |ui| {
                        ui.selectable_value(&mut draft.made_for, None, "Nothing in particular");
                        for (id, name) in &scripts {
                            ui.selectable_value(&mut draft.made_for, Some((id.clone(), name.clone())), name);
                        }
                    });
                    info_icon(ui).on_hover_text(
                        "A script on the same server it's meant to be played with (a game's level, say). The \
                         scripts you have from that server are listed.",
                    );
                });
                ui.end_row();
                ui.label("Tags");
                ui.add(egui::TextEdit::singleline(&mut draft.tags).hint_text("piano, chill").desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Post as");
                ui.add(egui::TextEdit::singleline(&mut draft.author_name).hint_text("a name").desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Server");
                egui::ComboBox::from_id_salt("publish_song_server").selected_text(host(&draft.server)).show_ui(ui, |ui| {
                    for s in &taking {
                        ui.selectable_value(&mut draft.server, s.clone(), host(s));
                    }
                });
                ui.end_row();
                if !draft.anonymous {
                    ui.label("Slug");
                    match &updating {
                        Some(slug) => {
                            ui.horizontal(|ui| {
                                ui.label(format!("{slug} (version after the last)"));
                                info_icon(ui).on_hover_text(
                                    "You published this song before, so it becomes its next version. Untick below \
                                     to publish it as a new song instead.",
                                );
                            });
                        }
                        None => {
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut draft.slug).desired_width(200.0));
                                info_icon(ui).on_hover_text(
                                    "Its name among your uploads: lowercase letters, digits and dashes. Publishing \
                                     again with the same slug adds a new version instead of a new song.",
                                );
                            });
                        }
                    }
                    ui.end_row();
                }
            });
            ui.label("Description");
            ui.add(egui::TextEdit::multiline(&mut draft.description).desired_rows(3).desired_width(f32::INFINITY));
            if draft
                .published
                .as_ref()
                .is_some_and(|p| library::resolve_server(&p.server) == draft.server && p.author_id.is_some() && p.author_id == my_id)
            {
                let mut as_update = !draft.as_new;
                if ui.checkbox(&mut as_update, "A new version of the one published before").changed() {
                    draft.as_new = !as_update;
                }
            }
            ui.add_enabled_ui(!signed_only, |ui| {
                ui.checkbox(&mut draft.anonymous, "Post anonymously")
                    .on_hover_text("Not signed with your identity: it can't be given new versions or deleted later.")
                    .on_disabled_hover_text("This server only takes signed uploads.");
            });
            ui.weak(
                "Only share songs you have the right to: your own, arrangements of public-domain works, or ones \
                 released for sharing. Someone else's song can be taken down by a copyright notice.",
            );
            if let Some(info) = &info {
                ui.weak(format!("Rules on {}: {}", host(&draft.server), info.rules));
            }
            if let Some(error) = &draft.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                close = ui.button("Cancel").clicked();
                let ready = !draft.busy && taking.contains(&draft.server);
                send = ui.add_enabled(ready, egui::Button::new(if draft.busy { "Publishing..." } else { "Publish" })).clicked();
            });
        });
        if close || (response.should_close() && !draft.busy) {
            return;
        }
        if send {
            self.send_song(&mut draft, updating);
        }
        self.song_library.publish = Some(draft);
    }

    /// Check the draft and upload it.
    fn send_song(&mut self, draft: &mut SongDraft, updating: Option<String>) {
        let bytes = match std::fs::read(&draft.file) {
            Ok(bytes) => bytes,
            Err(e) => {
                draft.error = Some(format!("Couldn't read it: {e}"));
                return;
            }
        };
        if let Err(problem) = songs::analyze(&bytes) {
            draft.error = Some(format!("Can't publish it: {problem}."));
            return;
        }
        let slug = if draft.anonymous { None } else { Some(updating.unwrap_or_else(|| draft.slug.trim().to_string())) };
        if slug.as_deref().is_some_and(|s| !library::is_slug(s)) {
            draft.error = Some("The slug can only have lowercase letters, digits and dashes (not at the ends).".into());
            return;
        }
        let upload = SongUpload {
            name: draft.name.trim().to_string(),
            composer: draft.composer.trim().to_string(),
            arranger: draft.arranger.trim().to_string(),
            from: draft.from.to_string(),
            from_title: draft.from_title.trim().to_string(),
            description: draft.description.trim().to_string(),
            tags: library::parse_tags(&draft.tags),
            author_name: draft.author_name.trim().to_string(),
            rights: draft.rights.to_string(),
            license: if draft.rights == "licensed" { draft.license.trim().to_string() } else { String::new() },
            made_for: draft.made_for.as_ref().map(|(id, _)| id.clone()),
            data: songs::encode(&bytes),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            slug,
        };
        if let Err(problem) = songs::check_upload(&upload) {
            draft.error = Some(format!("Can't publish it: {problem}."));
            return;
        }
        let identity = if draft.anonymous {
            None
        } else {
            match self.ensure_identity() {
                Ok(identity) => Some(identity),
                Err(e) => {
                    draft.error = Some(format!("Couldn't make your identity: {e}"));
                    return;
                }
            }
        };
        draft.error = None;
        draft.busy = true;
        if self.config.library.author_name != upload.author_name {
            self.config.library.author_name = upload.author_name.clone();
            let _ = self.config.save();
        }
        let (server, file) = (draft.server.clone(), draft.file.clone());
        let pinned = self.config.library.pinned.get(&server).cloned();
        self.spawn_songs(move || {
            let key = match pinned {
                Some(key) => Ok(key),
                None => client::info(&server).map(|i| i.key),
            };
            let result = key.clone().and_then(|key| client::upload_song(&server, &key, &upload, identity.as_ref()));
            Event::Published(server, key.unwrap_or_default(), file, Box::new(upload), result)
        });
    }

    /// An upload finished: on success, note where it went beside the file.
    fn song_published(&mut self, server: String, key: String, file: PathBuf, upload: SongUpload, result: Result<Receipt, String>) {
        if let Some(draft) = &mut self.song_library.publish {
            draft.busy = false;
        }
        if !key.is_empty() && !self.config.library.pinned.contains_key(&server) {
            self.config.library.pinned.insert(server.clone(), key);
            let _ = self.config.save();
        }
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(e) => {
                if let Some(draft) = &mut self.song_library.publish {
                    draft.error = Some(e);
                }
                return;
            }
        };
        let facts = songs::analyze(&songs::decode(&upload.data).unwrap_or_default()).ok();
        let source = SongSource {
            server: server.clone(),
            id: receipt.id.clone(),
            version: receipt.version,
            sha256: receipt.sha256.clone(),
            name: upload.name.clone(),
            author_name: upload.author_name.clone(),
            author_id: upload.slug.is_some().then(|| self.library.my_id()).flatten(),
            slug: upload.slug.clone(),
            from: upload.from.clone(),
            song: songs::SongInfo {
                composer: upload.composer.clone(),
                arranger: if upload.arranger.is_empty() { upload.author_name.clone() } else { upload.arranger.clone() },
                from_title: upload.from_title.clone(),
                copyright: facts.as_ref().and_then(|f| f.copyright.clone()),
                rights: upload.rights.clone(),
                license: upload.license.clone(),
                made_for: upload.made_for.clone(),
                length: facts.as_ref().map_or(0.0, |f| f.length),
                notes: facts.as_ref().map_or(0, |f| f.notes),
                tracks: facts.as_ref().map_or(0, |f| f.tracks),
                channels: facts.as_ref().map_or(0, |f| f.channels),
                preview_start: facts.as_ref().map_or(0.0, |f| f.preview_start),
            },
            receipt: Some(receipt.clone()),
        };
        if let Err(e) = source.write(&file) {
            log::warn!("couldn't note where {} was published: {e}", file.display());
        }
        log::info!("published song {} to {server} as {} v{}", file.display(), receipt.id, receipt.version);
        let said = if receipt.version > 1 {
            format!("Published version {} of \"{}\" to {}.", receipt.version, upload.name, host(&server))
        } else {
            format!("Published \"{}\" to {}.", upload.name, host(&server))
        };
        self.status = said.clone();
        if let Some(draft) = &mut self.song_library.publish {
            draft.done = Some((server, receipt.id, said));
        }
        self.library_search();
    }
}

/// A song's credits and facts, in its details. A click on the script it's
/// made for comes back as that script's ID.
pub(super) fn song_facts_ui(ui: &mut egui::Ui, song: &songs::SongInfo) -> Option<String> {
    let mut pick = None;
    egui::Grid::new("song_facts").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        ui.weak("Composer");
        ui.label(&song.composer);
        ui.end_row();
        ui.weak("Arranged by");
        ui.label(&song.arranger);
        ui.end_row();
        if !song.from_title.is_empty() {
            ui.weak("From");
            ui.label(&song.from_title);
            ui.end_row();
        }
        ui.weak("Rights");
        match song.license.trim() {
            "" => ui.label(songs::rights_title(&song.rights)),
            license => ui.label(format!("{} ({license})", songs::rights_title(&song.rights))),
        };
        ui.end_row();
        ui.weak("Length");
        ui.label(crate::song_info::format_length(song.length));
        ui.end_row();
        ui.weak("Notes");
        ui.label(format!(
            "{} in {} track{}, {} channel{}",
            song.notes,
            song.tracks,
            if song.tracks == 1 { "" } else { "s" },
            song.channels,
            if song.channels == 1 { "" } else { "s" }
        ));
        ui.end_row();
        if let Some(copyright) = &song.copyright {
            ui.weak("The file says");
            ui.label(copyright);
            ui.end_row();
        }
        if let Some(script) = &song.made_for {
            ui.weak("Made for");
            if ui.link("a script on this server").on_hover_text("Show the script it's meant to be played with").clicked() {
                pick = Some(script.clone());
            }
            ui.end_row();
        }
    });
    pick
}
