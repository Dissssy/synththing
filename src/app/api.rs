//! The local app API (`api.rs`) from the app's side: keeping the server
//! running as the preferences say, answering its calls between frames,
//! sending events while anything listens, and Preferences > Integrations.

use eframe::egui;

use super::App;
use crate::api::{self, Access, Call, NowPlaying, PlaylistInfo, PlaylistRef, SongInfo};
use crate::engine::EngineView;

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
    /// A token just made, shown until Preferences close: (app name, token).
    new_token: Option<(String, String)>,
    /// The name being typed for a new app.
    new_name: String,
}

/// What a `now_playing` event is sent for a change of.
#[derive(Clone, PartialEq)]
struct Seen {
    song: Option<String>,
    playlist: Option<String>,
    paused: bool,
    speed: f64,
    volume: f32,
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
        } else if self.api.server.as_ref().is_none_or(|s| s.addr().port() != port) && self.api.failed.as_ref().is_none_or(|(p, _)| *p != port) {
            self.api.server = None;
            match api::Server::start(port, settings.apps.clone(), ctx.clone()) {
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
        let Some(server) = &self.api.server else { return };
        server.set_apps(&self.config.api.apps);
        let calls: Vec<Call> = server.calls().collect();
        for call in calls {
            match call {
                Call::Command(command, reply) => {
                    let result = if self.command_available(command) {
                        self.run_command(ctx, command, view);
                        Ok(())
                    } else {
                        Err(format!("\"{}\" can't be done right now", command.name()))
                    };
                    let _ = reply.send(result);
                }
                Call::NowPlaying(reply) => {
                    let _ = reply.send(self.api_now_playing(view));
                }
                Call::Library(reply) => {
                    let _ = reply.send(api::Library { store: self.songs.clone(), playlists: self.api_playlists() });
                }
            }
        }
        self.api_events(view);
    }

    /// Send what changed since the last events, while a stream is open.
    fn api_events(&mut self, view: &EngineView) {
        if !self.api.server.as_ref().is_some_and(api::Server::listened_to) {
            self.api.seen = None;
            return;
        }
        let seen = Seen {
            song: self.current_song.as_deref().and_then(|p| self.songs.key(p)),
            playlist: self.api_playlist().map(|p| p.id),
            paused: view.paused,
            speed: view.speed,
            volume: self.volume,
            loads: view.loads,
        };
        let revision = self.songs.revision();
        let last = self.api.seen.replace((seen.clone(), view.generation, revision));
        let Some((last_seen, last_generation, last_revision)) = last else { return };
        let now = (seen != last_seen).then(|| self.api_now_playing(view));
        let Some(server) = &self.api.server else { return };
        match now {
            Some(now) => server.event("now_playing", &now),
            None if view.generation != last_generation => server.event("seeked", &serde_json::json!({ "position": view.position })),
            None => {}
        }
        if revision != last_revision {
            server.event("library_changed", &serde_json::json!({}));
        }
    }

    fn api_playlist(&self) -> Option<PlaylistRef> {
        let np = self.now_playing.as_ref()?;
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
        }
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
        ui.checkbox(&mut settings.on, "Let programs on this computer use the API");
        ui.horizontal(|ui| {
            ui.label("Port");
            let mut port = settings.port();
            ui.add(egui::DragValue::new(&mut port).range(1024..=65535));
            settings.port = Some(port).filter(|&p| p != api::DEFAULT_PORT);
        });
        match (&self.api.server, &self.api.failed) {
            (Some(server), _) => {
                ui.weak(format!("Listening on http://{}", server.addr()));
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
        let mut remove = None;
        for (i, app) in settings.apps.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(&app.name);
                ui.checkbox(&mut app.read, "Read").on_hover_text("See the library, what's playing, and events");
                ui.checkbox(&mut app.control, "Control").on_hover_text("Run commands: play, pause, next track...");
                if ui.button("Remove").on_hover_text("Its token stops working").clicked() {
                    remove = Some(i);
                }
            });
        }
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

        if self.config.api != before {
            if self.config.api.port() != before.port() {
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
