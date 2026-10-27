//! Syncing this device with the user's others (`settings_sync.rs`,
//! `playlist_sync.rs`, `song_sync.rs`, `wishlist.rs`), with no window: each
//! blob (the settings, the playlists, the song library and its transfers,
//! the wishlist) syncs with
//! the authority on a thread of its own: at launch, a moment after it
//! changes here, and when the notification stream says another device
//! wrote. Songs move through the relay on a thread of their own too, one
//! after another, and another listens for songs streamed here. The
//! library's and transfers' states live on a thread of their own as well
//! (`songs`): they cover every song.
//!
//! Whatever runs it (the app, `app/sync.rs`) calls `DeviceSync::poll` often,
//! with the authority as it knows it (`Link`) and its songs and playlists
//! and wishlist (`Here`), and acts on what comes back (`Report`): what only it can do,
//! such as putting synced settings into its config.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::Config;
use crate::device::Device;
use crate::frame_timing;
use crate::library::identity::{self, Identity};
use crate::library::{self, client};
use crate::playlist::Library;
use crate::playlist_sync;
use crate::settings_sync::{self, Change, Fields, State, Winner};
use crate::song_store::{SongStore, Stored};
use crate::song_sync::{self, Moved, Relay, Task};
use crate::wishlist::{self, Wishlist};

mod songs;

/// How long after a change before it's sent (a slider being dragged
/// saves over and over).
const SETTLE: Duration = Duration::from_secs(2);
/// How long after a sync that failed before the next try.
const RETRY: Duration = Duration::from_secs(60);
/// How long a song streamed to a device isn't sent to it again, waiting
/// for it to say it has it.
const GIVEN_FOR: Duration = Duration::from_secs(5 * 60);

/// What's synced, each its own blob on the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blob {
    Settings,
    Playlists,
    Library,
    Transfers,
    Wishlist,
}

impl Blob {
    const ALL: [Blob; 5] = [Blob::Settings, Blob::Playlists, Blob::Library, Blob::Transfers, Blob::Wishlist];

    fn name(self) -> &'static str {
        match self {
            Blob::Settings => settings_sync::NAME,
            Blob::Playlists => playlist_sync::NAME,
            Blob::Library => song_sync::LIBRARY,
            Blob::Transfers => song_sync::TRANSFERS,
            Blob::Wishlist => wishlist::NAME,
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
            Blob::Wishlist => "wishlist synced",
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

/// The authority and who's signed in, as whatever runs the sync knows them
/// now.
pub struct Link {
    pub server: String,
    /// Its key (`library::server_key`).
    pub server_key: String,
    pub identity: Option<Identity>,
    /// The biggest song the relay keeps (MB), once its info's been read.
    pub relay_song_mb: Option<u64>,
}

/// The songs, playlists and wishlist here, which syncing reads and changes.
pub struct Here<'a> {
    pub songs: &'a mut SongStore,
    pub playlists: &'a mut Library,
    pub wishlist: &'a mut Wishlist,
}

/// What a sync brought that whatever runs it acts on.
pub enum Report {
    /// The settings synced: put `result` in the config, then hand it back
    /// (`DeviceSync::settings_applied`).
    Settings(Synced),
    /// The playlists synced: hand it back to take them in
    /// (`DeviceSync::apply_playlists`).
    Playlists(Synced),
    /// A song from the other devices: add it to the library.
    Fetched(Stored),
    /// The synced library changed the songs here.
    LibraryChanged,
    /// Something to tell the user.
    Status(String),
    /// What a sync or a song moved did, for the debug toast.
    Debug(String),
}

/// A settings or playlists sync, done: its outcome, and this device's
/// state with what changed here meanwhile merged in (`result`).
pub struct Synced {
    outcome: Outcome,
    state: State,
    pub result: Fields,
}

impl Synced {
    /// What the sync changed here.
    pub fn changes(&self) -> &[Change] {
        &self.outcome.changes
    }

    /// The settings this device syncs.
    pub fn choices(&self) -> &settings_sync::Choices {
        &self.state.choices
    }
}

pub struct DeviceSync {
    /// Sync is on here (the settings' state loads), as of `for_identity`.
    on: bool,
    for_identity: Option<String>,
    /// The kinds of settings this device syncs (`State::choices`).
    choices: settings_sync::Choices,
    channels: [Channel; 5],
    events: (Sender<Done>, Receiver<Done>),
    error: Option<String>,
    /// The songs here or the playlists changed: say so in the library, and
    /// see what to fetch or send.
    songs_dirty: bool,
    /// This device (its ID and name), once it's needed.
    device: Option<Device>,
    /// The devices syncing songs, as the transfers say.
    devices: Vec<song_sync::SyncedDevice>,
    /// The songs in the library no device has, by key.
    unheld: Vec<String>,
    /// Songs being moved through the relay, by key.
    busy: BTreeSet<String>,
    moves: (Sender<Moving>, Receiver<Moving>),
    /// A move failed: no more until then.
    moves_paused: Option<Instant>,
    /// The relay was full: no sending until then (fetching goes on).
    sends_paused: Option<Instant>,
    /// Wants (their paths) a song was streamed for, and when.
    given: BTreeMap<String, Instant>,
    /// Listening for songs streamed here: set to stop.
    listener: Option<Arc<AtomicBool>>,
    /// The songs this device wants, and by relay ID, for the listener.
    wanted: BTreeSet<String>,
    wanted_ids: Arc<Mutex<HashMap<String, String>>>,
    /// How the songs being moved are going.
    progress: Progress,
    /// Keeps the library's and transfers' states.
    worker: (Sender<songs::Request>, Receiver<songs::Reply>),
    /// The songs went to the worker; the next change waits for its plan.
    songs_pending: bool,
    /// What the last poll brought, to hand over.
    reports: Vec<Report>,
    /// Held while an update is offered: nothing starts, and the songs
    /// being moved stop after the one moving.
    held: Arc<AtomicBool>,
}

/// How a round of songs being moved is going: those to fetch or send, how
/// many are done, and of those, fetched and sent.
#[derive(Default)]
struct Progress {
    total: usize,
    done: usize,
    fetched: usize,
    sent: usize,
    /// Songs moved (or found gone, or deleted): see what's next once done.
    changed: bool,
    /// Songs too big for the relay (named), and how big it takes (MB).
    too_big: Vec<String>,
    song_mb: u64,
}

/// How the songs being moved are going.
enum Moving {
    Moved(Moved),
    Finished,
    /// A song streamed here (or one that failed to).
    Received(Moved),
}

impl Default for DeviceSync {
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
            songs_dirty: false,
            device: None,
            devices: Vec::new(),
            unheld: Vec::new(),
            busy: BTreeSet::new(),
            moves: mpsc::channel(),
            moves_paused: None,
            sends_paused: None,
            given: BTreeMap::new(),
            listener: None,
            wanted: BTreeSet::new(),
            wanted_ids: Arc::default(),
            progress: Progress::default(),
            worker,
            songs_pending: false,
            reports: Vec::new(),
            held: Arc::default(),
        }
    }
}

/// How a sync went.
type SyncResult = Result<Outcome, String>;
/// A blob's sync, done.
type Done = (Blob, SyncResult);

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
fn sync(job: Job) -> SyncResult {
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
pub fn describe(what: &str, changes: &[Change], title: impl Fn(&str) -> String) -> String {
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

/// `config` with the settings synced (`Report::Settings`) put in, and how
/// it then reads as synced leaves (for `DeviceSync::settings_applied`).
pub fn synced_config(config: &Config, synced: &Synced) -> Result<(Config, BTreeMap<String, Value>), String> {
    let mut json = config.to_json().map_err(|e| e.to_string())?;
    settings_sync::apply::<Config>(&mut json, &synced.result, synced.choices());
    let config: Config = serde_json::from_value(json).map_err(|e| e.to_string())?;
    let applied = settings_sync::synced_leaves(&config.to_json().unwrap_or_default(), synced.choices());
    Ok((config, applied))
}

/// A song's name, by key.
fn name_of(songs: &SongStore, key: &str) -> String {
    songs.known().find(|(k, _)| *k == key).map_or_else(|| key.to_string(), |(_, s)| s.name.clone())
}

impl DeviceSync {
    pub fn on(&self) -> bool {
        self.on
    }

    /// Held while an update is offered (`poll`).
    pub fn held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }

    /// Why the last sync failed, until one works.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn choices(&self) -> &settings_sync::Choices {
        &self.choices
    }

    /// The devices syncing songs, as the transfers say.
    pub fn devices(&self) -> &[song_sync::SyncedDevice] {
        &self.devices
    }

    /// The songs in the library no device has, by key.
    pub fn unheld(&self) -> &[String] {
        &self.unheld
    }

    /// This device's ID, once it's been needed.
    pub fn device_id(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.id.as_str())
    }

    /// Whether `blob` syncs here.
    fn syncing(&self, blob: Blob) -> bool {
        self.on
            && match blob {
                Blob::Settings => true,
                Blob::Playlists => settings_sync::syncs(&self.choices, &settings_sync::PLAYLISTS),
                Blob::Library | Blob::Transfers => settings_sync::syncs(&self.choices, &settings_sync::SONGS),
                Blob::Wishlist => settings_sync::syncs(&self.choices, &settings_sync::WISHLIST),
            }
    }

    /// Whether the song library syncs here (so a song can be removed from
    /// every device).
    pub fn syncs_library(&self) -> bool {
        self.syncing(Blob::Library)
    }

    /// Whether songs move between this device and the others.
    pub fn syncs_transfers(&self) -> bool {
        self.syncing(Blob::Transfers)
    }

    /// The songs here changed.
    pub fn songs_changed(&mut self) {
        self.songs_dirty = true;
    }

    fn channel(&mut self, blob: Blob) -> &mut Channel {
        &mut self.channels[blob.index()]
    }

    /// The notification stream opened again (`None`), or said synced data
    /// changed: sync what's newer.
    pub fn server_seen(&mut self, changed: Option<library::sync::Changed>) {
        if !self.on {
            return;
        }
        for blob in Blob::ALL {
            let newer = match &changed {
                Some(changed) => changed.name == blob.name() && changed.revision > self.channels[blob.index()].revision,
                None => true,
            };
            if newer && self.syncing(blob) {
                self.channel(blob).wanted = Some(Instant::now());
            }
        }
    }

    /// The playlists changed here: with them syncing, note what changed,
    /// to send once it's settled.
    pub fn playlists_changed(&mut self, songs: &SongStore, playlists: &Library) {
        self.songs_dirty = true;
        if !self.syncing(Blob::Playlists) {
            return;
        }
        let Some(mut state) = State::load(playlist_sync::NAME) else { return };
        if state.record_leaves(playlist_sync::leaves(playlists, songs), settings_sync::now()) {
            if let Err(e) = state.save(playlist_sync::NAME) {
                log::warn!("playlist sync: {e}");
            }
            self.channel(Blob::Playlists).dirty_since = Some(Instant::now());
        }
    }

    /// The wishlist changed here: with it syncing, note what changed, to
    /// send once it's settled.
    pub fn wishlist_changed(&mut self, list: &Wishlist) {
        if !self.syncing(Blob::Wishlist) {
            return;
        }
        let Some(mut state) = State::load(wishlist::NAME) else { return };
        if state.record_leaves(wishlist::leaves(list), settings_sync::now()) {
            if let Err(e) = state.save(wishlist::NAME) {
                log::warn!("wishlist sync: {e}");
            }
            self.channel(Blob::Wishlist).dirty_since = Some(Instant::now());
        }
    }

    /// Start a sync when one's due, and take one that's done: what it
    /// brought that's for whatever runs this to act on. While `held`, none
    /// starts; what was due waits until it's let go.
    pub fn poll(&mut self, link: &Link, here: &mut Here, held: bool) -> Vec<Report> {
        if self.held.swap(held, Ordering::Relaxed) != held {
            if held {
                log::info!("sync held while an update is offered");
                self.stop_listening();
            } else {
                log::info!("sync resumed");
                // (The songs that were waiting to move.)
                self.songs_dirty = true;
            }
        }
        let whose = link.identity.as_ref().map(Identity::public);
        if whose != self.for_identity {
            // (Signed in, out, or as someone else: on only if the state's
            // for this sync key.)
            let state = State::load(settings_sync::NAME);
            self.on = state.is_some() && link.identity.is_some();
            self.choices = state.map(|s| s.choices).unwrap_or_default();
            for blob in [Blob::Settings, Blob::Playlists, Blob::Wishlist] {
                let state = State::load(blob.name());
                let wanted = self.syncing(blob).then(Instant::now);
                *self.channel(blob) = Channel { revision: state.as_ref().map_or(0, |s| s.revision), wanted, ..Channel::default() };
                // (One this device syncs that hasn't started here: sync was
                // on before this version had it.)
                if state.is_none() && self.syncing(blob) {
                    self.start_blob(link, here, blob);
                }
            }
            // (The songs' once the worker has read them: `Reply::Loaded`.)
            for blob in [Blob::Library, Blob::Transfers] {
                *self.channel(blob) = Channel::default();
            }
            self.ask_worker(songs::Request::Reload);
            self.for_identity = whose;
            self.songs_dirty = true;
            self.stop_listening();
        }
        if !self.syncing(Blob::Transfers) {
            self.stop_listening();
        }
        if settings_sync::take_dirty() && self.on {
            self.channel(Blob::Settings).dirty_since = Some(Instant::now());
        }
        while let Ok((blob, result)) = self.events.1.try_recv() {
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
                    Blob::Settings | Blob::Playlists => self.blob_synced(blob, outcome),
                    Blob::Wishlist => self.wishlist_synced(here.wishlist, outcome),
                    Blob::Library | Blob::Transfers => {
                        self.error = None;
                        let songs = self.songs_now(link, here);
                        self.ask_worker(songs::Request::Synced(blob, outcome, songs));
                    }
                },
                Err(e) => {
                    log::info!("{} sync: {e}", blob.name());
                    self.error = Some(e);
                    self.channel(blob).wanted = Some(Instant::now() + RETRY);
                }
            }
        }
        while let Ok(reply) = self.worker.1.try_recv() {
            frame_timing::time("sync worker replies", || self.take_reply(link, here, reply));
        }
        frame_timing::time("poll_moves", || self.poll_moves(here));
        if held {
            return std::mem::take(&mut self.reports);
        }
        if self.songs_dirty && self.on && !self.songs_pending {
            self.songs_dirty = false;
            self.songs_pending = true;
            let songs = self.songs_now(link, here);
            self.ask_worker(songs::Request::Songs(songs));
        }
        for blob in Blob::ALL {
            let channel = &self.channels[blob.index()];
            let settled = channel.dirty_since.is_some_and(|t| t.elapsed() >= SETTLE);
            let due = channel.wanted.is_some_and(|t| Instant::now() >= t);
            if self.syncing(blob) && !channel.running && (settled || due) {
                let channel = self.channel(blob);
                channel.dirty_since = None;
                channel.wanted = None;
                let adopt = channel.adopt;
                frame_timing::time("start_sync", || self.start_sync(link, blob, adopt));
            }
        }
        std::mem::take(&mut self.reports)
    }

    /// The authority, its key, the identity and the sync key, to sync with.
    fn sync_with(&mut self, link: &Link) -> Option<(String, String, Identity, [u8; 32])> {
        let identity = link.identity.clone()?;
        let Some(sync_key) = identity::sync_key() else {
            self.on = false;
            return None;
        };
        Some((link.server.clone(), link.server_key.clone(), identity, sync_key))
    }

    fn start_sync(&mut self, link: &Link, blob: Blob, adopt: bool) {
        if matches!(blob, Blob::Library | Blob::Transfers) {
            let Some((server, server_key, identity, sync_key)) = self.sync_with(link) else { return };
            let choices = self.choices.clone();
            let job = Job { server, server_key, identity, sync_key, blob, base: Fields::new(), local: Fields::new(), adopt, choices };
            self.channel(blob).running = true;
            return self.ask_worker(songs::Request::Sync(Box::new(job)));
        }
        let Some(state) = State::load(blob.name()) else { return };
        let Some((server, server_key, identity, sync_key)) = self.sync_with(link) else { return };
        let choices = self.choices.clone();
        let job = Job { server, server_key, identity, sync_key, blob, base: state.base, local: state.fields, adopt, choices };
        self.channel(blob).running = true;
        let tx = self.events.0.clone();
        std::thread::Builder::new()
            .name(format!("{} sync", blob.name()))
            .spawn(move || {
                let _ = tx.send((blob, sync(job)));
            })
            .ok();
    }

    /// A settings or playlists sync is done: merge in whatever was changed
    /// here while it ran, for whatever runs this to take in.
    fn blob_synced(&mut self, blob: Blob, outcome: Outcome) {
        self.error = None;
        // (Turned off meanwhile.)
        let Some(state) = State::load(blob.name()) else { return };
        let result = match blob {
            Blob::Settings => settings_sync::merge(&outcome.local, &state.fields, &outcome.merged, &state.choices).0,
            _ => settings_sync::merge_where(&outcome.local, &state.fields, &outcome.merged, |_| true).0,
        };
        let synced = Synced { outcome, state, result };
        self.reports.push(if blob == Blob::Settings { Report::Settings(synced) } else { Report::Playlists(synced) });
    }

    /// The settings synced (`Report::Settings`) are in the config, which
    /// now reads as `applied` (`settings_sync::synced_leaves`): keep them
    /// as synced, and send what changed here meanwhile next.
    pub fn settings_applied(&mut self, synced: Synced, applied: BTreeMap<String, Value>) {
        self.blob_taken(Blob::Settings, synced, applied);
    }

    /// Take in the playlists synced (`Report::Playlists`): the IDs of
    /// those replaced or removed, or why they couldn't be saved.
    pub fn apply_playlists(&mut self, synced: Synced, here: &mut Here) -> Result<Vec<String>, String> {
        let touched = playlist_sync::apply(here.playlists, here.songs, &synced.result)?;
        let applied = playlist_sync::leaves(here.playlists, here.songs);
        self.blob_taken(Blob::Playlists, synced, applied);
        Ok(touched)
    }

    /// A wishlist sync is done: take its wishes, with whatever was changed
    /// here while it ran, and send those next.
    fn wishlist_synced(&mut self, list: &mut Wishlist, outcome: Outcome) {
        self.error = None;
        let Some(state) = State::load(wishlist::NAME) else { return };
        let result = settings_sync::merge_where(&outcome.local, &state.fields, &outcome.merged, |_| true).0;
        wishlist::apply(list, &result);
        if !outcome.changes.is_empty() {
            let title = |id: &str| list.wishes.get(id).map_or_else(|| format!("a removed wish ({id})"), |w| w.title.clone());
            self.reports.push(Report::Debug(describe("Wishlist", &outcome.changes, title)));
        }
        let applied = wishlist::leaves(list);
        self.blob_taken(Blob::Wishlist, Synced { outcome, state, result }, applied);
    }

    fn blob_taken(&mut self, blob: Blob, synced: Synced, applied: BTreeMap<String, Value>) {
        let Synced { outcome, mut state, result } = synced;
        state.applied = applied;
        if result != outcome.merged {
            self.channel(blob).dirty_since = Some(Instant::now());
        }
        state.revision = outcome.revision;
        state.base = outcome.merged;
        state.fields = result;
        self.channel(blob).revision = state.revision;
        if let Err(e) = state.save(blob.name()) {
            log::warn!("{} sync: {e}", blob.name());
        }
    }

    /// Start syncing `blob` here: the server's leaves, if it has any, with
    /// this device's added.
    fn start_blob(&mut self, link: &Link, here: &Here, blob: Blob) {
        let leaves = match blob {
            Blob::Settings => return,
            Blob::Playlists => playlist_sync::leaves(here.playlists, here.songs),
            Blob::Wishlist => wishlist::leaves(here.wishlist),
            Blob::Library | Blob::Transfers => {
                let songs = self.songs_now(link, here);
                return self.ask_worker(songs::Request::Start(blob, songs));
            }
        };
        let Some(sync_key) = identity::sync_key() else { return };
        match State::of(&sync_key, leaves).save(blob.name()) {
            Ok(()) => *self.channel(blob) = Channel { wanted: Some(Instant::now()), adopt: true, ..Channel::default() },
            Err(e) => self.error = Some(e),
        }
    }

    /// The songs as they are here, for the worker.
    fn songs_now(&mut self, link: &Link, here: &Here) -> Box<songs::Songs> {
        let _timing = crate::frame_timing::scope("songs for the sync worker");
        let Device { id: device, name: device_name } = self.device.get_or_insert_with(Device::load).clone();
        let wants = song_sync::Wants {
            starred: settings_sync::syncs(&self.choices, &settings_sync::STARRED_SONGS),
            every: settings_sync::syncs(&self.choices, &settings_sync::EVERY_SONG),
        };
        let wanted = song_sync::wanted(here.songs, here.playlists, wants);
        if wanted != self.wanted
            && let Some(sync_key) = identity::sync_key()
        {
            *self.wanted_ids.lock().unwrap_or_else(|p| p.into_inner()) = song_sync::wanted_ids(&sync_key, &wanted);
            self.wanted = wanted.clone();
        }
        if self.syncing(Blob::Transfers) && !self.held() {
            self.listen_for_songs(link, here.songs, &device);
        }
        self.given.retain(|_, at| at.elapsed() < GIVEN_FOR);
        Box::new(songs::Songs {
            store: here.songs.clone(),
            wanted,
            device,
            device_name,
            given: self.given.keys().cloned().collect(),
            busy: self.busy.clone(),
            paused: self.moves_paused.is_some_and(|t| Instant::now() < t),
            library: self.syncing(Blob::Library),
            transfers: self.syncing(Blob::Transfers),
        })
    }

    fn ask_worker(&self, request: songs::Request) {
        let _ = self.worker.0.send(request);
    }

    fn take_reply(&mut self, link: &Link, here: &mut Here, reply: songs::Reply) {
        match reply {
            songs::Reply::Loaded { library, transfers } => {
                for (blob, revision) in [(Blob::Library, library), (Blob::Transfers, transfers)] {
                    match revision {
                        Some(revision) => {
                            let wanted = self.syncing(blob).then(Instant::now);
                            *self.channel(blob) = Channel { revision, wanted, ..Channel::default() };
                        }
                        // (One this device syncs that hasn't started here:
                        // sync was on before this version had it.)
                        None if self.syncing(blob) => self.start_blob(link, here, blob),
                        None => {}
                    }
                }
            }
            songs::Reply::Started(blob) => *self.channel(blob) = Channel { wanted: Some(Instant::now()), adopt: true, ..Channel::default() },
            songs::Reply::Changed(blob) => self.channel(blob).dirty_since = Some(Instant::now()),
            songs::Reply::Planned(tasks) => {
                self.songs_pending = false;
                self.start_moves(link, here.songs, tasks);
            }
            songs::Reply::Skipped(blob) => self.channel(blob).running = false,
            songs::Reply::Taken { blob, revision, library } => {
                self.channel(blob).revision = revision;
                if let Some(library) = library
                    && song_sync::apply_library(here.songs, library)
                {
                    self.reports.push(Report::LibraryChanged);
                }
                self.songs_dirty = true;
            }
            songs::Reply::Devices(devices) => self.devices = devices,
            songs::Reply::Unheld(keys) => self.unheld = keys,
            songs::Reply::Error(e) => self.error = Some(e),
        }
    }

    /// Stop syncing `blob` here.
    fn stop_blob(&mut self, blob: Blob) {
        match blob {
            Blob::Library | Blob::Transfers => self.ask_worker(songs::Request::Stop(blob)),
            _ => settings_sync::stop(blob.name()),
        }
    }

    /// Start moving the songs the worker planned to.
    fn start_moves(&mut self, link: &Link, songs: &SongStore, tasks: Vec<Task>) {
        // (Held: planned again once it's let go.)
        if tasks.is_empty() || self.held() {
            return;
        }
        if link.relay_song_mb == Some(0) {
            // (Its "not found" would read as songs not sent yet.)
            self.error = Some(format!("{} can't move songs between your devices yet: it needs updating.", library::host(&link.server)));
            return;
        }
        // (Not sent to the relay while it was full a moment ago, but still
        // streamed to a device listening.)
        let room = song_sync::Room { song_mb: link.relay_song_mb.unwrap_or(u64::MAX), full: self.sends_paused.is_some() };
        let Some((server, server_key, identity, sync_key)) = self.sync_with(link) else { return };
        let relay = Relay { server, server_key, identity, sync_key, dir: songs.dir().to_path_buf() };
        self.busy = tasks.iter().map(|t| t.key().to_string()).collect();
        self.progress = Progress { total: tasks.iter().filter(|t| !matches!(t, Task::Delete(_))).count(), song_mb: room.song_mb, ..Progress::default() };
        let (tx, held) = (self.moves.0.clone(), Arc::clone(&self.held));
        std::thread::Builder::new()
            .name("song transfers".into())
            .spawn(move || {
                // (One song failing doesn't stop the rest; the failed ones
                // wait for the next try. Held, the rest wait too.)
                song_sync::run_all(&relay, &tasks, room, |moved| tx.send(Moving::Moved(moved)).is_ok() && !held.load(Ordering::Relaxed));
                let _ = tx.send(Moving::Finished);
            })
            .ok();
    }

    /// Take the songs moved since the last poll.
    fn poll_moves(&mut self, here: &mut Here) {
        while let Ok(moving) = self.moves.1.try_recv() {
            let moved = match moving {
                Moving::Moved(moved) => moved,
                Moving::Finished => {
                    self.busy.clear();
                    let Progress { fetched, sent, changed, too_big, song_mb, .. } = std::mem::take(&mut self.progress);
                    // (With nothing moved, the next try waits for something
                    // to change, or a pause to end.)
                    self.songs_dirty |= changed;
                    if !too_big.is_empty() {
                        self.error = Some(format!(
                            "{} is too big to move between your devices while they aren't both open (the server keeps songs up to {song_mb} MB).",
                            too_big.join(", ")
                        ));
                    }
                    let songs = |n: usize| if n == 1 { "1 song".to_string() } else { format!("{n} songs") };
                    let status = match (fetched, sent) {
                        (0, 0) => None,
                        (_, 0) => Some(format!("Downloaded {} from your other devices.", songs(fetched))),
                        (0, _) => Some(format!("Sent {} for your other devices.", songs(sent))),
                        _ => Some(format!("Downloaded {} from your other devices, and sent {}.", songs(fetched), songs(sent))),
                    };
                    self.reports.extend(status.map(Report::Status));
                    continue;
                }
                Moving::Received(moved) => {
                    self.song_received(here.songs, moved);
                    continue;
                }
            };
            let progress = &mut self.progress;
            if let Moved::Full(unsent) = moved {
                // The others take what's waiting first.
                progress.total = progress.total.saturating_sub(unsent).max(progress.done);
                self.sends_paused = Some(Instant::now() + RETRY);
                continue;
            }
            if !matches!(moved, Moved::Deleted) {
                progress.done = (progress.done + 1).min(progress.total);
            }
            progress.changed |= matches!(moved, Moved::Sent(_) | Moved::Streamed { .. } | Moved::Fetched(_) | Moved::Gone(_) | Moved::Deleted);
            let shown = match &moved {
                Moved::Fetched(key) => {
                    progress.fetched += 1;
                    Some(format!("Downloaded {}", name_of(here.songs, key)))
                }
                Moved::Sent(key) | Moved::Streamed { key, .. } => {
                    progress.sent += 1;
                    Some(format!("Sent {}", name_of(here.songs, key)))
                }
                _ => None,
            };
            if let Some(text) = shown {
                let Progress { done, total, .. } = self.progress;
                self.reports.push(Report::Debug(format!("{text} ({done} of {total})")));
            }
            match &moved {
                Moved::Fetched(key) => self.take_fetched(here.songs, key.clone()),
                Moved::Failed(key, e) => {
                    log::info!("moving song {key}: {e}");
                    self.error = Some(format!("couldn't move a song between your devices: {e}"));
                    self.moves_paused = Some(Instant::now() + RETRY);
                }
                Moved::Sent(_) | Moved::Gone(_) => self.ask_worker(songs::Request::Moved(moved.clone())),
                Moved::Streamed { key, to } => {
                    self.given.insert(song_sync::want(to, key), Instant::now());
                }
                Moved::TooBig(key) => {
                    let name = name_of(here.songs, key);
                    self.progress.too_big.push(name);
                }
                Moved::Deleted | Moved::Full(_) => {}
            }
        }
        if self.moves_paused.is_some_and(|t| Instant::now() >= t) {
            self.moves_paused = None;
            self.songs_dirty = true;
        }
        if self.sends_paused.is_some_and(|t| Instant::now() >= t) {
            self.sends_paused = None;
            self.songs_dirty = true;
        }
    }

    /// A song fetched from the other devices joins the library, unless it
    /// was removed from every device as it came (it's deleted again).
    fn take_fetched(&mut self, songs: &mut SongStore, key: String) {
        if songs.is_removed(&key) {
            if let Err(e) = songs.discard(&key) {
                log::warn!("{e:#}");
            }
            return;
        }
        let name = name_of(songs, &key);
        self.reports.push(Report::Fetched(Stored { key, name }));
    }

    /// A song streamed here (or one that failed to).
    fn song_received(&mut self, songs: &mut SongStore, moved: Moved) {
        match moved {
            Moved::Fetched(key) => {
                if !songs.is_removed(&key) {
                    self.reports.push(Report::Status(format!("Downloaded {} from your other devices.", name_of(songs, &key))));
                }
                self.take_fetched(songs, key);
                self.songs_dirty = true;
            }
            Moved::Failed(key, e) => log::info!("a song streamed here ({key}): {e}"),
            _ => {}
        }
    }

    /// Listen for songs streamed here, on a thread of its own, from now
    /// until songs stop syncing (`stop_listening`).
    fn listen_for_songs(&mut self, link: &Link, songs: &SongStore, device: &str) {
        if self.listener.is_some() {
            return;
        }
        let Some((server, server_key, identity, sync_key)) = self.sync_with(link) else { return };
        let relay = Relay { server, server_key, identity, sync_key, dir: songs.dir().to_path_buf() };
        let stop = Arc::new(AtomicBool::new(false));
        let (device, wanted, tx, stopped) = (device.to_string(), Arc::clone(&self.wanted_ids), self.moves.0.clone(), Arc::clone(&stop));
        let spawned = std::thread::Builder::new().name("song inbox".into()).spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                // (Looked up as a song comes, not as listening starts: the
                // songs wanted may change while it waits.)
                let wanted = |id: &str| wanted.lock().unwrap_or_else(|p| p.into_inner()).get(id).cloned();
                match song_sync::listen(&relay, &device, wanted) {
                    Ok(None) => {}
                    Ok(Some(moved)) => {
                        if tx.send(Moving::Received(moved)).is_err() {
                            return;
                        }
                    }
                    // (Half a minute before the next try.)
                    Err(e) => {
                        log::info!("listening for songs: {e}");
                        for _ in 0..30 {
                            if stopped.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            }
        });
        if spawned.is_ok() {
            self.listener = Some(stop);
        }
    }

    fn stop_listening(&mut self) {
        if let Some(stop) = self.listener.take() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Turn sync on here, starting from the config as it is (`config`, as
    /// JSON).
    pub fn turn_on(&mut self, link: &Link, here: &Here, config: &Value) {
        let made = identity::ensure_sync_key().and_then(|sync_key| State::new(&sync_key, config).save(settings_sync::NAME));
        match made {
            Ok(()) => {
                self.on = true;
                self.choices = Default::default();
                self.error = None;
                *self.channel(Blob::Settings) = Channel::default();
                self.start_sync(link, Blob::Settings, true);
                for blob in [Blob::Playlists, Blob::Wishlist, Blob::Library, Blob::Transfers] {
                    if self.syncing(blob) {
                        self.start_blob(link, here, blob);
                    }
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn turn_off(&mut self) {
        for blob in Blob::ALL {
            self.stop_blob(blob);
        }
        self.on = false;
        self.error = None;
    }

    /// Settings (and kinds of songs) ticked or unticked for this device:
    /// `chosen`, each with whether it syncs now, from the config as it is
    /// (`config`, as JSON).
    pub fn choose(&mut self, link: &Link, here: &Here, chosen: Vec<(&'static settings_sync::Setting, bool)>, config: &Value) {
        let Some(mut state) = State::load(settings_sync::NAME) else { return };
        let own = [
            settings_sync::PLAYLISTS.key,
            settings_sync::WISHLIST.key,
            settings_sync::SONGS.key,
            settings_sync::STARRED_SONGS.key,
            settings_sync::EVERY_SONG.key,
        ];
        let any_on = chosen.iter().any(|(setting, on)| *on && !own.contains(&setting.key));
        let toggled = |of: &settings_sync::Setting| chosen.iter().find(|(setting, _)| setting.key == of.key).map(|(_, on)| *on);
        let (playlists, wishes, songs) = (toggled(&settings_sync::PLAYLISTS), toggled(&settings_sync::WISHLIST), toggled(&settings_sync::SONGS));
        if toggled(&settings_sync::EVERY_SONG).is_some() || toggled(&settings_sync::STARRED_SONGS).is_some() {
            self.songs_dirty = true;
        }
        for (setting, on) in chosen {
            state.choose(setting, on, config);
        }
        if let Err(e) = state.save(settings_sync::NAME) {
            self.error = Some(e);
            return;
        }
        self.choices = state.choices;
        if any_on {
            self.channel(Blob::Settings).wanted = Some(Instant::now());
        }
        for (blob, toggled) in [(Blob::Playlists, playlists), (Blob::Wishlist, wishes)] {
            match toggled {
                Some(true) => self.start_blob(link, here, blob),
                Some(false) => self.stop_blob(blob),
                None => {}
            }
        }
        for blob in [Blob::Library, Blob::Transfers] {
            match songs {
                Some(true) => self.start_blob(link, here, blob),
                Some(false) => self.stop_blob(blob),
                None => {}
            }
        }
    }

    /// Stop moving songs to a device (by ID), until it syncs again.
    pub fn forget_device(&mut self, device: &str) {
        self.devices.retain(|d| d.id != device);
        self.ask_worker(songs::Request::Forget(device.to_string()));
    }

    /// Clear the songs no device has from the library, on every device.
    pub fn clear_unheld(&mut self, store: &mut SongStore) {
        let keys = std::mem::take(&mut self.unheld);
        store.forget_absent(keys.iter().map(String::as_str));
        self.ask_worker(songs::Request::Clear(keys));
    }

    /// Songs being moved between this device and the others: how many are
    /// done, of how many.
    pub fn moving(&self) -> Option<(usize, usize)> {
        let Progress { total, done, .. } = self.progress;
        (total > 0).then_some((done, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A device's sync state after a sync, as `DeviceSync::poll` leaves it.
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
        let plan = song_sync::plan(&state.applied, device, store, wanted, &BTreeSet::new(), &BTreeSet::new());
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
