//! Sync in the app (`settings_sync.rs`, `playlist_sync.rs`): the switch in
//! Preferences > Online Library, and syncing each blob (the settings, the
//! playlists) with the authority on a thread of its own: at launch, a
//! moment after it changes here, and when the notification stream says
//! another device wrote.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::config::Config;
use crate::library::identity::{self, Identity};
use crate::library::{self, client};
use crate::playlist_sync;
use crate::settings_sync::{self, Change, Fields, State, Winner};

/// How long after a change before it's sent (a slider being dragged
/// saves over and over).
const SETTLE: Duration = Duration::from_secs(2);
/// How long after a sync that failed before the next try.
const RETRY: Duration = Duration::from_secs(60);
/// How long the debug toast shows.
const TOAST: Duration = Duration::from_secs(6);

/// What's synced, each its own blob on the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blob {
    Settings,
    Playlists,
}

impl Blob {
    const ALL: [Blob; 2] = [Blob::Settings, Blob::Playlists];

    fn name(self) -> &'static str {
        match self {
            Blob::Settings => settings_sync::NAME,
            Blob::Playlists => playlist_sync::NAME,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// One blob's syncing.
#[derive(Default)]
struct Channel {
    /// The server's revision as of the last sync.
    revision: i64,
    /// A sync is running.
    running: bool,
    /// It changed here then: send it once it's settled.
    dirty_since: Option<Instant>,
    /// Sync as soon as it can (after this, for a retry).
    wanted: Option<Instant>,
}

pub struct SyncState {
    /// Sync is on here (the settings' state loads), as of `for_identity`.
    on: bool,
    for_identity: Option<String>,
    /// The kinds of settings this device syncs (`State::choices`).
    choices: settings_sync::Choices,
    channels: [Channel; 2],
    events: (Sender<Done>, Receiver<Done>),
    error: Option<String>,
    toast: Option<(String, Instant)>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            on: false,
            for_identity: None,
            choices: Default::default(),
            channels: Default::default(),
            events: mpsc::channel(),
            error: None,
            toast: None,
        }
    }
}

/// How a sync went.
type Synced = Result<Outcome, String>;
/// A blob's sync, done.
type Done = (Blob, Synced);

/// A sync, for its thread.
struct Job {
    server: String,
    server_key: String,
    identity: Identity,
    sync_key: [u8; 32],
    blob: Blob,
    base: Fields,
    local: Fields,
    /// Just turned on: the server's leaves win where it has them.
    adopt: bool,
    choices: settings_sync::Choices,
}

impl Job {
    /// Whether the leaf at `path` syncs on this device.
    fn syncs(&self, path: &str) -> bool {
        self.blob == Blob::Playlists || settings_sync::syncs(&self.choices, settings_sync::setting_of(path))
    }
}

/// What a sync did.
struct Outcome {
    /// The server's revision and leaves now.
    revision: i64,
    merged: Fields,
    /// This device's leaves the sync started from.
    local: Fields,
    changes: Vec<Change>,
}

/// Merge with the server's leaves and write the result, until a write
/// isn't beaten by another device's.
fn sync(job: Job) -> Synced {
    let name = job.blob.name();
    for _ in 0..5 {
        let (revision, plain) = client::sync_get(&job.server, &job.server_key, &job.identity, &job.sync_key, name)?;
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
                    .filter(|(path, _)| job.syncs(path))
                    .map(|(path, _)| Change { path: path.clone(), conflict: None })
                    .collect();
                (merged, changes)
            }
            Some(server) => settings_sync::merge_where(&job.base, &job.local, server, |path| job.syncs(path)),
            None => (job.local.clone(), Vec::new()),
        };
        if server.as_ref() == Some(&merged) {
            return Ok(Outcome { revision, merged, local: job.local, changes });
        }
        let plain = serde_json::to_vec(&merged).map_err(|e| e.to_string())?;
        let written = client::sync_write(&job.server, &job.server_key, &job.identity, &job.sync_key, name, revision, &plain)?;
        if written.stored {
            return Ok(Outcome { revision: written.revision, merged, local: job.local, changes });
        }
    }
    Err(format!("other devices kept changing the {name}: trying again in a minute"))
}

/// The debug toast's text: what changed, each named by `title`.
fn describe(what: &str, changes: &[Change], title: impl Fn(&str) -> String) -> String {
    let parts: Vec<String> = changes
        .iter()
        .map(|c| match c.conflict {
            None => title(&c.path),
            Some(Winner::ThisDevice) => format!("{} (changed on both: this device's kept)", title(&c.path)),
            Some(Winner::Server) => format!("{} (changed on both: the other device's taken)", title(&c.path)),
        })
        .collect();
    format!("{what} sync changed: {}", parts.join(", "))
}

impl App {
    pub(super) fn sync_on(&self) -> bool {
        self.sync.on
    }

    /// Whether `blob` syncs here.
    fn syncing(&self, blob: Blob) -> bool {
        self.sync.on && (blob == Blob::Settings || settings_sync::syncs(&self.sync.choices, &settings_sync::PLAYLISTS))
    }

    fn channel(&mut self, blob: Blob) -> &mut Channel {
        &mut self.sync.channels[blob.index()]
    }

    /// The notification stream from `server` opened again (`None`), or said
    /// synced data changed: sync what's the authority's and newer.
    pub(super) fn sync_server_seen(&mut self, server: &str, changed: Option<library::sync::Changed>) {
        if !self.sync.on || server != self.authority().0 {
            return;
        }
        for blob in Blob::ALL {
            let newer = match &changed {
                Some(changed) => changed.name == blob.name() && changed.revision > self.sync.channels[blob.index()].revision,
                None => true,
            };
            if newer && self.syncing(blob) {
                self.channel(blob).wanted = Some(Instant::now());
            }
        }
    }

    /// The playlists changed here: with them syncing, note what changed,
    /// to send once it's settled.
    pub(super) fn playlists_changed(&mut self) {
        if !self.syncing(Blob::Playlists) {
            return;
        }
        let Some(mut state) = State::load(playlist_sync::NAME) else { return };
        if state.record_leaves(playlist_sync::leaves(&self.playlists, &self.songs), settings_sync::now()) {
            if let Err(e) = state.save(playlist_sync::NAME) {
                log::warn!("playlist sync: {e}");
            }
            self.channel(Blob::Playlists).dirty_since = Some(Instant::now());
        }
    }

    /// Each frame: start a sync when one's due, and take one that's done.
    pub(super) fn poll_sync(&mut self) {
        let identity = self.library_identity();
        let whose = identity.as_ref().map(Identity::public);
        if whose != self.sync.for_identity {
            // (Signed in, out, or as someone else: on only if the state's
            // for this sync key.)
            let state = State::load(settings_sync::NAME);
            self.sync.on = state.is_some() && identity.is_some();
            self.sync.choices = state.map(|s| s.choices).unwrap_or_default();
            for blob in Blob::ALL {
                let revision = State::load(blob.name()).map_or(0, |s| s.revision);
                let wanted = self.syncing(blob).then(Instant::now);
                *self.channel(blob) = Channel { revision, wanted, ..Channel::default() };
            }
            self.sync.for_identity = whose;
        }
        if settings_sync::take_dirty() && self.sync.on {
            self.channel(Blob::Settings).dirty_since = Some(Instant::now());
        }
        while let Ok((blob, result)) = self.sync.events.1.try_recv() {
            self.channel(blob).running = false;
            match result {
                Ok(outcome) => match blob {
                    Blob::Settings => self.settings_synced(outcome),
                    Blob::Playlists => self.playlists_synced(outcome),
                },
                Err(e) => {
                    log::info!("{} sync: {e}", blob.name());
                    self.sync.error = Some(e);
                    self.channel(blob).wanted = Some(Instant::now() + RETRY);
                }
            }
        }
        for blob in Blob::ALL {
            let channel = &self.sync.channels[blob.index()];
            let settled = channel.dirty_since.is_some_and(|t| t.elapsed() >= SETTLE);
            let due = channel.wanted.is_some_and(|t| Instant::now() >= t);
            if self.syncing(blob) && !channel.running && (settled || due) {
                let channel = self.channel(blob);
                channel.dirty_since = None;
                channel.wanted = None;
                self.start_sync(blob, false);
            }
        }
    }

    fn start_sync(&mut self, blob: Blob, adopt: bool) {
        let (server, server_key) = self.authority();
        let (Some(identity), Some(server_key)) = (self.library_identity(), server_key) else {
            self.sync.error = Some(format!("{} hasn't been reached yet: open the Online Library first.", super::library::host(&server)));
            return;
        };
        let Some(sync_key) = identity::sync_key() else {
            self.sync.on = false;
            return;
        };
        let Some(state) = State::load(blob.name()) else { return };
        let choices = self.sync.choices.clone();
        let job = Job { server, server_key, identity, sync_key, blob, base: state.base, local: state.fields, adopt, choices };
        self.channel(blob).running = true;
        let tx = self.sync.events.0.clone();
        std::thread::Builder::new()
            .name(format!("{} sync", blob.name()))
            .spawn(move || {
                let _ = tx.send((blob, sync(job)));
            })
            .ok();
    }

    /// A settings sync is done: take its settings, with whatever was
    /// changed here while it ran, and send those next.
    fn settings_synced(&mut self, outcome: Outcome) {
        self.sync.error = None;
        // (Turned off meanwhile.)
        let Some(mut state) = State::load(settings_sync::NAME) else { return };
        let (result, _) = settings_sync::merge(&outcome.local, &state.fields, &outcome.merged, &state.choices);
        let mut json = match serde_json::to_value(&self.config) {
            Ok(json) => json,
            Err(e) => return log::warn!("settings sync: {e}"),
        };
        settings_sync::apply::<Config>(&mut json, &result, &state.choices);
        match serde_json::from_value::<Config>(json) {
            Ok(config) => {
                let before = self.current_theme();
                let modes_changed = (config.loop_mode, config.shuffle) != (self.config.loop_mode, self.config.shuffle);
                self.config = config;
                if self.current_theme() != before {
                    self.reapply_theme();
                }
                // (Loop and Shuffle in use are the app's own, saved as they
                // change: take the synced ones where they're remembered.)
                if modes_changed && self.config.remember.modes {
                    self.loop_mode = self.config.loop_mode;
                    self.shuffle = self.config.shuffle;
                }
            }
            Err(e) => return log::warn!("settings sync: {e}"),
        }
        state.applied = settings_sync::synced_leaves(&serde_json::to_value(&self.config).unwrap_or_default(), &state.choices);
        if result != outcome.merged {
            self.channel(Blob::Settings).dirty_since = Some(Instant::now());
        }
        state.revision = outcome.revision;
        state.base = outcome.merged;
        state.fields = result;
        self.channel(Blob::Settings).revision = state.revision;
        if let Err(e) = state.save(settings_sync::NAME).and_then(|()| self.config.save().map_err(|e| e.to_string())) {
            log::warn!("settings sync: {e}");
        }
        if self.config.sync_debug && !outcome.changes.is_empty() {
            self.sync.toast = Some((describe("Settings", &outcome.changes, str::to_string), Instant::now()));
        }
    }

    /// A playlist sync is done: take its playlists, with whatever was
    /// changed here while it ran, and send those next.
    fn playlists_synced(&mut self, outcome: Outcome) {
        self.sync.error = None;
        let Some(mut state) = State::load(playlist_sync::NAME) else { return };
        let (result, _) = settings_sync::merge_where(&outcome.local, &state.fields, &outcome.merged, |_| true);
        let playing = self.now_playing.as_ref().and_then(|np| self.playlists.lists.get(np.list)).map(|l| l.id.clone());
        let viewed = self.viewed_playlist.and_then(|v| self.playlists.lists.get(v)).map(|l| l.id.clone());
        match playlist_sync::apply(&mut self.playlists, &mut self.songs, &result) {
            Ok(touched) if !touched.is_empty() => self.playlists_replaced(&touched, playing, viewed),
            Ok(_) => {}
            Err(e) => {
                self.status = format!("Couldn't save a synced playlist: {e}");
                return;
            }
        }
        state.applied = playlist_sync::leaves(&self.playlists, &self.songs);
        if result != outcome.merged {
            self.channel(Blob::Playlists).dirty_since = Some(Instant::now());
        }
        state.revision = outcome.revision;
        state.base = outcome.merged;
        state.fields = result;
        self.channel(Blob::Playlists).revision = state.revision;
        if let Err(e) = state.save(playlist_sync::NAME) {
            log::warn!("playlist sync: {e}");
        }
        if self.config.sync_debug && !outcome.changes.is_empty() {
            let title = |id: &str| {
                let named = self.playlists.find(id).map(|i| self.playlists.lists[i].playlist.name.clone());
                named.unwrap_or_else(|| format!("a removed playlist ({id})"))
            };
            self.sync.toast = Some((describe("Playlist", &outcome.changes, title), Instant::now()));
        }
    }

    /// Synced playlists replaced or removed these (by ID): find the one
    /// playing and the one open again, and start over in one that changed.
    fn playlists_replaced(&mut self, touched: &[String], playing: Option<String>, viewed: Option<String>) {
        self.playlist_selection.clear();
        self.now_playing = self.now_playing.take().and_then(|mut np| {
            let id = playing?;
            np.list = self.playlists.find(&id)?;
            if touched.contains(&id) {
                let len = self.playlists.lists[np.list].playlist.entries.len();
                (np.entry < len).then_some(())?;
                np.history = vec![np.entry];
                np.upcoming = None;
            }
            Some(np)
        });
        self.viewed_playlist = viewed
            .and_then(|id| self.playlists.find(&id))
            .or_else(|| (!self.playlists.lists.is_empty()).then_some(0));
        self.refresh_missing();
        self.plan_upcoming();
    }

    /// Start syncing the playlists here: the server's, if it has any, with
    /// this device's added.
    fn start_playlist_sync(&mut self) {
        let Some(sync_key) = identity::sync_key() else { return };
        let state = State::of(&sync_key, playlist_sync::leaves(&self.playlists, &self.songs));
        match state.save(playlist_sync::NAME) {
            Ok(()) => {
                *self.channel(Blob::Playlists) = Channel::default();
                self.start_sync(Blob::Playlists, true);
            }
            Err(e) => self.sync.error = Some(e),
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
            "Keeps your settings and playlists the same on every device signed in as you that has \
             this on. What belongs to one device stays on it: folders, layouts, the audio buffer, the \
             mobile layout, the interface scale, updates, and the soundfont a playlist entry uses. \
             They're encrypted on this device first, with a key only your devices have, so the server \
             can't read them. Turning it on here takes what your other devices synced, if there's \
             anything; a song in a playlist that isn't on this device shows as unavailable.",
            |ui| ui.add_enabled(signed_in && offered, egui::Checkbox::new(&mut on, "Sync settings and playlists between my devices")),
        );
        if !offered {
            ui.weak(format!("{} doesn't sync yet.", super::library::host(&authority)));
        }
        if on != self.sync.on {
            if on {
                self.turn_sync_on();
            } else {
                for blob in Blob::ALL {
                    settings_sync::stop(blob.name());
                }
                self.sync.on = false;
                self.sync.error = None;
            }
        }
        if self.sync.on {
            if let Some(error) = &self.sync.error {
                ui.weak(format!("Couldn't sync: {error}"));
            }
            self.sync_choices_ui(ui);
            let mut debug = self.config.sync_debug;
            if ui.checkbox(&mut debug, "Show what each sync changed (for testing)").changed() {
                self.config.sync_debug = debug;
                if let Err(e) = self.config.save() {
                    log::warn!("saving the config: {e}");
                }
            }
        }
    }

    /// Which settings this device syncs: each category, opening to its
    /// settings.
    fn sync_choices_ui(&mut self, ui: &mut egui::Ui) {
        let mut chosen: Vec<(&'static settings_sync::Setting, bool)> = Vec::new();
        egui::CollapsingHeader::new("What this device syncs").id_salt("sync_choices").show(ui, |ui| {
            ui.weak("Unticked: this device keeps its own, and doesn't change the others'.");
            for category in settings_sync::CATEGORIES {
                let choices = &self.sync.choices;
                let on = category.settings.iter().filter(|s| settings_sync::syncs(choices, s)).count();
                let id = ui.make_persistent_id(("sync_category", category.name));
                egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                    .show_header(ui, |ui| {
                        let mut all = on == category.settings.len();
                        let mixed = on > 0 && !all;
                        let checkbox = egui::Checkbox::new(&mut all, category.name).indeterminate(mixed);
                        if ui.add(checkbox).changed() {
                            chosen.extend(category.settings.iter().map(|s| (s, all)));
                        }
                    })
                    .body(|ui| {
                        for setting in category.settings {
                            let mut on = settings_sync::syncs(choices, setting);
                            if ui.checkbox(&mut on, setting.name).changed() {
                                chosen.push((setting, on));
                            }
                        }
                    });
            }
        });
        if chosen.is_empty() {
            return;
        }
        let Some(mut state) = State::load(settings_sync::NAME) else { return };
        let config = serde_json::to_value(&self.config).unwrap_or_default();
        let any_on = chosen.iter().any(|(setting, on)| *on && setting.key != settings_sync::PLAYLISTS.key);
        let playlists = chosen.iter().find(|(setting, _)| setting.key == settings_sync::PLAYLISTS.key).map(|(_, on)| *on);
        for (setting, on) in chosen {
            state.choose(setting, on, &config);
        }
        if let Err(e) = state.save(settings_sync::NAME) {
            self.sync.error = Some(e);
            return;
        }
        self.sync.choices = state.choices;
        if any_on {
            self.channel(Blob::Settings).wanted = Some(Instant::now());
        }
        match playlists {
            Some(true) => self.start_playlist_sync(),
            Some(false) => settings_sync::stop(playlist_sync::NAME),
            None => {}
        }
    }

    fn turn_sync_on(&mut self) {
        let made = identity::ensure_sync_key().and_then(|sync_key| {
            let json = serde_json::to_value(&self.config).map_err(|e| e.to_string())?;
            State::new(&sync_key, &json).save(settings_sync::NAME)
        });
        match made {
            Ok(()) => {
                self.sync.on = true;
                self.sync.choices = Default::default();
                self.sync.error = None;
                *self.channel(Blob::Settings) = Channel::default();
                self.start_sync(Blob::Settings, true);
                if self.syncing(Blob::Playlists) {
                    self.start_playlist_sync();
                }
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
            blob: Blob::Settings,
            base: state.base.clone(),
            local: state.fields.clone(),
            adopt,
            choices: state.choices.clone(),
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

        // Playlists are a blob of their own: a whole playlist is one leaf,
        // and one changed on both devices takes the newer change.
        let playlist = |name: &str| json!({ "name": name, "songs": [] });
        let desktop_lists = State::of(&[4; 32], [("00000000000000aa".to_string(), playlist("Road trip"))].into());
        let up = sync(Job { blob: Blob::Playlists, ..job(&base, &key, &me, &desktop_lists, true) }).unwrap();
        let mut desktop_lists = after(desktop_lists, &up);
        let phone_lists = State::of(&[4; 32], [("00000000000000bb".to_string(), playlist("Gym"))].into());
        let down = sync(Job { blob: Blob::Playlists, ..job(&base, &key, &me, &phone_lists, true) }).unwrap();
        assert_eq!(down.merged.len(), 2, "each device's playlists, on both");
        let mut phone_lists = after(phone_lists, &down);
        let caught_up = sync(Job { blob: Blob::Playlists, ..job(&base, &key, &me, &desktop_lists, false) }).unwrap();
        desktop_lists = after(desktop_lists, &caught_up);
        assert_eq!(desktop_lists.fields.len(), 2);
        desktop_lists.record_leaves([("00000000000000aa".to_string(), playlist("Road trip 2"))].into(), now + 20);
        phone_lists.record_leaves(
            [("00000000000000aa".to_string(), playlist("Roadtrip")), ("00000000000000bb".to_string(), playlist("Gym"))].into(),
            now + 30,
        );
        sync(Job { blob: Blob::Playlists, ..job(&base, &key, &me, &desktop_lists, false) }).unwrap();
        let last = sync(Job { blob: Blob::Playlists, ..job(&base, &key, &me, &phone_lists, false) }).unwrap();
        assert_eq!(last.merged["00000000000000aa"].value["name"], "Roadtrip", "the newer change");
        assert!(!last.merged.contains_key("00000000000000bb"), "removed on the desktop");
        // (The settings are untouched by all that.)
        let (_, plain) = client::sync_get(&base, &key, &me, &[4; 32], settings_sync::NAME).unwrap();
        assert_eq!(serde_json::from_slice::<Fields>(&plain.unwrap()).unwrap(), second.merged);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
