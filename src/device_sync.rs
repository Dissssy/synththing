//! Syncing this device with the user's others (`settings_sync.rs`,
//! `playlist_sync.rs`, `song_sync.rs`, `wishlist.rs`, `image_sync.rs`),
//! with no window: each blob (the settings, the playlists, the song
//! library and its transfers, the wishlist, the songs' play stats, the
//! pictures' transfers) syncs with
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
use crate::image_sync;
use crate::library::identity::{self, Identity};
use crate::library::{self, client};
use crate::playlist::Library;
use crate::play_stats;
use crate::playlist_sync;
use crate::settings_sync::{self, Change, Fields, State, Winner};
use crate::song_store::{SongStore, Stored};
use crate::song_sync::{self, HubLeaf, Moved, Relay, Task};
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
/// How often a device that hasn't reached the hub directly tries again
/// (or one that has checks it still does).
const HUB_CHECK: Duration = Duration::from_secs(10 * 60);
/// How full a shard of a split blob may get (sealed) before the device
/// writing it splits every shard in two: below the most one blob can be
/// (`library::sync::MAX_DATA`), with room for what's added meanwhile.
const SPLIT_AT: usize = library::sync::MAX_DATA * 3 / 4;

/// What's synced, each its own blob on the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blob {
    Settings,
    Playlists,
    Library,
    Transfers,
    Wishlist,
    Stats,
    Images,
}

impl Blob {
    const ALL: [Blob; 7] = [Blob::Settings, Blob::Playlists, Blob::Library, Blob::Transfers, Blob::Wishlist, Blob::Stats, Blob::Images];

    fn name(self) -> &'static str {
        match self {
            Blob::Settings => settings_sync::NAME,
            Blob::Playlists => playlist_sync::NAME,
            Blob::Library => song_sync::LIBRARY,
            Blob::Transfers => song_sync::TRANSFERS,
            Blob::Wishlist => wishlist::NAME,
            Blob::Stats => play_stats::NAME,
            Blob::Images => image_sync::NAME,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Whether it's split over shards on the server, as many as it takes
    /// (`sync_split`): one that grows with the library, past what one blob
    /// can hold.
    fn split(self) -> bool {
        self == Blob::Library
    }

    /// The name on the server of its shard `shard`: `library-3`, say, or
    /// just its name if it isn't split.
    fn server_name(self, shard: usize) -> String {
        if self.split() { format!("{}-{shard}", self.name()) } else { self.name().to_string() }
    }

    /// The name on the server of a split one's shard count:
    /// `library-shards`.
    fn count_name(self) -> String {
        format!("{}-shards", self.name())
    }

    /// Which of its shards the blob named `name` on the server is.
    fn shard_named(self, name: &str) -> Option<usize> {
        if !self.split() {
            return (name == self.name()).then_some(0);
        }
        name.strip_prefix(self.name())?.strip_prefix('-')?.parse().ok()
    }

    /// Its section in the slow-frame log, taking in a sync.
    fn synced_section(self) -> &'static str {
        match self {
            Blob::Settings => "settings synced",
            Blob::Playlists => "playlists synced",
            Blob::Library => "library synced",
            Blob::Transfers => "transfers synced",
            Blob::Wishlist => "wishlist synced",
            Blob::Stats => "play stats synced",
            Blob::Images => "images synced",
        }
    }
}

/// The shard (of `shards`, a power of two) the leaf at `path` is kept in:
/// by its hash, so every device puts it in the same one, they fill evenly,
/// and doubling the count splits each shard `i` into `i` and `i + shards`.
fn shard_of(path: &str, shards: usize) -> usize {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(path.as_bytes());
    u64::from_le_bytes(hash[..8].try_into().unwrap_or_default()) as usize % shards
}

/// One blob's syncing.
#[derive(Default)]
struct Channel {
    /// The server's revision of each of its shards as of the last sync
    /// (none yet: those of a split blob aren't kept between runs).
    revisions: Vec<i64>,
    /// A sync is running.
    running: bool,
    /// The newest revision the server announced of each shard while it
    /// ran: maybe this device's own write, known once it's done.
    heard: Vec<i64>,
    /// It changed here then: send it once it's settled.
    dirty_since: Option<Instant>,
    /// Sync as soon as it can (after this, for a retry).
    wanted: Option<Instant>,
    /// Just started syncing here: the next sync takes the server's leaves
    /// where it has them, until one works.
    adopt: bool,
}

impl Channel {
    /// A blob's channel, as of `revision` (a saved state's): known as its
    /// shard's revision unless it's split (then they're synced afresh).
    fn at(blob: Blob, revision: i64) -> Self {
        let revisions = if blob.split() { Vec::new() } else { vec![revision] };
        Channel { revisions, ..Channel::default() }
    }
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
    /// A playlist's picture came from the other devices.
    ImageFetched,
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
    channels: [Channel; 7],
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
    /// The hub, as the transfers name it.
    hub: Option<HubLeaf>,
    /// Where this device reaches the hub directly, if it does.
    hub_reached: Option<song_sync::Hub>,
    /// When it last tried to (once it's started trying).
    hub_checked: Option<Instant>,
    hub_checking: bool,
    /// This device is the hub (`be_hub`): its leaf, and whether it's still
    /// to be named the hub over another.
    hub_offer: Option<(HubLeaf, bool)>,
    /// The playlists' pictures or their transfers changed: see what to
    /// fetch or send.
    images_dirty: bool,
    /// Pictures being moved through the relay, by key.
    images_busy: BTreeSet<String>,
    /// What came of each picture moved (`None`: they're all done).
    image_moves: (Sender<Option<Moved>>, Receiver<Option<Moved>>),
    /// A picture moved this round: see what's next once they're done.
    images_moved: bool,
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
    /// Where the hub answered, if it did.
    Hub(Option<song_sync::Hub>),
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
            hub: None,
            hub_reached: None,
            hub_checked: None,
            hub_checking: false,
            hub_offer: None,
            images_dirty: false,
            images_busy: BTreeSet::new(),
            image_moves: mpsc::channel(),
            images_moved: false,
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
    /// The server's revision of each shard, and the leaves, now.
    revisions: Vec<i64>,
    merged: Fields,
    /// This device's leaves the sync started from.
    local: Fields,
    changes: Vec<Change>,
}

impl Outcome {
    /// What's kept as the blob's revision: its one shard's, or for one
    /// split over several, how many writes they've had in all.
    fn revision(&self) -> i64 {
        self.revisions.iter().sum()
    }
}

/// Merge with the server's leaves and write the result, until a write
/// isn't beaten by another device's.
fn sync(job: Job) -> SyncResult {
    if job.blob.split() {
        return sync_split(job);
    }
    let name = job.blob.name();
    for _ in 0..5 {
        let (revision, plain) = client::sync_get(&job.server, &job.server_key, &job.identity, &job.sync_key, name)?;
        let server: Option<Fields> = plain.and_then(|p| serde_json::from_slice(&p).ok());
        let (merged, changes) = merge(&job, server.as_ref());
        if server.as_ref() == Some(&merged) {
            return Ok(Outcome { revisions: vec![revision], merged, local: job.local, changes });
        }
        let written = client::sync_write(&job.server, &job.server_key, &job.identity, name, revision, &seal(&job, &merged)?)?;
        if written.stored {
            return Ok(Outcome { revisions: vec![written.revision], merged, local: job.local, changes });
        }
    }
    Err(format!("other devices kept changing the {name}: trying again in a minute"))
}

/// This device's leaves merged with the server's (`None`: it has none yet,
/// so they go up as they are), and what that changed here.
fn merge(job: &Job, server: Option<&Fields>) -> (Fields, Vec<Change>) {
    match server {
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
    }
}

/// Leaves, deflated and sealed for the server.
fn seal(job: &Job, fields: &Fields) -> Result<String, String> {
    let plain = serde_json::to_vec(fields).map_err(|e| e.to_string())?;
    library::sync::seal(&job.sync_key, &library::sync::pack(&plain))
}

/// A split blob's shard count, as the server keeps it (`count_name`).
#[derive(serde::Serialize, serde::Deserialize)]
struct ShardCount {
    shards: usize,
}

/// Sync a blob split over shards: its shard count (one until it's
/// needed more), every shard read and merged as one, and the shards that
/// changed written back, last first. When one would be too full
/// (`SPLIT_AT`), the count doubles first, and leaves move to their new
/// shards: written to those (higher) before they're taken out of the old
/// ones, so a write that fails between leaves them in both, not neither.
/// A leaf found in a shard it doesn't belong in (left there that way, or by
/// a device that hadn't seen the count change) is taken only if the shard
/// it belongs in lacks it, and then moved.
fn sync_split(job: Job) -> SyncResult {
    let name = job.blob.name();
    let count_name = job.blob.count_name();
    'again: for _ in 0..5 {
        let (count_revision, plain) = client::sync_get(&job.server, &job.server_key, &job.identity, &job.sync_key, &count_name)?;
        let count = plain.and_then(|p| serde_json::from_slice::<ShardCount>(&p).ok()).map_or(1, |c| c.shards.max(1).next_power_of_two());
        // (Each read at once: one round trip, however many there are.)
        let read: Vec<Result<(i64, Option<Fields>), String>> = std::thread::scope(|scope| {
            let reads: Vec<_> = (0..count)
                .map(|shard| {
                    let job = &job;
                    scope.spawn(move || {
                        let (revision, plain) = client::sync_get(&job.server, &job.server_key, &job.identity, &job.sync_key, &job.blob.server_name(shard))?;
                        Ok((revision, plain.and_then(|p| serde_json::from_slice(&p).ok())))
                    })
                })
                .collect();
            reads.into_iter().map(|read| read.join().unwrap_or_else(|_| Err("a shard's read failed".into()))).collect()
        });
        let (mut revisions, mut on_server): (Vec<i64>, Vec<Option<Fields>>) = read.into_iter().collect::<Result<Vec<_>, _>>()?.into_iter().unzip();
        let mut server: Option<Fields> = None;
        for (shard, fields) in on_server.iter().enumerate() {
            let Some(fields) = fields else { continue };
            let server = server.get_or_insert_with(Fields::new);
            for (path, field) in fields {
                if shard_of(path, count) == shard {
                    server.insert(path.clone(), field.clone());
                } else {
                    server.entry(path.clone()).or_insert_with(|| field.clone());
                }
            }
        }
        let (merged, changes) = merge(&job, server.as_ref());
        let mut shards = count;
        let parts = loop {
            let mut parts = vec![Fields::new(); shards];
            for (path, field) in &merged {
                parts[shard_of(path, shards)].insert(path.clone(), field.clone());
            }
            let sealed: Vec<String> = parts.iter().map(|part| seal(&job, part)).collect::<Result<_, _>>()?;
            if sealed.iter().all(|data| data.len() <= SPLIT_AT) || shards >= library::sync::MAX_SHARDS {
                break parts.into_iter().zip(sealed).collect::<Vec<_>>();
            }
            shards *= 2;
        };
        if shards != count {
            log::info!("{name} sync: splitting it over {shards} shards (from {count})");
            let plain = serde_json::to_vec(&ShardCount { shards }).map_err(|e| e.to_string())?;
            let data = library::sync::seal(&job.sync_key, &library::sync::pack(&plain))?;
            if !client::sync_write(&job.server, &job.server_key, &job.identity, &count_name, count_revision, &data)?.stored {
                continue 'again;
            }
            revisions.resize(shards, 0);
            on_server.resize(shards, None);
        }
        for (shard, (part, data)) in parts.iter().enumerate().rev() {
            if on_server[shard].as_ref() == Some(part) || (on_server[shard].is_none() && part.is_empty()) {
                continue;
            }
            let written = client::sync_write(&job.server, &job.server_key, &job.identity, &job.blob.server_name(shard), revisions[shard], data)?;
            if !written.stored {
                continue 'again;
            }
            revisions[shard] = written.revision;
        }
        return Ok(Outcome { revisions, merged, local: job.local, changes });
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
                // (Playlists', albums' and artists' pictures.)
                Blob::Images => settings_sync::syncs(&self.choices, &settings_sync::PLAYLISTS) || settings_sync::syncs(&self.choices, &settings_sync::SONGS),
                Blob::Library | Blob::Transfers | Blob::Stats => settings_sync::syncs(&self.choices, &settings_sync::SONGS),
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
        self.images_dirty = true;
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
            let shard = match &changed {
                Some(changed) => match blob.shard_named(&changed.name) {
                    Some(shard) if changed.revision > self.channels[blob.index()].revisions.get(shard).copied().unwrap_or(0) => Some(shard),
                    _ => continue,
                },
                None => None,
            };
            if !self.syncing(blob) {
                continue;
            }
            match (&changed, shard) {
                (Some(changed), Some(shard)) if self.channel(blob).running => {
                    let heard = &mut self.channel(blob).heard;
                    if heard.len() <= shard {
                        heard.resize(shard + 1, 0);
                    }
                    heard[shard] = heard[shard].max(changed.revision);
                }
                _ => self.channel(blob).wanted = Some(Instant::now()),
            }
        }
    }

    /// The playlists changed here: with them syncing, note what changed,
    /// to send once it's settled.
    pub fn playlists_changed(&mut self, songs: &SongStore, playlists: &Library) {
        self.songs_dirty = true;
        self.images_dirty = true;
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

    /// The songs' play stats changed here: with them syncing, note what
    /// changed, to send once it's settled.
    pub fn stats_changed(&mut self, songs: &SongStore) {
        if !self.syncing(Blob::Stats) {
            return;
        }
        let Some(mut state) = State::load(play_stats::NAME) else { return };
        if state.record_leaves(play_stats::leaves(songs.stats()), settings_sync::now()) {
            if let Err(e) = state.save(play_stats::NAME) {
                log::warn!("play stats sync: {e}");
            }
            self.channel(Blob::Stats).dirty_since = Some(Instant::now());
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
            for blob in [Blob::Settings, Blob::Playlists, Blob::Wishlist, Blob::Stats, Blob::Images] {
                let state = State::load(blob.name());
                let wanted = self.syncing(blob).then(Instant::now);
                *self.channel(blob) = Channel { wanted, ..Channel::at(blob, state.as_ref().map_or(0, |s| s.revision)) };
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
                log::info!("{} synced: revision {}, {} changed", blob.name(), outcome.revision(), outcome.changes.len());
            }
            let channel = self.channel(blob);
            channel.running = false;
            let heard = std::mem::take(&mut channel.heard);
            if let Ok(outcome) = &result {
                channel.adopt = false;
                // (Noted now, not once the worker's taken it in: the server
                // announces this write meanwhile.)
                channel.revisions.resize(outcome.revisions.len(), 0);
                for (known, &now) in channel.revisions.iter_mut().zip(&outcome.revisions) {
                    *known = (*known).max(now);
                }
                if heard.iter().zip(&outcome.revisions).any(|(heard, now)| heard > now) {
                    channel.wanted = Some(Instant::now());
                }
            }
            match result {
                Ok(outcome) => match blob {
                    Blob::Settings | Blob::Playlists => self.blob_synced(blob, outcome),
                    Blob::Wishlist => self.wishlist_synced(here.wishlist, outcome),
                    Blob::Stats => self.stats_synced(here.songs, outcome),
                    Blob::Images => self.images_synced(outcome),
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
        self.poll_image_moves();
        if held {
            return std::mem::take(&mut self.reports);
        }
        self.check_hub();
        if self.images_dirty && self.syncing(Blob::Images) && self.images_busy.is_empty() {
            self.images_dirty = false;
            self.move_images(link, here.playlists, here.songs);
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

    /// The pictures' transfers synced: take them, with whatever was changed
    /// here while they ran, then see what to fetch or send.
    fn images_synced(&mut self, outcome: Outcome) {
        self.error = None;
        let Some(state) = State::load(image_sync::NAME) else { return };
        let result = settings_sync::merge_where(&outcome.local, &state.fields, &outcome.merged, |_| true).0;
        let applied = result.iter().map(|(path, field)| (path.clone(), field.value.clone())).collect();
        self.blob_taken(Blob::Images, Synced { outcome, state, result }, applied);
        self.images_dirty = true;
    }

    /// Say which pictures the playlists here want, and move those the
    /// transfers say to, one after another, on a thread of their own.
    fn move_images(&mut self, link: &Link, playlists: &Library, songs: &SongStore) {
        let Some(mut state) = State::load(image_sync::NAME) else { return };
        let device = self.device.get_or_insert_with(Device::load).id.clone();
        let plan = image_sync::plan(&state.applied, &device, &image_sync::wanted(playlists, songs), &self.images_busy);
        if state.record_leaves(plan.leaves, settings_sync::now()) {
            if let Err(e) = state.save(image_sync::NAME) {
                log::warn!("image sync: {e}");
            }
            self.channel(Blob::Images).dirty_since = Some(Instant::now());
        }
        if plan.tasks.is_empty() || self.held() {
            return;
        }
        let (Ok(dir), Some((server, server_key, identity, sync_key))) = (crate::images::dir(), self.sync_with(link)) else { return };
        let relay = Relay { server, server_key, identity, sync_key, dir, device, hub: None };
        self.images_busy = plan.tasks.iter().map(|t| t.key().to_string()).collect();
        let tasks = plan.tasks;
        let tx = self.image_moves.0.clone();
        let spawned = std::thread::Builder::new().name("image transfers".into()).spawn(move || {
            for task in &tasks {
                if tx.send(Some(song_sync::run(&relay, task))).is_err() {
                    return;
                }
            }
            let _ = tx.send(None);
        });
        if let Err(e) = spawned {
            log::warn!("couldn't start moving pictures: {e}");
            self.images_busy.clear();
        }
    }

    /// Take what came of the pictures moved: mark those sent ready, and once
    /// they're all done, see what's next if any moved (one that failed
    /// waits for the next change).
    fn poll_image_moves(&mut self) {
        while let Ok(moved) = self.image_moves.1.try_recv() {
            let Some(moved) = moved else {
                self.images_busy.clear();
                self.images_dirty |= std::mem::take(&mut self.images_moved);
                continue;
            };
            match &moved {
                Moved::Failed(key, e) => log::info!("moving the picture {key}: {e}"),
                _ => self.images_moved = true,
            }
            if let Moved::Fetched(_) = moved {
                self.reports.push(Report::ImageFetched);
            }
            let Some(mut state) = State::load(image_sync::NAME) else { continue };
            if state.record_leaves(song_sync::after(state.applied.clone(), &moved), settings_sync::now()) {
                if let Err(e) = state.save(image_sync::NAME) {
                    log::warn!("image sync: {e}");
                }
                self.channel(Blob::Images).dirty_since = Some(Instant::now());
            }
        }
    }

    /// A play stats sync is done: take the counts, with whatever was
    /// counted here while it ran, and send those next.
    fn stats_synced(&mut self, songs: &mut SongStore, outcome: Outcome) {
        self.error = None;
        let Some(state) = State::load(play_stats::NAME) else { return };
        let result = settings_sync::merge_where(&outcome.local, &state.fields, &outcome.merged, |_| true).0;
        songs.set_stats(play_stats::read(&result));
        let applied = play_stats::leaves(songs.stats());
        self.blob_taken(Blob::Stats, Synced { outcome, state, result }, applied);
    }

    fn blob_taken(&mut self, blob: Blob, synced: Synced, applied: BTreeMap<String, Value>) {
        let Synced { outcome, mut state, result } = synced;
        state.applied = applied;
        if result != outcome.merged {
            self.channel(blob).dirty_since = Some(Instant::now());
        }
        state.revision = outcome.revision();
        state.base = outcome.merged;
        state.fields = result;
        self.channel(blob).revisions = outcome.revisions;
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
            Blob::Stats => play_stats::leaves(here.songs.stats()),
            Blob::Images => {
                let device = self.device.get_or_insert_with(Device::load).id.clone();
                image_sync::plan(&BTreeMap::new(), &device, &image_sync::wanted(here.playlists, here.songs), &BTreeSet::new()).leaves
            }
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
        let hub_offer = self.hub_offer.clone();
        Box::new(songs::Songs {
            store: here.songs.clone(),
            wanted,
            device,
            device_name,
            given: self.given.keys().cloned().collect(),
            busy: self.busy.clone(),
            paused: self.moves_paused.is_some_and(|t| Instant::now() < t),
            hub_direct: self.hub_reached.is_some(),
            hub_only: settings_sync::syncs(&self.choices, &settings_sync::HUB_ONLY),
            hub: hub_offer,
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
                            *self.channel(blob) = Channel { wanted, ..Channel::at(blob, revision) };
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
            songs::Reply::Taken { blob, revisions, library } => {
                self.channel(blob).revisions = revisions;
                if let Some(library) = library
                    && song_sync::apply_library(here.songs, library)
                {
                    self.reports.push(Report::LibraryChanged);
                }
                self.songs_dirty = true;
            }
            songs::Reply::Devices(devices, hub) => {
                self.devices = devices;
                // (Named the hub: from now on, another named after it is.)
                if let Some((offered, take_over)) = &mut self.hub_offer
                    && hub.as_ref().is_some_and(|hub| hub.device == offered.device)
                {
                    *take_over = false;
                }
                if !matches!((&hub, &self.hub), (Some(new), Some(old)) if new.same_place(old)) {
                    // (Reached again, wherever it is now.)
                    self.hub_reached = None;
                    self.hub_checked = None;
                }
                self.hub = hub;
            }
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
        let device = self.device.get_or_insert_with(Device::load).id.clone();
        let relay = Relay { server, server_key, identity, sync_key, dir: songs.dir().to_path_buf(), device, hub: self.hub_reached.clone() };
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

    /// See whether the hub answers directly, on a thread of its own, if
    /// it's time to (and this device isn't it).
    fn check_hub(&mut self) {
        let Some(hub) = &self.hub else { return };
        let me = &self.device.get_or_insert_with(Device::load).id;
        if hub.device == *me || self.hub_checking || !self.syncing(Blob::Transfers) || self.hub_checked.is_some_and(|t| t.elapsed() < HUB_CHECK) {
            return;
        }
        let (hub, tx) = (hub.clone(), self.moves.0.clone());
        let spawned = std::thread::Builder::new().name("hub check".into()).spawn(move || {
            let _ = tx.send(Moving::Hub(song_sync::reach(&hub)));
        });
        self.hub_checking = spawned.is_ok();
        self.hub_checked = Some(Instant::now());
    }

    /// Make this device the hub (`hub.rs`), named so in the transfers over
    /// any other: `leaf` says where it's reached.
    pub fn be_hub(&mut self, leaf: HubLeaf) {
        self.hub_offer = Some((leaf, true));
        self.songs_dirty = true;
    }

    /// What the server found of this hub's addresses from the internet,
    /// for the other devices to show.
    pub fn hub_checks(&mut self, checks: Vec<song_sync::AddressCheck>) {
        if let Some((leaf, _)) = &mut self.hub_offer {
            leaf.checks = checks;
            self.songs_dirty = true;
        }
    }

    /// Check whether this device reaches the hub directly now, not when it's
    /// next due.
    pub fn check_hub_now(&mut self) {
        self.hub_checked = None;
    }

    /// Whether it's checking that now.
    pub fn checking_hub(&self) -> bool {
        self.hub_checking
    }

    /// The hub, as the transfers name it.
    pub fn hub(&self) -> Option<&HubLeaf> {
        self.hub.as_ref()
    }

    /// Where this device reaches the hub directly, if it does.
    pub fn hub_reached(&self) -> Option<&song_sync::Hub> {
        self.hub_reached.as_ref()
    }

    /// A song sent straight here (to the hub, `hub_server.rs`), now in the
    /// library's folder.
    pub fn received(&mut self, songs: &mut SongStore, key: String) {
        self.song_received(songs, Moved::Fetched(key));
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
                Moving::Hub(reached) => {
                    self.hub_checking = false;
                    if reached.is_some() != self.hub_reached.is_some() {
                        // (Whether it takes songs from the hub changed.)
                        self.songs_dirty = true;
                    }
                    self.hub_reached = reached;
                    continue;
                }
            };
            if let Moved::HubLost(e) = &moved {
                log::info!("the hub: {e}");
                self.hub_reached = None;
                self.hub_checked = Some(Instant::now());
                self.songs_dirty = true;
                continue;
            }
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
            progress.changed |= matches!(moved, Moved::Sent(_) | Moved::Streamed { .. } | Moved::Elsewhere { .. } | Moved::Fetched(_) | Moved::Gone(_) | Moved::Deleted);
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
                Moved::Streamed { key, to } | Moved::Elsewhere { key, to } => {
                    self.given.insert(song_sync::want(to, key), Instant::now());
                }
                Moved::TooBig(key) => {
                    let name = name_of(here.songs, key);
                    self.progress.too_big.push(name);
                }
                Moved::Deleted | Moved::Full(_) | Moved::HubLost(_) => {}
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
        let relay = Relay { server, server_key, identity, sync_key, dir: songs.dir().to_path_buf(), device: device.to_string(), hub: None };
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
                for blob in [Blob::Playlists, Blob::Wishlist, Blob::Stats, Blob::Images, Blob::Library, Blob::Transfers] {
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
            settings_sync::HUB_ONLY.key,
        ];
        let any_on = chosen.iter().any(|(setting, on)| *on && !own.contains(&setting.key));
        let toggled = |of: &settings_sync::Setting| chosen.iter().find(|(setting, _)| setting.key == of.key).map(|(_, on)| *on);
        let (playlists, wishes, songs) = (toggled(&settings_sync::PLAYLISTS), toggled(&settings_sync::WISHLIST), toggled(&settings_sync::SONGS));
        if [&settings_sync::EVERY_SONG, &settings_sync::STARRED_SONGS, &settings_sync::HUB_ONLY].into_iter().any(|s| toggled(s).is_some()) {
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
        // (The pictures sync while the playlists or the songs do.)
        let images = Some(self.syncing(Blob::Images)).filter(|&on| on != State::load(image_sync::NAME).is_some());
        for (blob, toggled) in [(Blob::Playlists, playlists), (Blob::Images, images), (Blob::Wishlist, wishes)] {
            match toggled {
                Some(true) => self.start_blob(link, here, blob),
                Some(false) => self.stop_blob(blob),
                None => {}
            }
        }
        for blob in [Blob::Library, Blob::Transfers, Blob::Stats] {
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
        state.revision = outcome.revision();
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
        assert_eq!(up.revisions, [1]);
        assert!(up.changes.is_empty());
        let mut desktop = after(desktop, &up);
        assert!(!desktop.fields.contains_key("buffer_ms"), "the device's own");

        // The phone turns it on: it takes the desktop's.
        let phone = State::new(&[4; 32], &json!({ "theme": "light", "shuffle": true, "mini_player": true }));
        let down = sync(job(&base, &key, &me, &phone, true)).unwrap();
        assert_eq!(down.merged["theme"].value, json!("dark"));
        assert_eq!(down.merged["mini_player"].value, json!(true), "the phone's own setting the server lacked goes up");
        assert_eq!(down.revisions, [2]);
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

    #[test]
    fn a_big_library_syncs_in_shards() {
        use sha2::Digest;
        let dir = std::env::temp_dir().join(format!("synththing-library-shards-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.json"), r#"{ "previews": false, "check_uploads": false, "sync": true }"#).unwrap();
        let running = library::server::start(&dir, Some("127.0.0.1:0".into())).unwrap();
        let base = format!("http://{}", running.addr);
        let key = client::info(&base).unwrap().key;
        let me = Identity::generate().unwrap();
        let hash = |text: String| library::hex(&sha2::Sha256::digest(text.as_bytes()));

        // More songs than one blob holds (hashes don't deflate).
        let songs: BTreeMap<String, Value> =
            (0..12_000).map(|i| (format!("{}.flac", hash(format!("song {i}"))), json!({ "name": format!("Song {i}"), "mbc": hash(format!("recording {i}")) }))).collect();
        let hub = State::of(&[4; 32], songs.clone());
        let whole = library::sync::seal(&[4; 32], &library::sync::pack(&serde_json::to_vec(&hub.fields).unwrap())).unwrap();
        assert!(whole.len() > library::sync::MAX_DATA, "too big for one blob");
        let library_job = |state: &State, adopt| Job { blob: Blob::Library, ..job(&base, &key, &me, state, adopt) };
        let up = sync(library_job(&hub, true)).unwrap();
        let shards = up.revisions.len();
        assert!(shards > 1 && shards.is_power_of_two(), "split as it needed: {shards}");
        assert_eq!(up.revisions, vec![1; shards], "each shard written once");
        let mut hub = after(hub, &up);

        // The desktop takes them all, and adds one: only its shard is written.
        let desktop = State::of(&[4; 32], Default::default());
        let down = sync(library_job(&desktop, true)).unwrap();
        assert_eq!(down.merged.len(), songs.len());
        let mut desktop = after(desktop, &down);
        let new = format!("{}.flac", hash("new".into()));
        let mut leaves = desktop.applied.clone();
        leaves.insert(new.clone(), json!({ "name": "New" }));
        desktop.record_leaves(leaves, settings_sync::now());
        let added = sync(library_job(&desktop, false)).unwrap();
        let shard = shard_of(&new, shards);
        assert!(added.revisions.iter().enumerate().all(|(i, &r)| r == if i == shard { 2 } else { 1 }), "{:?}", added.revisions);
        let desktop = after(desktop, &added);

        // The hub hears of it.
        let heard = sync(library_job(&hub, false)).unwrap();
        assert_eq!(heard.changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), [new.as_str()]);
        hub = after(hub, &heard);
        assert_eq!(hub.fields.len(), songs.len() + 1);

        // The library grows past what those shards hold: they split again,
        // and the other device finds every song where it now is.
        let more: BTreeMap<String, Value> = (12_000..30_000)
            .map(|i| (format!("{}.flac", hash(format!("song {i}"))), json!({ "name": format!("Song {i}"), "mbc": hash(format!("recording {i}")) })))
            .collect();
        let mut leaves = hub.applied.clone();
        leaves.extend(more.clone());
        hub.record_leaves(leaves, settings_sync::now());
        let grown = sync(library_job(&hub, false)).unwrap();
        assert!(grown.revisions.len() > shards, "split again: {}", grown.revisions.len());
        let caught_up = sync(library_job(&desktop, false)).unwrap();
        assert_eq!(caught_up.merged.len(), songs.len() + more.len() + 1);
        assert_eq!(caught_up.changes.len(), more.len(), "only the new songs are new to it");
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One round on a device: sync the transfers, plan, do the tasks, and
    /// sync what they changed. Its state after.
    fn round(base: &str, key: &str, me: &Identity, state: State, device: &str, store: &mut crate::song_store::SongStore, wanted: &BTreeSet<String>) -> State {
        let relay = Relay { server: base.into(), server_key: key.into(), identity: me.clone(), sync_key: [4; 32], dir: store.dir().to_path_buf(), device: "test".into(), hub: None };
        let synced = sync(Job { blob: Blob::Transfers, ..job(base, key, me, &state, false) }).unwrap();
        let mut state = after(state, &synced);
        let plan = song_sync::plan(&state.applied, device, |k| store.has(k), wanted, &BTreeSet::new(), &BTreeSet::new(), None);
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
