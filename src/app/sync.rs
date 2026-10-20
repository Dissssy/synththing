//! Sync in the app (`settings_sync.rs`, `playlist_sync.rs`,
//! `song_sync.rs`): the switch in Preferences > Online Library, and
//! syncing each blob (the settings, the playlists, the song library and
//! its transfers) with the authority on a thread of its own: at launch, a
//! moment after it changes here, and when the notification stream says
//! another device wrote. Songs move through the relay on a thread of their
//! own too, one after another. The library's and transfers' states live
//! on a thread of their own as well (`songs`): they cover every song.

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

use super::App;
use crate::config::Config;
use crate::library::identity::{self, Identity};
use crate::library::{self, client};
use crate::device::Device;
use crate::frame_timing;
use crate::playlist_sync;
use crate::settings_sync::{self, Change, Fields, State, Winner};
use crate::song_store::Stored;
use crate::song_sync::{self, Moved, Relay, Task};

mod songs;

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
    Library,
    Transfers,
}

impl Blob {
    const ALL: [Blob; 4] = [Blob::Settings, Blob::Playlists, Blob::Library, Blob::Transfers];

    fn name(self) -> &'static str {
        match self {
            Blob::Settings => settings_sync::NAME,
            Blob::Playlists => playlist_sync::NAME,
            Blob::Library => song_sync::LIBRARY,
            Blob::Transfers => song_sync::TRANSFERS,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Its section in the slow-frame log, taking in a sync.
    fn synced_section(self) -> &'static str {
        match self {
            Blob::Settings => "settings synced",
            Blob::Playlists => "playlists synced",
            Blob::Library => "library synced",
            Blob::Transfers => "transfers synced",
        }
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
    /// Just started syncing here: the next sync takes the server's leaves
    /// where it has them, until one works.
    adopt: bool,
}

pub struct SyncState {
    /// Sync is on here (the settings' state loads), as of `for_identity`.
    on: bool,
    for_identity: Option<String>,
    /// The kinds of settings this device syncs (`State::choices`).
    choices: settings_sync::Choices,
    channels: [Channel; 4],
    events: (Sender<Done>, Receiver<Done>),
    error: Option<String>,
    toast: Option<(String, Instant)>,
    /// The songs here or the playlists changed: say so in the library, and
    /// see what to fetch or send.
    songs_dirty: bool,
    /// This device's ID (`Device::id`), once it's needed.
    device: Option<String>,
    /// Songs being moved through the relay, by key.
    busy: BTreeSet<String>,
    moves: (Sender<Moving>, Receiver<Moving>),
    /// A move failed: no more until then.
    moves_paused: Option<Instant>,
    /// How the songs being moved are going.
    progress: Progress,
    /// Keeps the library's and transfers' states.
    worker: (Sender<songs::Request>, Receiver<songs::Reply>),
    /// The songs went to the worker; the next change waits for its plan.
    songs_pending: bool,
}

/// How a round of songs being moved is going: those to fetch or send, how
/// many are done, and of those, fetched and sent.
#[derive(Default)]
struct Progress {
    total: usize,
    done: usize,
    fetched: usize,
    sent: usize,
}

/// How the songs being moved are going.
enum Moving {
    Moved(Moved),
    Finished,
}

impl Default for SyncState {
    fn default() -> Self {
        let events = mpsc::channel();
        let worker = songs::spawn(events.0.clone());
        Self {
            on: false,
            for_identity: None,
            choices: Default::default(),
            channels: Default::default(),
            events,
            error: None,
            toast: None,
            songs_dirty: false,
            device: None,
            busy: BTreeSet::new(),
            moves: mpsc::channel(),
            moves_paused: None,
            progress: Progress::default(),
            worker,
            songs_pending: false,
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
        self.blob != Blob::Settings || settings_sync::syncs(&self.choices, settings_sync::setting_of(path))
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
        self.sync.on
            && match blob {
                Blob::Settings => true,
                Blob::Playlists => settings_sync::syncs(&self.sync.choices, &settings_sync::PLAYLISTS),
                Blob::Library | Blob::Transfers => settings_sync::syncs(&self.sync.choices, &settings_sync::SONGS),
            }
    }

    /// The songs here changed (`refresh_missing`).
    pub(super) fn songs_changed(&mut self) {
        self.sync.songs_dirty = true;
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
        self.sync.songs_dirty = true;
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
            for blob in [Blob::Settings, Blob::Playlists] {
                let state = State::load(blob.name());
                let wanted = self.syncing(blob).then(Instant::now);
                *self.channel(blob) = Channel { revision: state.as_ref().map_or(0, |s| s.revision), wanted, ..Channel::default() };
                // (One this app syncs that hasn't started here: sync was on
                // before this app had it.)
                if state.is_none() && self.syncing(blob) {
                    self.start_blob(blob);
                }
            }
            // (The songs' once the worker has read them: `Reply::Loaded`.)
            for blob in [Blob::Library, Blob::Transfers] {
                *self.channel(blob) = Channel::default();
            }
            self.ask_worker(songs::Request::Reload);
            self.sync.for_identity = whose;
            self.sync.songs_dirty = true;
        }
        if settings_sync::take_dirty() && self.sync.on {
            self.channel(Blob::Settings).dirty_since = Some(Instant::now());
        }
        while let Ok((blob, result)) = self.sync.events.1.try_recv() {
            let _timing = frame_timing::scope(blob.synced_section());
            if let Ok(outcome) = &result {
                log::info!("{} synced: revision {}, {} changed", blob.name(), outcome.revision, outcome.changes.len());
            }
            self.channel(blob).running = false;
            if result.is_ok() {
                self.channel(blob).adopt = false;
            }
            match result {
                Ok(outcome) => match blob {
                    Blob::Settings => self.settings_synced(outcome),
                    Blob::Playlists => self.playlists_synced(outcome),
                    Blob::Library | Blob::Transfers => {
                        self.sync.error = None;
                        let songs = self.songs_now();
                        self.ask_worker(songs::Request::Synced(blob, outcome, songs));
                    }
                },
                Err(e) => {
                    log::info!("{} sync: {e}", blob.name());
                    self.sync.error = Some(e);
                    self.channel(blob).wanted = Some(Instant::now() + RETRY);
                }
            }
        }
        while let Ok(reply) = self.sync.worker.1.try_recv() {
            frame_timing::time("sync worker replies", || self.take_reply(reply));
        }
        frame_timing::time("poll_moves", || self.poll_moves());
        if self.sync.songs_dirty && self.sync.on && !self.sync.songs_pending {
            self.sync.songs_dirty = false;
            self.sync.songs_pending = true;
            let songs = self.songs_now();
            self.ask_worker(songs::Request::Songs(songs));
        }
        for blob in Blob::ALL {
            let channel = &self.sync.channels[blob.index()];
            let settled = channel.dirty_since.is_some_and(|t| t.elapsed() >= SETTLE);
            let due = channel.wanted.is_some_and(|t| Instant::now() >= t);
            if self.syncing(blob) && !channel.running && (settled || due) {
                let channel = self.channel(blob);
                channel.dirty_since = None;
                channel.wanted = None;
                let adopt = channel.adopt;
                frame_timing::time("start_sync", || self.start_sync(blob, adopt));
            }
        }
    }

    /// The authority, its key, the identity and the sync key, to sync with.
    fn sync_with(&mut self) -> Option<(String, String, Identity, [u8; 32])> {
        let (server, server_key) = self.authority();
        let (Some(identity), Some(server_key)) = (self.library_identity(), server_key) else {
            self.sync.error = Some(format!("{} hasn't been reached yet: open the Online Library first.", super::library::host(&server)));
            return None;
        };
        let Some(sync_key) = identity::sync_key() else {
            self.sync.on = false;
            return None;
        };
        Some((server, server_key, identity, sync_key))
    }

    fn start_sync(&mut self, blob: Blob, adopt: bool) {
        if matches!(blob, Blob::Library | Blob::Transfers) {
            let Some((server, server_key, identity, sync_key)) = self.sync_with() else { return };
            let choices = self.sync.choices.clone();
            let job = Job { server, server_key, identity, sync_key, blob, base: Fields::new(), local: Fields::new(), adopt, choices };
            self.channel(blob).running = true;
            return self.ask_worker(songs::Request::Sync(Box::new(job)));
        }
        let Some(state) = State::load(blob.name()) else { return };
        let Some((server, server_key, identity, sync_key)) = self.sync_with() else { return };
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
        let mut json = match self.config.to_json() {
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
        state.applied = settings_sync::synced_leaves(&self.config.to_json().unwrap_or_default(), &state.choices);
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

    /// Start syncing `blob` here: the server's leaves, if it has any, with
    /// this device's added.
    fn start_blob(&mut self, blob: Blob) {
        let leaves = match blob {
            Blob::Settings => return,
            Blob::Playlists => playlist_sync::leaves(&self.playlists, &self.songs),
            Blob::Library | Blob::Transfers => {
                let songs = self.songs_now();
                return self.ask_worker(songs::Request::Start(blob, songs));
            }
        };
        let Some(sync_key) = identity::sync_key() else { return };
        match State::of(&sync_key, leaves).save(blob.name()) {
            Ok(()) => *self.channel(blob) = Channel { wanted: Some(Instant::now()), adopt: true, ..Channel::default() },
            Err(e) => self.sync.error = Some(e),
        }
    }

    /// The songs as they are here, for the worker.
    fn songs_now(&mut self) -> Box<songs::Songs> {
        let _timing = crate::frame_timing::scope("songs for the sync worker");
        let device = self.sync.device.get_or_insert_with(|| Device::load().id).clone();
        let every = settings_sync::syncs(&self.sync.choices, &settings_sync::EVERY_SONG);
        let (authority, _) = self.authority();
        Box::new(songs::Songs {
            store: self.songs.clone(),
            wanted: song_sync::wanted(&self.songs, &self.playlists, every),
            device,
            busy: self.sync.busy.clone(),
            paused: self.sync.moves_paused.is_some_and(|t| Instant::now() < t),
            relay_song_mb: self.library_server_info(&authority).map(|i| i.relay_song_mb),
            library: self.syncing(Blob::Library),
            transfers: self.syncing(Blob::Transfers),
        })
    }

    fn ask_worker(&self, request: songs::Request) {
        let _ = self.sync.worker.0.send(request);
    }

    fn take_reply(&mut self, reply: songs::Reply) {
        match reply {
            songs::Reply::Loaded { library, transfers } => {
                for (blob, revision) in [(Blob::Library, library), (Blob::Transfers, transfers)] {
                    match revision {
                        Some(revision) => {
                            let wanted = self.syncing(blob).then(Instant::now);
                            *self.channel(blob) = Channel { revision, wanted, ..Channel::default() };
                        }
                        // (One this app syncs that hasn't started here: sync
                        // was on before this app had it.)
                        None if self.syncing(blob) => self.start_blob(blob),
                        None => {}
                    }
                }
            }
            songs::Reply::Started(blob) => *self.channel(blob) = Channel { wanted: Some(Instant::now()), adopt: true, ..Channel::default() },
            songs::Reply::Changed(blob) => self.channel(blob).dirty_since = Some(Instant::now()),
            songs::Reply::Planned { tasks, too_big } => {
                self.sync.songs_pending = false;
                self.start_moves(tasks, too_big);
            }
            songs::Reply::Skipped(blob) => self.channel(blob).running = false,
            songs::Reply::Taken { blob, revision, library } => {
                self.channel(blob).revision = revision;
                if let Some(library) = library {
                    song_sync::apply_library(&mut self.songs, &library);
                }
                self.sync.songs_dirty = true;
            }
            songs::Reply::Error(e) => self.sync.error = Some(e),
        }
    }

    /// Stop syncing `blob` here.
    fn stop_blob(&mut self, blob: Blob) {
        match blob {
            Blob::Library | Blob::Transfers => self.ask_worker(songs::Request::Stop(blob)),
            _ => settings_sync::stop(blob.name()),
        }
    }

    /// Start moving the songs the worker planned to (`too_big`, the names
    /// of those the relay won't take).
    fn start_moves(&mut self, tasks: Vec<Task>, too_big: Vec<String>) {
        if tasks.is_empty() && too_big.is_empty() {
            return;
        }
        let (authority, _) = self.authority();
        let limit = self.library_server_info(&authority).map(|i| i.relay_song_mb);
        if limit == Some(0) {
            // (Its "not found" would read as songs not sent yet.)
            self.sync.error = Some(format!("{} can't move songs between your devices yet: it needs updating.", super::library::host(&authority)));
            return;
        }
        if !too_big.is_empty() {
            self.sync.error = Some(format!(
                "{} is too big to move between your devices (the server takes songs up to {} MB).",
                too_big.join(", "),
                limit.unwrap_or_default()
            ));
        }
        if tasks.is_empty() {
            return;
        }
        let Some((server, server_key, identity, sync_key)) = self.sync_with() else { return };
        let relay = Relay { server, server_key, identity, sync_key, dir: self.songs.dir().to_path_buf() };
        self.sync.busy = tasks.iter().map(|t| t.key().to_string()).collect();
        self.sync.progress = Progress { total: tasks.iter().filter(|t| !matches!(t, Task::Delete(_))).count(), ..Progress::default() };
        let tx = self.sync.moves.0.clone();
        std::thread::Builder::new()
            .name("song transfers".into())
            .spawn(move || {
                // (One song failing doesn't stop the rest; the failed ones
                // wait for the next try.)
                for task in &tasks {
                    if tx.send(Moving::Moved(song_sync::run(&relay, task))).is_err() {
                        break;
                    }
                }
                let _ = tx.send(Moving::Finished);
            })
            .ok();
    }

    /// Take the songs moved since the last frame.
    fn poll_moves(&mut self) {
        while let Ok(moving) = self.sync.moves.1.try_recv() {
            let moved = match moving {
                Moving::Moved(moved) => moved,
                Moving::Finished => {
                    self.sync.busy.clear();
                    self.sync.songs_dirty = true;
                    let Progress { fetched, sent, .. } = std::mem::take(&mut self.sync.progress);
                    let songs = |n: usize| if n == 1 { "1 song".to_string() } else { format!("{n} songs") };
                    match (fetched, sent) {
                        (0, 0) => {}
                        (_, 0) => self.status = format!("Downloaded {} from your other devices.", songs(fetched)),
                        (0, _) => self.status = format!("Sent {} for your other devices.", songs(sent)),
                        _ => self.status = format!("Downloaded {} from your other devices, and sent {}.", songs(fetched), songs(sent)),
                    }
                    continue;
                }
            };
            let progress = &mut self.sync.progress;
            if !matches!(moved, Moved::Deleted) {
                progress.done = (progress.done + 1).min(progress.total);
            }
            let name = |key: &str| self.songs.known().find(|(k, _)| *k == key).map_or_else(|| key.to_string(), |(_, s)| s.name.clone());
            let shown = match &moved {
                Moved::Fetched(key) => {
                    progress.fetched += 1;
                    Some(format!("Downloaded {}", name(key)))
                }
                Moved::Sent(key) => {
                    progress.sent += 1;
                    Some(format!("Sent {}", name(key)))
                }
                _ => None,
            };
            if let Some(text) = shown
                && self.config.sync_debug
            {
                let Progress { done, total, .. } = self.sync.progress;
                self.sync.toast = Some((format!("{text} ({done} of {total})"), Instant::now()));
            }
            match &moved {
                Moved::Fetched(key) => {
                    let name = name(key);
                    self.add_to_library([Stored { key: key.clone(), name }]);
                }
                Moved::Failed(key, e) => {
                    log::info!("moving song {key}: {e}");
                    self.sync.error = Some(format!("couldn't move a song between your devices: {e}"));
                    self.sync.moves_paused = Some(Instant::now() + RETRY);
                }
                Moved::Sent(_) | Moved::Gone(_) => self.ask_worker(songs::Request::Moved(moved.clone())),
                Moved::Deleted => {}
            }
        }
        if self.sync.moves_paused.is_some_and(|t| Instant::now() >= t) {
            self.sync.moves_paused = None;
            self.sync.songs_dirty = true;
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
             this on, and brings the songs in your playlists (or every song) over from your other \
             devices. What belongs to one device stays on it: folders, layouts, the audio buffer, the \
             mobile layout, the interface scale, updates, and the soundfont a playlist entry uses. \
             Everything is encrypted on this device first, with a key only your devices have, so the \
             server can't read it. Turning it on here takes what your other devices synced, if there's \
             anything; a song shows as unavailable until it's here.",
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
                    self.stop_blob(blob);
                }
                self.sync.on = false;
                self.sync.error = None;
            }
        }
        if self.sync.on {
            if let Some(error) = &self.sync.error {
                ui.weak(format!("Couldn't sync: {error}"));
            }
            if let Some((done, total)) = self.songs_moving() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(format!("Moving songs between your devices: {done} of {total}"));
                });
            }
            self.sync_choices_ui(ui);
            self.blocked_songs_ui(ui);
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
        let config = self.config.to_json().unwrap_or_default();
        let own = [settings_sync::PLAYLISTS.key, settings_sync::SONGS.key, settings_sync::EVERY_SONG.key];
        let any_on = chosen.iter().any(|(setting, on)| *on && !own.contains(&setting.key));
        let toggled = |of: &settings_sync::Setting| chosen.iter().find(|(setting, _)| setting.key == of.key).map(|(_, on)| *on);
        let (playlists, songs) = (toggled(&settings_sync::PLAYLISTS), toggled(&settings_sync::SONGS));
        if toggled(&settings_sync::EVERY_SONG).is_some() {
            self.sync.songs_dirty = true;
        }
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
            Some(true) => self.start_blob(Blob::Playlists),
            Some(false) => self.stop_blob(Blob::Playlists),
            None => {}
        }
        for blob in [Blob::Library, Blob::Transfers] {
            match songs {
                Some(true) => self.start_blob(blob),
                Some(false) => self.stop_blob(blob),
                None => {}
            }
        }
    }

    /// The songs removed here, which this device doesn't fetch from the
    /// others: each can be fetched again.
    fn blocked_songs_ui(&mut self, ui: &mut egui::Ui) {
        let blocked: Vec<(String, String)> =
            self.songs.blocked().map(|(key, song)| (key.to_string(), song.map_or_else(|| key.to_string(), |s| s.name.clone()))).collect();
        if blocked.is_empty() || !self.syncing(Blob::Transfers) {
            return;
        }
        let mut unblock = None;
        egui::CollapsingHeader::new(format!("Songs removed from this device ({})", blocked.len())).id_salt("sync_blocked").show(ui, |ui| {
            ui.weak("This device doesn't fetch these from your other devices.");
            for (key, name) in &blocked {
                ui.horizontal(|ui| {
                    if ui.small_button("Fetch again").clicked() {
                        unblock = Some(key.clone());
                    }
                    ui.label(name);
                });
            }
        });
        if let Some(key) = unblock {
            self.songs.unblock(&key);
            self.sync.songs_dirty = true;
        }
    }

    fn turn_sync_on(&mut self) {
        let made = identity::ensure_sync_key().and_then(|sync_key| {
            let json = self.config.to_json().map_err(|e| e.to_string())?;
            State::new(&sync_key, &json).save(settings_sync::NAME)
        });
        match made {
            Ok(()) => {
                self.sync.on = true;
                self.sync.choices = Default::default();
                self.sync.error = None;
                *self.channel(Blob::Settings) = Channel::default();
                self.start_sync(Blob::Settings, true);
                for blob in [Blob::Playlists, Blob::Library, Blob::Transfers] {
                    if self.syncing(blob) {
                        self.start_blob(blob);
                    }
                }
            }
            Err(e) => self.sync.error = Some(e),
        }
    }

    /// Songs being moved between this device and the others: how many are
    /// done, of how many.
    pub(super) fn songs_moving(&self) -> Option<(usize, usize)> {
        let Progress { total, done, .. } = self.sync.progress;
        (total > 0).then_some((done, total))
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

/// Songs being moved (`App::songs_moving`), as a spinner and a count.
pub(super) fn songs_moving_ui(ui: &mut egui::Ui, moving: Option<(usize, usize)>) {
    if let Some((done, total)) = moving {
        let hover = "Moving songs between your devices: done, of how many";
        ui.spinner().on_hover_text(hover);
        ui.weak(format!("{done}/{total}")).on_hover_text(hover);
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

    /// One round on a device: sync the transfers, plan, do the tasks, and
    /// sync what they changed. Its state after.
    fn round(base: &str, key: &str, me: &Identity, state: State, device: &str, store: &mut crate::song_store::SongStore, wanted: &BTreeSet<String>) -> State {
        let relay = Relay { server: base.into(), server_key: key.into(), identity: me.clone(), sync_key: [4; 32], dir: store.dir().to_path_buf() };
        let synced = sync(Job { blob: Blob::Transfers, ..job(base, key, me, &state, false) }).unwrap();
        let mut state = after(state, &synced);
        let plan = song_sync::plan(&state.applied, device, store, wanted, &BTreeSet::new());
        let mut leaves = plan.leaves;
        for task in &plan.tasks {
            let moved = song_sync::run(&relay, task);
            if let Moved::Fetched(key) = &moved {
                store.add(Stored { key: key.clone(), name: "Song.mid".into() });
            }
            leaves = song_sync::after(leaves, &moved);
        }
        state.record_leaves(leaves, settings_sync::now());
        let synced = sync(Job { blob: Blob::Transfers, ..job(base, key, me, &state, false) }).unwrap();
        after(state, &synced)
    }

    #[test]
    fn a_song_moves_between_devices() {
        let dir = std::env::temp_dir().join(format!("synththing-song-transfer-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false, "check_uploads": false, "sync": true }"#).unwrap();
        let running = library::server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = client::info(&base).unwrap().key;
        let me = Identity::generate().unwrap();
        let mut desktop = crate::song_store::SongStore::open(dir.join("desktop"));
        let song = crate::song_store::write_in(b"song", "Song.mid", &dir.join("desktop")).unwrap();
        desktop.add(song.clone());
        let mut phone = crate::song_store::SongStore::open(dir.join("phone"));
        let wants = BTreeSet::from([song.key.clone()]);

        let none = BTreeSet::new();
        let phone_state = round(&base, &key, &me, State::of(&[4; 32], Default::default()), "phone", &mut phone, &wants);
        let desktop_state = round(&base, &key, &me, State::of(&[4; 32], Default::default()), "desktop", &mut desktop, &none);
        assert!(desktop_state.applied.contains_key(&format!("ready/{}", song.key)), "sent");
        let phone_state = round(&base, &key, &me, phone_state, "phone", &mut phone, &wants);
        assert!(phone.has(&song.key), "fetched");
        let phone_state = round(&base, &key, &me, phone_state, "phone", &mut phone, &none);
        assert!(phone_state.applied.is_empty(), "no one wants it now: gone from the relay");
        let desktop_state = round(&base, &key, &me, desktop_state, "desktop", &mut desktop, &none);
        assert!(desktop_state.applied.is_empty());
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
