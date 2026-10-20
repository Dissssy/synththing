//! Settings sync in the app (`settings_sync.rs`): the switch in
//! Preferences > Online Library, and syncing with the authority on a
//! thread of its own: at launch, a moment after a synced setting is
//! saved, and when the notification stream says another device wrote.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::config::Config;
use crate::library::identity::{self, Identity};
use crate::library::{self, client};
use crate::settings_sync::{self, Change, Fields, State, Winner};

/// How long after a synced setting's saved before it's sent (a slider
/// being dragged saves over and over).
const SETTLE: Duration = Duration::from_secs(2);
/// How long after a sync that failed before the next try.
const RETRY: Duration = Duration::from_secs(60);
/// How long the debug toast shows.
const TOAST: Duration = Duration::from_secs(6);

pub struct SyncState {
    /// Sync is on here (`State::load` finds one), as of `for_identity`.
    on: bool,
    for_identity: Option<String>,
    /// The server's revision as of the last sync.
    revision: i64,
    /// A sync is running.
    running: bool,
    events: (Sender<Synced>, Receiver<Synced>),
    /// A synced setting was saved then: send it once it's settled.
    dirty_since: Option<Instant>,
    /// Sync as soon as it can (after this, for a retry).
    wanted: Option<Instant>,
    error: Option<String>,
    toast: Option<(String, Instant)>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            on: false,
            for_identity: None,
            revision: 0,
            running: false,
            events: mpsc::channel(),
            dirty_since: None,
            wanted: None,
            error: None,
            toast: None,
        }
    }
}

/// How a sync went.
type Synced = Result<Outcome, String>;

/// A sync, for its thread.
struct Job {
    server: String,
    server_key: String,
    identity: Identity,
    sync_key: [u8; 32],
    base: Fields,
    local: Fields,
    /// Just turned on: the server's settings win where it has them.
    adopt: bool,
}

/// What a sync did.
struct Outcome {
    /// The server's revision and settings now.
    revision: i64,
    merged: Fields,
    /// This device's settings the sync started from.
    local: Fields,
    changes: Vec<Change>,
}

/// Merge with the server's settings and write the result, until a write
/// isn't beaten by another device's.
fn sync(job: Job) -> Synced {
    for _ in 0..5 {
        let (revision, plain) =
            client::sync_get(&job.server, &job.server_key, &job.identity, &job.sync_key, settings_sync::NAME)?;
        let server: Option<Fields> = plain.and_then(|p| serde_json::from_slice(&p).ok());
        let (merged, changes) = match &server {
            Some(server) if job.adopt => {
                let mut merged = server.clone();
                for (path, field) in &job.local {
                    merged.entry(path.clone()).or_insert_with(|| field.clone());
                }
                let changes = merged
                    .iter()
                    .filter(|(path, field)| job.local.get(*path).map(|f| &f.value) != Some(&field.value))
                    .map(|(path, _)| Change { path: path.clone(), conflict: None })
                    .collect();
                (merged, changes)
            }
            Some(server) => settings_sync::merge(&job.base, &job.local, server),
            None => (job.local.clone(), Vec::new()),
        };
        if server.as_ref() == Some(&merged) {
            return Ok(Outcome { revision, merged, local: job.local, changes });
        }
        let plain = serde_json::to_vec(&merged).map_err(|e| e.to_string())?;
        let written =
            client::sync_write(&job.server, &job.server_key, &job.identity, &job.sync_key, settings_sync::NAME, revision, &plain)?;
        if written.stored {
            return Ok(Outcome { revision: written.revision, merged, local: job.local, changes });
        }
    }
    Err("other devices kept changing the settings: trying again in a minute".into())
}

fn describe(changes: &[Change]) -> String {
    let parts: Vec<String> = changes
        .iter()
        .map(|c| match c.conflict {
            None => c.path.clone(),
            Some(Winner::ThisDevice) => format!("{} (changed on both: this device's kept)", c.path),
            Some(Winner::Server) => format!("{} (changed on both: the other device's taken)", c.path),
        })
        .collect();
    format!("Settings sync changed: {}", parts.join(", "))
}

impl App {
    pub(super) fn sync_on(&self) -> bool {
        self.sync.on
    }

    /// The notification stream from `server` opened again (`None`), or said
    /// synced data changed: sync if it's the authority's and newer.
    pub(super) fn sync_server_seen(&mut self, server: &str, changed: Option<library::sync::Changed>) {
        if !self.sync.on || server != self.authority().0 {
            return;
        }
        match changed {
            Some(changed) if changed.name != settings_sync::NAME || changed.revision <= self.sync.revision => {}
            _ => self.sync.wanted = Some(Instant::now()),
        }
    }

    /// Each frame: start a sync when one's due, and take one that's done.
    pub(super) fn poll_sync(&mut self) {
        let identity = self.library_identity();
        let whose = identity.as_ref().map(Identity::public);
        if whose != self.sync.for_identity {
            // (Signed in, out, or as someone else: on only if the state's
            // for this sync key.)
            let state = State::load();
            self.sync.on = state.is_some() && identity.is_some();
            self.sync.revision = state.map_or(0, |s| s.revision);
            self.sync.wanted = self.sync.on.then(Instant::now);
            self.sync.for_identity = whose;
        }
        if settings_sync::take_dirty() && self.sync.on {
            self.sync.dirty_since = Some(Instant::now());
        }
        while let Ok(result) = self.sync.events.1.try_recv() {
            self.sync.running = false;
            match result {
                Ok(outcome) => self.synced(outcome),
                Err(e) => {
                    log::info!("settings sync: {e}");
                    self.sync.error = Some(e);
                    self.sync.wanted = Some(Instant::now() + RETRY);
                }
            }
        }
        let settled = self.sync.dirty_since.is_some_and(|t| t.elapsed() >= SETTLE);
        let due = self.sync.wanted.is_some_and(|t| Instant::now() >= t);
        if self.sync.on && !self.sync.running && (settled || due) {
            self.sync.dirty_since = None;
            self.sync.wanted = None;
            self.start_sync(false);
        }
    }

    fn start_sync(&mut self, adopt: bool) {
        let (server, server_key) = self.authority();
        let (Some(identity), Some(server_key)) = (self.library_identity(), server_key) else {
            self.sync.error = Some(format!("{} hasn't been reached yet: open the Online Library first.", super::library::host(&server)));
            return;
        };
        let (Some(sync_key), Some(state)) = (identity::sync_key(), State::load()) else {
            self.sync.on = false;
            return;
        };
        let job = Job { server, server_key, identity, sync_key, base: state.base, local: state.fields, adopt };
        self.sync.running = true;
        let tx = self.sync.events.0.clone();
        std::thread::Builder::new()
            .name("settings sync".into())
            .spawn(move || {
                let _ = tx.send(sync(job));
            })
            .ok();
    }

    /// A sync is done: take its settings, with whatever was changed here
    /// while it ran, and send those next.
    fn synced(&mut self, outcome: Outcome) {
        self.sync.error = None;
        // (Turned off meanwhile.)
        let Some(mut state) = State::load() else { return };
        let (result, _) = settings_sync::merge(&outcome.local, &state.fields, &outcome.merged);
        let mut json = match serde_json::to_value(&self.config) {
            Ok(json) => json,
            Err(e) => return log::warn!("settings sync: {e}"),
        };
        settings_sync::apply::<Config>(&mut json, &result);
        match serde_json::from_value::<Config>(json) {
            Ok(config) => {
                let theme_changed = config.theme != self.config.theme;
                self.config = config;
                if theme_changed {
                    self.reapply_theme();
                }
            }
            Err(e) => return log::warn!("settings sync: {e}"),
        }
        state.applied = settings_sync::synced_leaves(&serde_json::to_value(&self.config).unwrap_or_default());
        if result != outcome.merged {
            self.sync.dirty_since = Some(Instant::now());
        }
        state.revision = outcome.revision;
        state.base = outcome.merged;
        state.fields = result;
        self.sync.revision = state.revision;
        if let Err(e) = state.save().and_then(|()| self.config.save().map_err(|e| e.to_string())) {
            log::warn!("settings sync: {e}");
        }
        if self.config.sync_debug && !outcome.changes.is_empty() {
            self.sync.toast = Some((describe(&outcome.changes), Instant::now()));
        }
    }

    /// The switch, under the identity in Preferences > Online Library.
    pub(super) fn sync_ui(&mut self, ui: &mut egui::Ui) {
        let signed_in = self.library_identity().is_some();
        let (authority, _) = self.authority();
        let offered = self.library_server_info(&authority).is_none_or(|i| i.sync);
        let mut on = self.sync.on;
        super::with_info(
            ui,
            "Keeps your settings the same on every device signed in as you that has this on. What \
             belongs to one device stays on it: folders, layouts, the audio buffer, the mobile layout, \
             the interface scale and updates. They're encrypted on this device first, with a key only \
             your devices have, so the server can't read them. Turning it on here takes the settings \
             your other devices synced, if there are any.",
            |ui| ui.add_enabled(signed_in && offered, egui::Checkbox::new(&mut on, "Sync settings between my devices")),
        );
        if !offered {
            ui.weak(format!("{} doesn't sync settings yet.", super::library::host(&authority)));
        }
        if on != self.sync.on {
            if on {
                self.turn_sync_on();
            } else {
                settings_sync::turn_off();
                self.sync.on = false;
                self.sync.error = None;
            }
        }
        if self.sync.on {
            if let Some(error) = &self.sync.error {
                ui.weak(format!("Couldn't sync: {error}"));
            }
            let mut debug = self.config.sync_debug;
            if ui.checkbox(&mut debug, "Show what each sync changed (for testing)").changed() {
                self.config.sync_debug = debug;
                if let Err(e) = self.config.save() {
                    log::warn!("saving the config: {e}");
                }
            }
        }
    }

    fn turn_sync_on(&mut self) {
        let made = identity::ensure_sync_key().and_then(|sync_key| {
            let json = serde_json::to_value(&self.config).map_err(|e| e.to_string())?;
            State::new(&sync_key, &json).save()
        });
        match made {
            Ok(()) => {
                self.sync.on = true;
                self.sync.revision = 0;
                self.sync.error = None;
                self.start_sync(true);
            }
            Err(e) => self.sync.error = Some(e),
        }
    }

    /// The debug toast: what the last sync changed.
    pub(super) fn sync_toast_ui(&mut self, ctx: &egui::Context) {
        let Some((text, since)) = &self.sync.toast else { return };
        if since.elapsed() > TOAST {
            self.sync.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let width = (ctx.content_rect().width() - 32.0).min(480.0);
        let clicked = egui::Area::new(egui::Id::new("sync_toast"))
            .order(egui::Order::Tooltip)
            .sense(egui::Sense::click())
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 48.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(width);
                    ui.add(egui::Label::new(text.as_str()).wrap());
                });
            })
            .response
            .clicked();
        if clicked {
            self.sync.toast = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A device's sync state after a sync, as `App::synced` leaves it.
    fn after(mut state: State, outcome: &Outcome) -> State {
        state.revision = outcome.revision;
        state.base = outcome.merged.clone();
        state.fields = outcome.merged.clone();
        state.applied = outcome.merged.iter().map(|(k, f)| (k.clone(), f.value.clone())).collect();
        state
    }

    fn job(base: &str, key: &str, identity: &Identity, state: &State, adopt: bool) -> Job {
        Job {
            server: base.into(),
            server_key: key.into(),
            identity: identity.clone(),
            sync_key: [4; 32],
            base: state.base.clone(),
            local: state.fields.clone(),
            adopt,
        }
    }

    #[test]
    fn two_devices() {
        let dir = std::env::temp_dir().join(format!("synththing-settings-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false, "check_uploads": false, "sync": true }"#).unwrap();
        let running = library::server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = client::info(&base).unwrap().key;
        let me = Identity::generate().unwrap();

        // The desktop turns it on first: its settings go up.
        let desktop = State::new(&[4; 32], &json!({ "theme": "dark", "shuffle": false, "editor_font_size": 14, "buffer_ms": 40 }));
        let up = sync(job(&base, &key, &me, &desktop, true)).unwrap();
        assert_eq!(up.revision, 1);
        assert!(up.changes.is_empty());
        let mut desktop = after(desktop, &up);
        assert!(!desktop.fields.contains_key("buffer_ms"), "the device's own");

        // The phone turns it on: it takes the desktop's.
        let phone = State::new(&[4; 32], &json!({ "theme": "light", "shuffle": true, "mini_player": true }));
        let down = sync(job(&base, &key, &me, &phone, true)).unwrap();
        assert_eq!(down.merged["theme"].value, json!("dark"));
        assert_eq!(down.merged["mini_player"].value, json!(true), "the phone's own setting the server lacked goes up");
        assert_eq!(down.revision, 2);
        let mut phone = after(phone, &down);

        // Each changes something (the phone offline); both sync: both have both.
        let now = settings_sync::now();
        desktop.record(&json!({ "theme": "dark", "shuffle": true, "editor_font_size": 16, "buffer_ms": 40 }), now + 5);
        phone.record(&json!({ "theme": "light", "shuffle": false, "editor_font_size": 18, "mini_player": true }), now + 10);
        let first = sync(job(&base, &key, &me, &desktop, false)).unwrap();
        let second = sync(job(&base, &key, &me, &phone, false)).unwrap();
        assert_eq!(second.merged["theme"].value, json!("light"), "only the phone changed it");
        assert_eq!(second.merged["shuffle"].value, json!(true), "only the desktop changed it");
        assert_eq!(second.merged["editor_font_size"].value, json!(18), "changed on both: the newer change");
        assert_eq!(
            second.changes.iter().map(|c| (c.path.as_str(), c.conflict)).collect::<Vec<_>>(),
            [("editor_font_size", Some(Winner::ThisDevice)), ("shuffle", None)]
        );
        let desktop = after(desktop, &first);
        let again = sync(job(&base, &key, &me, &desktop, false)).unwrap();
        assert_eq!(again.merged, second.merged);
        assert_eq!(again.changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["editor_font_size", "theme"]);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
