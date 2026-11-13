//! The song library's and its transfers' sync states, kept on a thread of
//! their own. Noting the songs here in them, planning what to move and
//! taking in each sync go over every song in the library, which takes
//! longer than a frame once there are thousands. `DeviceSync` sends what
//! changed and takes back what it has to act on (`DeviceSync::poll`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::{sync, Blob, Done, Job, Outcome};
use crate::library::identity;
use crate::settings_sync::{self, State};
use crate::song_store::SongStore;
use crate::song_sync::{self, HubLeaf, HubPlan, Moved, SyncedDevice, SyncedLibrary, Task};

/// The songs as the device has them, for the worker to go by.
pub(super) struct Songs {
    pub store: SongStore,
    /// The songs in the playlists, and which others this device takes:
    /// the worker works out the songs it wants from them
    /// (`song_sync::wanted`), off the UI thread.
    pub listed: Vec<String>,
    pub wants: song_sync::Wants,
    /// This device's ID and name (`Device`).
    pub device: String,
    pub device_name: String,
    /// Wants (their paths) a song was just streamed for: the device it
    /// went to says it has it next.
    pub given: BTreeSet<String>,
    /// Songs being moved now: left alone.
    pub busy: BTreeSet<String>,
    /// A move failed a moment ago: plan, but move nothing yet.
    pub paused: bool,
    /// This device reaches the hub directly now.
    pub hub_direct: bool,
    /// This device moves songs only to and from the hub, once there is one
    /// (`settings_sync::HUB_ONLY`).
    pub hub_only: bool,
    /// This device is the hub: its leaf, and whether to name it the hub
    /// even if another is (as it starts).
    pub hub: Option<(HubLeaf, bool)>,
    pub library: bool,
    pub transfers: bool,
    /// How many synced libraries (`Reply::Taken`) the store has taken: one
    /// the worker sent since isn't in it yet.
    pub libraries_taken: u64,
}

pub(super) enum Request {
    /// Signed in, out, or as someone else: read the states again.
    Reload,
    /// Start syncing the blob here (the songs, for the library's leaves).
    Start(Blob, Box<Songs>),
    /// Stop syncing the blob here.
    Stop(Blob),
    /// The songs changed: note them, and plan what to move.
    Songs(Box<Songs>),
    /// A song was moved.
    Moved(Moved),
    /// Forget a device (by ID): its wants, and it in the list.
    Forget(String),
    /// Clear these songs from the library: no device has them.
    Clear(Vec<String>),
    /// Sync, with this device's leaves (the job's `base` and `local`) put in.
    Sync(Box<Job>),
    /// A sync is done: take it in, with the songs as they are now noted
    /// first.
    Synced(Blob, Outcome, Box<Songs>),
}

pub(super) enum Reply {
    /// The states were read again: each one's revision, if there is one.
    Loaded { library: Option<i64>, transfers: Option<i64> },
    /// The blob started syncing here.
    Started(Blob),
    /// The blob changed here: send it once it's settled.
    Changed(Blob),
    /// What to move now (nothing while moves are going or paused).
    Planned(Vec<Task>),
    /// The devices in the transfers, as of now, and the hub, as the
    /// server last had it (one named here waits for it to say so).
    Devices(Vec<SyncedDevice>, Option<HubLeaf>),
    /// The songs in the library no device has (`song_sync::unheld`).
    Unheld(Vec<String>),
    /// The blob's sync didn't start: it has no state (turned off meanwhile).
    Skipped(Blob),
    /// A sync was taken in: the server's revision of each shard now, and
    /// for the library, its leaves to apply to the songs, read (only when
    /// they say more than the songs here already do).
    Taken { blob: Blob, revisions: Vec<i64>, library: Option<SyncedLibrary> },
    Error(String),
}

/// Starts the worker: syncs it starts report to `events`.
/// It keeps `wanted_ids`, the songs this device wants by their relay IDs,
/// for the song inbox.
pub(super) fn spawn(events: Sender<Done>, wanted_ids: Arc<Mutex<HashMap<String, String>>>) -> (Sender<Request>, Receiver<Reply>) {
    let (requests, inbox) = mpsc::channel();
    let (replies, outbox) = mpsc::channel();
    std::thread::Builder::new()
        .name("song sync".into())
        .spawn(move || {
            let mut worker = Worker { library: None, transfers: None, wanted: BTreeSet::new(), libraries_sent: 0, wanted_ids, events, replies };
            for request in inbox {
                worker.handle(request);
            }
        })
        .ok();
    (requests, outbox)
}

struct Worker {
    library: Option<State>,
    transfers: Option<State>,
    /// The songs this device wants, as of the last songs sent.
    wanted: BTreeSet<String>,
    /// How many synced libraries it's sent to be taken (`Songs::libraries_taken`).
    libraries_sent: u64,
    wanted_ids: Arc<Mutex<HashMap<String, String>>>,
    events: Sender<Done>,
    replies: Sender<Reply>,
}

impl Worker {
    fn handle(&mut self, request: Request) {
        if let Request::Start(_, songs) | Request::Songs(songs) | Request::Synced(_, _, songs) = &request {
            self.note_wanted(songs);
        }
        match request {
            Request::Reload => {
                // (Another sync key hashes the wanted songs to other IDs.)
                self.wanted.clear();
                self.library = State::load(song_sync::LIBRARY);
                self.transfers = State::load(song_sync::TRANSFERS);
                let revision = |state: &Option<State>| state.as_ref().map(|s| s.revision);
                self.reply(Reply::Loaded { library: revision(&self.library), transfers: revision(&self.transfers) });
            }
            Request::Start(blob, songs) => self.start(blob, &songs),
            Request::Stop(blob) => {
                settings_sync::stop(blob.name());
                match blob {
                    Blob::Library => self.library = None,
                    _ => self.transfers = None,
                }
            }
            Request::Songs(songs) => {
                self.record_library(&songs);
                let planned = self.plan(&songs);
                self.reply(planned);
            }
            Request::Moved(moved) => {
                let Some(state) = &mut self.transfers else { return };
                let leaves = song_sync::after(state.applied.clone(), &moved);
                if state.record_leaves(leaves, settings_sync::now()) {
                    self.save(Blob::Transfers);
                    self.reply(Reply::Changed(Blob::Transfers));
                }
            }
            Request::Forget(device) => {
                let Some(state) = &mut self.transfers else { return };
                let leaves = song_sync::forget(&state.applied, &device);
                let devices = state.record_leaves(leaves, settings_sync::now()).then(|| (song_sync::devices(&state.fields), song_sync::hub(&state.base)));
                if let Some((devices, hub)) = devices {
                    self.save(Blob::Transfers);
                    self.reply(Reply::Changed(Blob::Transfers));
                    self.reply(Reply::Devices(devices, hub));
                }
            }
            Request::Clear(keys) => {
                let Some(state) = &mut self.library else { return };
                let leaves = song_sync::clear(&state.applied, &keys);
                if state.record_leaves(leaves, settings_sync::now()) {
                    self.save(Blob::Library);
                    self.reply(Reply::Changed(Blob::Library));
                }
            }
            Request::Sync(mut job) => {
                let Some(state) = self.state(job.blob) else {
                    return self.reply(Reply::Skipped(job.blob));
                };
                job.base = state.base.clone();
                job.local = state.fields.clone();
                let events = self.events.clone();
                std::thread::Builder::new()
                    .name(format!("{} sync", job.blob.name()))
                    .spawn(move || {
                        let _ = events.send((job.blob, sync(*job)));
                    })
                    .ok();
            }
            Request::Synced(blob, outcome, songs) => self.synced(blob, outcome, &songs),
        }
    }

    /// Work out the songs this device wants, and their relay IDs if they
    /// changed (hashing each one, which took tens of milliseconds a frame
    /// on a phone when it ran on the UI thread).
    fn note_wanted(&mut self, songs: &Songs) {
        let wanted = song_sync::wanted(&songs.store, &songs.listed, songs.wants);
        if wanted != self.wanted {
            if let Some(sync_key) = identity::sync_key() {
                *self.wanted_ids.lock().unwrap_or_else(|p| p.into_inner()) = song_sync::wanted_ids(&sync_key, &wanted);
            }
            self.wanted = wanted;
        }
    }

    fn reply(&self, reply: Reply) {
        let _ = self.replies.send(reply);
    }

    fn state(&mut self, blob: Blob) -> Option<&mut State> {
        match blob {
            Blob::Library => self.library.as_mut(),
            Blob::Transfers => self.transfers.as_mut(),
            Blob::Settings | Blob::Playlists | Blob::Wishlist | Blob::Stats | Blob::Images => None,
        }
    }

    fn save(&mut self, blob: Blob) {
        if let Some(state) = self.state(blob)
            && let Err(e) = state.save(blob.name())
        {
            let blob = blob.name();
            log::warn!(blob; "{blob} sync: {e}");
        }
    }

    /// The library's leaves, those of every song here and the tags, from
    /// none synced yet; the transfers', none.
    fn start(&mut self, blob: Blob, songs: &Songs) {
        let Some(sync_key) = identity::sync_key() else { return };
        let leaves = match blob {
            Blob::Library => song_sync::library_leaves(&songs.store, &Default::default()),
            _ => Default::default(),
        };
        let state = State::of(&sync_key, leaves);
        if let Err(e) = state.save(blob.name()) {
            return self.reply(Reply::Error(e));
        }
        match blob {
            Blob::Library => self.library = Some(state),
            _ => self.transfers = Some(state),
        }
        self.reply(Reply::Started(blob));
    }

    /// Note the songs here in the synced library.
    /// Not from songs that haven't taken the last library sent yet: those
    /// would say a song removed from every device meanwhile is still here
    /// and not removed, bringing it back everywhere.
    fn record_library(&mut self, songs: &Songs) {
        if !songs.library || songs.libraries_taken != self.libraries_sent {
            return;
        }
        let Some(state) = &mut self.library else { return };
        let leaves = song_sync::library_leaves(&songs.store, &state.applied);
        if state.record_leaves(leaves, settings_sync::now()) {
            self.save(Blob::Library);
            self.reply(Reply::Changed(Blob::Library));
        }
    }

    /// Say which songs this device wants, and what to move.
    fn plan(&mut self, songs: &Songs) -> Reply {
        let nothing = Reply::Planned(Vec::new());
        if !songs.transfers {
            return nothing;
        }
        let Some(state) = &mut self.transfers else { return nothing };
        // (Devices gone quiet are forgotten.)
        let now = settings_sync::now();
        let mut applied = state.applied.clone();
        for device in song_sync::gone_quiet(&state.fields, &songs.device, now) {
            applied = song_sync::forget(&applied, &device);
        }
        let hub = song_sync::hub(&state.fields);
        let hub_plan = hub.as_ref().map(|hub| HubPlan { device: hub.device.clone(), direct: songs.hub_direct, only: songs.hub_only });
        let pulls = hub_plan.as_ref().is_some_and(|hub| hub.direct && hub.device != songs.device);
        let hub_only = hub_plan.as_ref().is_some_and(|hub| hub.only && hub.device != songs.device);
        let mut plan = song_sync::plan(&applied, &songs.device, |k| songs.store.has(k), &self.wanted, &songs.busy, &songs.given, hub_plan.as_ref());
        song_sync::note_device(&mut plan.leaves, &songs.device, &songs.device_name, pulls, hub_only, now);
        song_sync::note_have(&mut plan.leaves, &songs.device, &songs.store);
        if let Some((leaf, take_over)) = &songs.hub
            && (*take_over || hub.as_ref().is_none_or(|hub| hub.device == songs.device))
        {
            song_sync::note_hub(&mut plan.leaves, leaf);
        }
        let changed = state.record_leaves(plan.leaves, now);
        let devices = song_sync::devices(&state.fields);
        let hub = song_sync::hub(&state.base);
        let unheld = song_sync::unheld(&state.fields, &songs.device, &songs.store);
        if changed {
            self.save(Blob::Transfers);
            self.reply(Reply::Changed(Blob::Transfers));
        }
        self.reply(Reply::Devices(devices, hub));
        self.reply(Reply::Unheld(unheld));
        if plan.tasks.is_empty() || !songs.busy.is_empty() || songs.paused {
            return nothing;
        }
        Reply::Planned(plan.tasks)
    }

    /// A library or transfers sync is done: take its leaves, with whatever
    /// was changed here while it ran.
    fn synced(&mut self, blob: Blob, outcome: Outcome, songs: &Songs) {
        if blob == Blob::Library {
            // (What changed here meanwhile goes in first, not to be taken
            // for something another device removed.)
            self.record_library(songs);
        }
        let Some(state) = self.state(blob) else { return };
        let (result, _) = settings_sync::merge_where(&outcome.local, &state.fields, &outcome.merged, |_| true);
        let changed = result != outcome.merged;
        let applied: BTreeMap<String, Value> = result.iter().map(|(k, f)| (k.clone(), f.value.clone())).collect();
        // (The songs here are as `applied` says, just noted: with nothing
        // new from the other devices, there's nothing to apply.)
        let library = (blob == Blob::Library && applied != state.applied).then(|| song_sync::read_library(&result));
        state.applied = applied;
        state.revision = outcome.revision();
        state.base = outcome.merged;
        state.fields = result;
        self.save(blob);
        if changed {
            self.reply(Reply::Changed(blob));
        }
        if library.is_some() {
            self.libraries_sent += 1;
        }
        self.reply(Reply::Taken { blob, revisions: outcome.revisions, library });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_sync::{Field, Fields};

    fn songs(store: &SongStore, libraries_taken: u64) -> Box<Songs> {
        Box::new(Songs {
            store: store.clone(),
            listed: Vec::new(),
            wants: song_sync::Wants { starred: false, every: false },
            device: "phone".into(),
            device_name: "Phone".into(),
            given: BTreeSet::new(),
            busy: BTreeSet::new(),
            paused: false,
            hub_direct: false,
            hub_only: false,
            hub: None,
            library: true,
            transfers: false,
            libraries_taken,
        })
    }

    /// A phone that was offline comes back to a song removed from every
    /// device meanwhile: songs it noted before taking that in don't bring
    /// the song back.
    #[test]
    fn a_song_removed_meanwhile_stays_removed() {
        let root = std::env::temp_dir().join(format!("synththing-removed-meanwhile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        crate::config::set_config_dir(&root.join("config")).unwrap();
        let dir = root.join("phone");
        let mut store = SongStore::open(dir.clone());
        let song = crate::song_store::write_in(b"live", "Song (live).mid", &dir).unwrap();
        store.add(song.clone());
        let key = song.key.clone();
        let removed = format!("removed/{key}");

        let before = song_sync::library_leaves(&store, &BTreeMap::new());
        let (replies, outbox) = mpsc::channel();
        let (events, _) = mpsc::channel();
        let mut worker = Worker {
            library: Some(State::of(&[4; 32], before.clone())),
            transfers: None,
            wanted: BTreeSet::new(),
            libraries_sent: 0,
            wanted_ids: Arc::default(),
            events,
            replies,
        };
        let local = worker.library.as_ref().unwrap().fields.clone();

        // The desktop removed it everywhere while the phone was away.
        let mut merged: Fields = before.iter().filter(|(k, _)| **k != key).map(|(k, v)| (k.clone(), Field { value: v.clone(), changed: 1 })).collect();
        merged.insert(removed.clone(), Field { value: serde_json::json!({ "name": "Song (live).mid", "at": 1 }), changed: 1 });
        let outcome = Outcome { revisions: vec![2], merged, local, changes: Vec::new() };
        worker.handle(Request::Synced(Blob::Library, outcome, songs(&store, 0)));
        let library = outbox.try_iter().find_map(|reply| match reply {
            Reply::Taken { library, .. } => library,
            _ => None,
        });
        let library = library.expect("a library to take");

        // Songs noted before the phone took it: nothing comes back.
        worker.handle(Request::Songs(songs(&store, 0)));
        let fields = &worker.library.as_ref().unwrap().fields;
        assert!(!fields.contains_key(&key), "the removed song stays out of the library");
        assert!(fields.contains_key(&removed), "and stays removed");

        // Taken in, the phone deletes it; noted then, it stays removed.
        song_sync::apply_library(&mut store, library);
        assert!(!store.has(&key));
        worker.handle(Request::Songs(songs(&store, 1)));
        let fields = &worker.library.as_ref().unwrap().fields;
        assert!(!fields.contains_key(&key) && fields.contains_key(&removed));
        let _ = std::fs::remove_dir_all(&root);
    }
}
