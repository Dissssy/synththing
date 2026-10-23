//! The local app API (`api.rs`) from the app's side, and the system's media
//! controls (`media_controls.rs`), another way into the same core: keeping
//! each running as the preferences say, carrying out what they ask between
//! frames, telling them what changed, and Preferences > Integrations.

use eframe::egui;

use super::App;
use crate::api::{self, Access, Call, NowPlaying, PlaylistInfo, PlaylistRef, SongInfo};
use crate::commands::Command;
use crate::engine::EngineView;

/// How soon after a call a frame looks at what it changed.
const CALL_SETTLES: std::time::Duration = std::time::Duration::from_millis(30);

/// The API's state in the app.
#[derive(Default)]
pub(super) struct ApiState {
    server: Option<api::Server>,
    /// The port it couldn't listen on, and why (not tried again until the
    /// preferences change).
    failed: Option<(u16, String)>,
    /// What the last events told of: (what's playing, how far it had
    /// jumped, the library's revision).
    seen: Option<(Seen, u64, u64)>,
    /// What the playlists were when the last event told of them (a hash
    /// of each one's ID, name and songs), while a stream is open.
    playlists_seen: Option<u64>,
    /// What the play queue was when the last event told of it (a hash),
    /// while a stream is open.
    queue_seen: Option<u64>,
    /// The Soundfonts list and which is in use, as last seen.
    soundfonts_seen: Option<u64>,
    /// A token just made, shown until Preferences close: (app name, token).
    new_token: Option<(String, String)>,
    /// The name being typed for a new app.
    new_name: String,
    /// The main window's handle (an HWND on Windows), for the media
    /// controls.
    pub(super) window: Option<usize>,
    #[cfg(not(target_os = "android"))]
    media: Option<crate::media_controls::MediaControls>,
    /// Why the media controls couldn't start (not tried again this run).
    media_failed: Option<String>,
}

/// What a `now_playing` event is sent for a change of.
#[derive(Clone, PartialEq)]
struct Seen {
    /// Whether a song's loaded: which one is told by `loads`, which only
    /// moves once it has (the song before its length and position are in
    /// would be sent with the last one's).
    has_song: bool,
    playlist: Option<String>,
    paused: bool,
    speed: f64,
    volume: f32,
    modes: (bool, crate::playlist::LoopMode, crate::song_list::Autoplay),
    soundfont: Option<String>,
    loads: u64,
}

impl App {
    /// Every frame (`tick`): the server as the preferences say, its calls
    /// answered, events sent.
    pub(super) fn api_tick(&mut self, ctx: &egui::Context, view: &EngineView) {
        let settings = &self.config.api;
        let port = settings.port();
        let wanted = settings.on && !cfg!(target_os = "android");
        if !wanted {
            self.api.server = None;
            self.api.failed = None;
        } else if self.api.server.as_ref().is_none_or(|s| s.addr().port() != port || s.addr().ip().is_loopback() == settings.lan)
            && self.api.failed.as_ref().is_none_or(|(p, _)| *p != port)
        {
            self.api.server = None;
            match api::Server::start(port, settings.lan, settings.apps.clone(), ctx.clone()) {
                Ok(server) => {
                    log::info!("api: listening on {}", server.addr());
                    self.api.server = Some(server);
                    self.api.failed = None;
                }
                Err(e) => {
                    log::warn!("api: {e}");
                    self.api.failed = Some((port, e));
                }
            }
        }
        #[cfg(not(target_os = "android"))]
        self.media_tick(ctx, view);
        self.integration_changes(view);
        let Some(server) = &self.api.server else { return };
        server.set_apps(&self.config.api.apps);
        let calls: Vec<Call> = server.calls().collect();
        // (What a call changed lands in the engine's view a little later:
        // a frame then tells of it, even with nothing being drawn, before
        // another call can change it again.)
        if !calls.is_empty() {
            ctx.request_repaint_after(CALL_SETTLES);
        }
        for call in calls {
            match call {
                Call::Command(command, reply) => {
                    let _ = reply.send(self.api_run(ctx, command, view));
                }
                Call::NowPlaying(reply) => {
                    let _ = reply.send(self.api_now_playing(view));
                }
                Call::Edit(edit, reply) => {
                    let _ = reply.send(self.api_edit(edit));
                }
                Call::Set(set, reply) => {
                    let _ = reply.send(self.api_set(set, view));
                }
                Call::Queue(edit, reply) => {
                    let _ = reply.send(self.api_queue(edit));
                }
                Call::UseSoundfont(id, reply) => {
                    let _ = reply.send(self.api_use_soundfont(&id));
                }
                Call::Play { song, playlist, reply } => {
                    let _ = reply.send(self.api_play(song.as_deref(), playlist.as_deref()));
                }
                Call::Add { stored, program, reply } => {
                    let _ = reply.send(self.api_add(stored, &program));
                }
                Call::Library(reply) => {
                    let _ = reply.send(api::Library {
                        store: self.songs.clone(),
                        playlists: self.api_playlists(),
                        rulesets: self.all_rulesets(),
                        group_ignore: self.config.group_ignore.clone(),
                        articles: self.config.sort_with_articles,
                        online: self.config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup),
                        queue: self.queue.iter().filter_map(|path| self.songs.key(path)).collect(),
                        soundfonts: self.api_soundfonts(),
                    });
                }
            }
        }
    }

    /// A song `program` sent, stored already, into the library: marked as
    /// the program's if it's new there.
    fn api_add(&mut self, stored: crate::song_store::Stored, program: &str) -> Result<(SongInfo, bool), String> {
        // (The one-time import writes the library's index last: until
        // then, nothing else may.)
        if self.old_import.is_some() {
            return Err("synththing is still setting up its library: try again in a moment".into());
        }
        let key = stored.key.clone();
        let new = self.songs.get_by_key(&key).is_none();
        self.add_to_library([stored]);
        if new {
            self.songs.set_added_by(&key, program);
        }
        let song = self.songs.get_by_key(&key).ok_or("the song didn't stay in the library")?;
        Ok((SongInfo::of(&key, song, &self.songs), new))
    }

    /// `POST /v1/play`: playlist `playlist` (by its ID) from `song` or
    /// from the top, or `song` (by its key) on its own, leaving any
    /// playlist.
    fn api_play(&mut self, song: Option<&str>, playlist: Option<&str>) -> Result<(), (u16, String)> {
        let missing_song = || (404, "there's no song with that key in the library".to_string());
        let Some(id) = playlist else {
            let key = song.filter(|key| self.songs.has(key)).ok_or_else(missing_song)?;
            self.play_path(self.songs.path(key));
            return Ok(());
        };
        let list = self.playlists.lists.iter().position(|l| l.id == id).ok_or((404, "there's no playlist with that ID".to_string()))?;
        let Some(key) = song else {
            if self.playlists.lists[list].playlist.entries.is_empty() {
                return Err((409, "that playlist is empty".into()));
            }
            self.play_playlist(list);
            return Ok(());
        };
        let dir = self.songs.dir();
        let entry = self.playlists.lists[list]
            .playlist
            .entries
            .iter()
            .position(|e| crate::song_store::key_in(dir, &e.path) == Some(key))
            .ok_or((404, "that song isn't in that playlist".to_string()))?;
        self.play_entry(list, entry);
        Ok(())
    }

    /// A change to the play queue a program asked for.
    fn api_queue(&mut self, edit: api::QueueEdit) -> Result<(), (u16, String)> {
        match edit {
            api::QueueEdit::Add { song: Some(key), next, .. } => {
                if !self.songs.has(&key) {
                    return Err((404, "there's no song with that key in the library".into()));
                }
                self.enqueue(vec![self.songs.path(&key)], next);
            }
            api::QueueEdit::Add { playlist: Some(id), next, .. } => {
                let list = self.playlists.lists.iter().find(|l| l.id == id).ok_or((404, "there's no playlist with that ID".to_string()))?;
                let songs: Vec<_> = list.playlist.entries.iter().filter(|e| !e.missing).map(|e| e.path.clone()).collect();
                if songs.is_empty() {
                    return Err((409, "that playlist has no songs here".into()));
                }
                self.enqueue(songs, next);
            }
            api::QueueEdit::Add { .. } => return Err((400, "say what to queue".into())),
            api::QueueEdit::Remove(at) => {
                if at >= self.queue.len() {
                    return Err((404, format!("the queue has {} songs", self.queue.len())));
                }
                self.queue.remove(at);
            }
            api::QueueEdit::Clear => self.queue.clear(),
        }
        Ok(())
    }

    /// A change to the library a program asked for (its own permission:
    /// `Access::edit_library`).
    fn api_edit(&mut self, edit: api::Edit) -> Result<(), (u16, String)> {
        let song =
            |app: &App, key: &str| app.songs.has(key).then(|| app.songs.path(key)).ok_or((404, "there's no song with that key in the library".to_string()));
        match edit {
            api::Edit::Star { key, on } => {
                let path = song(self, &key)?;
                self.tag_songs(super::tags::TagAction::Set(crate::song_store::STARRED.into(), on), &[path]);
            }
            api::Edit::Tag { key, tag, on } => {
                let path = song(self, &key)?;
                let id = self.songs.tags().iter().find(|(_, t)| t.name.eq_ignore_ascii_case(&tag)).map(|(id, _)| id.clone());
                let id = id.ok_or_else(|| (404, format!("there's no tag \"{tag}\" (make it in synththing first)")))?;
                self.tag_songs(super::tags::TagAction::Set(id, on), &[path]);
            }
            api::Edit::Playlist { playlist, key, on } => {
                let list = self.playlists.lists.iter().position(|l| l.id == playlist).ok_or((404, "there's no playlist with that ID".to_string()))?;
                if self.playlists.lists[list].playlist.smart.is_some() {
                    return Err((409, "that's a smart playlist: its rules pick its songs".into()));
                }
                let path = song(self, &key)?;
                if on {
                    let end = self.playlists.lists[list].playlist.entries.len();
                    self.insert_songs(list, end, vec![path]);
                } else {
                    let dir = self.songs.dir();
                    let at: Vec<usize> = self.playlists.lists[list]
                        .playlist
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| crate::song_store::key_in(dir, &e.path) == Some(key.as_str()))
                        .map(|(i, _)| i)
                        .collect();
                    if at.is_empty() {
                        return Err((404, "that song isn't in that playlist".into()));
                    }
                    self.remove_entries(list, &at);
                }
            }
        }
        Ok(())
    }

    /// `POST /v1/seek` and `/v1/volume`.
    fn api_set(&mut self, set: api::Set, view: &EngineView) -> Result<(), (u16, String)> {
        use crate::audio::AudioCommand;
        match set {
            api::Set::Position(_) if self.current_song.is_none() => return Err((409, "nothing's playing".into())),
            api::Set::Position(to) => self.send(AudioCommand::Seek(if view.length > 0.0 { to.min(view.length) } else { to })),
            api::Set::Volume(to) => {
                self.volume = to;
                self.send_volume();
            }
            api::Set::Modes { shuffle, loop_mode, autoplay } => {
                if let Some(on) = shuffle {
                    self.set_shuffle(on);
                }
                if let Some(mode) = loop_mode {
                    self.set_loop_mode(mode);
                }
                if let Some(mode) = autoplay.filter(|&mode| mode != self.autoplay) {
                    self.set_autoplay(mode);
                }
            }
        }
        Ok(())
    }

    /// Run `command` if it can run now.
    fn api_run(&mut self, ctx: &egui::Context, command: Command, view: &EngineView) -> Result<(), String> {
        if !self.command_available(command) {
            return Err(format!("\"{}\" can't be done right now", command.name()));
        }
        self.run_command(ctx, command, view);
        Ok(())
    }

    /// The media controls as the preferences say, and what was pressed in
    /// them carried out.
    #[cfg(not(target_os = "android"))]
    fn media_tick(&mut self, ctx: &egui::Context, view: &EngineView) {
        use crate::audio::AudioCommand;
        use crate::media_controls::{Control, MediaControls};
        if !self.config.api.media_controls() {
            self.api.media = None;
        } else if self.api.media.is_none() && self.api.media_failed.is_none() {
            match MediaControls::new(self.api.window, ctx.clone()) {
                Ok(media) => {
                    self.api.media = Some(media);
                    // (Shown from scratch.)
                    self.api.seen = None;
                }
                Err(e) => {
                    log::warn!("media controls: {e}");
                    self.api.media_failed = Some(e);
                }
            }
        }
        let Some(media) = &self.api.media else { return };
        let controls: Vec<Control> = media.controls().collect();
        for control in controls {
            let command = match control {
                Control::Play => Command::Play,
                Control::Pause => Command::Pause,
                Control::Toggle => Command::TogglePause,
                Control::Next => Command::NextTrack,
                Control::Previous => Command::PreviousTrack,
                Control::Quit => Command::Quit,
                Control::Seek { seconds, relative } => {
                    self.send(if relative { AudioCommand::SeekRelative(seconds) } else { AudioCommand::Seek(seconds) });
                    continue;
                }
                Control::Volume(volume) => {
                    self.volume = volume;
                    self.send_volume();
                    continue;
                }
                Control::ShowWindow => {
                    self.show_window(ctx);
                    continue;
                }
            };
            let _ = self.api_run(ctx, command, view);
        }
    }

    /// Tell the event streams and the media controls what changed.
    fn integration_changes(&mut self, view: &EngineView) {
        let streams = self.api.server.as_ref().is_some_and(api::Server::listened_to);
        #[cfg(not(target_os = "android"))]
        let media = self.api.media.is_some();
        #[cfg(target_os = "android")]
        let media = false;
        if !streams && !media {
            self.api.seen = None;
            return;
        }
        self.api_events(view, streams);
    }

    /// Send what changed since last time: as events while a stream is
    /// open (`streams`), and to the media controls.
    fn api_events(&mut self, view: &EngineView, streams: bool) {
        let seen = Seen {
            has_song: self.current_song.is_some(),
            playlist: self.api_playlist().map(|p| p.id),
            paused: view.paused,
            speed: view.speed,
            volume: self.volume,
            modes: (self.shuffle, self.loop_mode, self.autoplay),
            soundfont: self.api_soundfont().map(|s| s.id),
            loads: view.loads,
        };
        let revision = self.songs.revision();
        let last = self.api.seen.replace((seen.clone(), view.generation, revision));
        let changed = last.as_ref().is_none_or(|(s, g, _)| *s != seen || *g != view.generation);
        #[cfg(not(target_os = "android"))]
        if changed && self.api.media.is_some() {
            let shown = self.media_shown(view);
            if let Some(media) = &mut self.api.media {
                media.show(&shown);
            }
        }
        let Some((last_seen, last_generation, last_revision)) = last else { return };
        let now = (seen != last_seen).then(|| self.api_now_playing(view));
        if !streams {
            // (Worked out afresh once one opens.)
            self.api.playlists_seen = None;
            self.api.queue_seen = None;
            self.api.soundfonts_seen = None;
        }
        let Some(server) = self.api.server.as_ref().filter(|_| streams) else { return };
        match now {
            Some(now) => server.event("now_playing", &now),
            None if view.generation != last_generation => server.event("seeked", &serde_json::json!({ "position": view.position })),
            None => {}
        }
        if revision != last_revision {
            server.event("library_changed", &serde_json::json!({}));
        }
        let playlists = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for list in &self.playlists.lists {
                (&list.id, &list.playlist.name).hash(&mut hasher);
                list.playlist.entries.iter().for_each(|e| e.path.hash(&mut hasher));
            }
            hasher.finish()
        };
        if self.api.playlists_seen.replace(playlists).is_some_and(|seen| seen != playlists) {
            server.event("playlists_changed", &serde_json::json!({}));
        }
        let queue = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.queue.hash(&mut hasher);
            hasher.finish()
        };
        if self.api.queue_seen.replace(queue).is_some_and(|seen| seen != queue) {
            server.event("queue_changed", &serde_json::json!({}));
        }
        let soundfonts = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (&self.config.soundfonts, self.active_sf).hash(&mut hasher);
            hasher.finish()
        };
        if self.api.soundfonts_seen.replace(soundfonts).is_some_and(|seen| seen != soundfonts) {
            server.event("soundfonts_changed", &serde_json::json!({}));
        }
    }

    /// What the media controls show.
    #[cfg(not(target_os = "android"))]
    fn media_shown(&mut self, view: &EngineView) -> crate::media_controls::Shown {
        let title = self.current_title();
        let facts = self.playing_facts();
        let text = |field| facts.as_ref().and_then(|f| f.text(field)).map(|t| t.into_owned());
        crate::media_controls::Shown {
            artist: text(crate::fields::Field::Artist),
            album: text(crate::fields::Field::Album),
            title,
            length: view.length,
            position: view.position,
            playing: !view.paused && !view.finished,
            loaded: self.current_song.is_some() && (view.has_midi || view.has_audio_file),
            volume: self.volume,
        }
    }

    fn api_playlist(&self) -> Option<PlaylistRef> {
        let np = self.playing_entry()?;
        let list = self.playlists.lists.get(np.list)?;
        Some(PlaylistRef { id: list.id.clone(), name: list.playlist.name.clone() })
    }

    fn api_now_playing(&self, view: &EngineView) -> NowPlaying {
        let song = self.current_song.as_deref().and_then(|path| {
            let key = self.songs.key(path)?;
            Some(SongInfo::of(&key, self.songs.get(path)?, &self.songs))
        });
        NowPlaying {
            song,
            playlist: self.api_playlist(),
            paused: view.paused,
            position: view.position,
            length: view.length,
            speed: view.speed,
            volume: self.volume,
            shuffle: self.shuffle,
            loop_mode: api::loop_name(self.loop_mode),
            autoplay: api::autoplay_name(self.autoplay),
            soundfont: self.api_soundfont(),
        }
    }

    /// The soundfont the MIDI song playing plays with.
    fn api_soundfont(&self) -> Option<PlaylistRef> {
        self.current_song.as_deref().filter(|path| super::is_midi_path(path))?;
        let path = self.loaded_sf.as_deref()?;
        Some(PlaylistRef { id: api::soundfont_id(path), name: crate::config::nice_name(path) })
    }

    fn api_soundfonts(&self) -> Vec<api::SoundfontInfo> {
        self.config
            .soundfonts
            .iter()
            .enumerate()
            .map(|(i, path)| api::SoundfontInfo {
                id: api::soundfont_id(path),
                name: crate::config::nice_name(path),
                size: std::fs::metadata(path).ok().map(|m| m.len()),
                active: self.active_sf == Some(i),
            })
            .collect()
    }

    fn api_use_soundfont(&mut self, id: &str) -> Result<(), (u16, String)> {
        let i = self.config.soundfonts.iter().position(|path| api::soundfont_id(path) == id).ok_or((404, "there's no such soundfont".to_string()))?;
        if !self.config.soundfonts[i].exists() {
            return Err((409, "its file is missing".into()));
        }
        self.activate_soundfont(i);
        Ok(())
    }

    fn api_playlists(&self) -> Vec<PlaylistInfo> {
        let dir = self.songs.dir();
        self.playlists
            .lists
            .iter()
            .map(|list| PlaylistInfo {
                id: list.id.clone(),
                name: list.playlist.name.clone(),
                smart: list.playlist.smart.is_some(),
                songs: list.playlist.entries.iter().filter_map(|e| crate::song_store::key_in(dir, &e.path).map(str::to_string)).collect(),
            })
            .collect()
    }

    /// Preferences > Integrations.
    pub(super) fn api_preferences_ui(&mut self, ui: &mut egui::Ui) {
        let before = self.config.api.clone();
        let settings = &mut self.config.api;
        ui.label(
            "Programs you run on this computer can use synththing's API: see what's playing, browse the \
             library, follow events and run commands. Each program gets its own token.",
        );
        ui.hyperlink_to("How to use it (docs/API.md)", "https://github.com/Dissssy/synththing/blob/master/docs/API.md");
        ui.add_space(4.0);
        let mut media = settings.media_controls();
        ui.checkbox(&mut media, "Show synththing in the system's media controls")
            .on_hover_text("Media keys, the volume overlay on Windows, Now Playing on macOS, MPRIS on Linux");
        settings.media_controls = (!media).then_some(false);
        if let Some(why) = self.api.media_failed.as_ref().filter(|_| media) {
            ui.colored_label(ui.visuals().error_fg_color, format!("They couldn't be reached: {why}"));
        }
        ui.add_space(4.0);
        ui.checkbox(&mut settings.on, "Let programs on this computer use the API");
        ui.checkbox(&mut settings.lan, "Let devices on the local network in too").on_hover_text(
            "Other computers and phones on your network can use it with a program's token. Tokens and everything else go unencrypted, \
             so only turn this on in a network you trust",
        );
        if settings.lan {
            ui.colored_label(ui.visuals().warn_fg_color, "Anyone on this network who gets a token can use it, and traffic isn't encrypted.");
        }
        ui.horizontal(|ui| {
            ui.label("Port");
            let mut port = settings.port();
            ui.add(egui::DragValue::new(&mut port).range(1024..=65535));
            settings.port = Some(port).filter(|&p| p != api::DEFAULT_PORT);
        });
        match (&self.api.server, &self.api.failed) {
            (Some(server), _) => {
                let addr = server.addr();
                if addr.ip().is_loopback() {
                    ui.weak(format!("Listening on http://{addr}"));
                } else {
                    ui.weak(format!("Listening on http://localhost:{0} here, and on port {0} of this computer's network address", addr.port()));
                }
            }
            (None, Some((_, why))) if settings.on => {
                ui.colored_label(ui.visuals().error_fg_color, why);
            }
            _ => {}
        }

        ui.separator();
        ui.strong("Programs");
        if settings.apps.is_empty() {
            ui.weak("None yet. Add one below to get its token.");
        }
        // (Each program's songs, by its name: what it added.)
        let mut added: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
        for (key, song) in self.songs.known() {
            if let Some(by) = &song.added_by {
                added.entry(by.as_str()).or_default().push(key);
            }
        }
        let mut remove = None;
        let mut remove_songs = None;
        if !settings.apps.is_empty() {
            // (Programs down the side, what each may do across the top.)
            let permissions: [(&str, &str, fn(&mut Access) -> &mut bool); 5] = [
                ("eye", "Read: see the library, what's playing, and events", |a| &mut a.read),
                ("play-pause", "Control: run commands (play, pause, next track...)", |a| &mut a.control),
                ("pencil-simple", "Edit library: star songs, tag them, and add songs to playlists or take them out", |a| &mut a.edit_library),
                ("download-simple", "Download songs: copy songs' files out of your library", |a| &mut a.download_songs),
                (
                    "music-notes-plus",
                    "Add songs: send songs into your library. Each one is marked as this program's (search addedby:name)",
                    |a| &mut a.add_songs,
                ),
            ];
            // (Every cell the same small box with its contents in the
            // middle, so the columns line up and stay narrow.)
            let cell = egui::vec2(28.0, 24.0);
            let any_added = settings.apps.iter().any(|app| added.contains_key(app.name.as_str()));
            egui::Grid::new("api_programs").striped(true).spacing([6.0, 4.0]).min_col_width(cell.x).show(ui, |ui| {
                ui.label("");
                for (name, info, _) in &permissions {
                    let (rect, response) = ui.allocate_exact_size(cell, egui::Sense::hover());
                    let color = ui.visuals().text_color();
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, super::icon(name), egui::FontId::proportional(16.0), color);
                    response.on_hover_text(*info);
                }
                ui.end_row();
                for (i, app) in settings.apps.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(&app.name);
                    });
                    for (_, info, field) in &permissions {
                        ui.add_sized(cell, egui::Checkbox::without_text(field(app))).on_hover_text(*info);
                    }
                    let trash = egui::Button::new(egui::RichText::new(super::icon("trash")).size(16.0)).frame(false);
                    if ui.add_sized(cell, trash).on_hover_text("Remove this program: its token stops working. The songs it added stay").clicked() {
                        remove = Some(i);
                    }
                    if any_added {
                        if let Some(keys) = added.get(app.name.as_str()) {
                            let plural = if keys.len() == 1 { "" } else { "s" };
                            let hover = format!("Remove the {} song{plural} {} added...", keys.len(), app.name);
                            let broom = egui::Button::new(egui::RichText::new(super::icon("broom")).size(16.0)).frame(false);
                            if ui.add_sized(cell, broom).on_hover_text(hover).clicked() {
                                remove_songs = Some(keys.clone());
                            }
                        } else {
                            ui.allocate_exact_size(cell, egui::Sense::hover());
                        }
                    }
                    ui.end_row();
                }
            });
        }
        let remove_songs: Option<Vec<std::path::PathBuf>> = remove_songs.map(|keys| keys.iter().map(|k| self.songs.path(k)).collect());
        if let Some(i) = remove {
            settings.apps.remove(i);
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.api.new_name).hint_text("Program name").desired_width(160.0));
            let name = self.api.new_name.trim().to_string();
            if ui.add_enabled(!name.is_empty(), egui::Button::new("Add")).clicked() {
                let (access, token) = Access::new(&name);
                settings.apps.push(access);
                self.api.new_token = Some((name, token));
                self.api.new_name.clear();
            }
        });
        if let Some((name, token)) = &self.api.new_token {
            ui.label(format!("{name}'s token. Copy it now: it isn't shown again."));
            ui.horizontal(|ui| {
                let mut shown = token.as_str();
                ui.add(egui::TextEdit::singleline(&mut shown).font(egui::TextStyle::Monospace).desired_width(260.0));
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(token.clone());
                }
            });
        }

        // (The usual Remove dialog, over the library, in place of
        // Preferences.)
        if let Some(paths) = remove_songs {
            self.remove_songs = paths;
            self.preferences_open = false;
            self.api_preferences_closed();
        }
        if self.config.api != before {
            if self.config.api.port() != before.port() || self.config.api.lan != before.lan {
                self.api.failed = None;
            }
            self.save_config();
        }
    }

    /// Preferences closed: a new token isn't shown again.
    pub(super) fn api_preferences_closed(&mut self) {
        self.api.new_token = None;
    }
}
