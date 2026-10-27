//! The song library's and its transfers' sync states, kept on a thread of
//! their own. Noting the songs here in them, planning what to move and
//! taking in each sync go over every song in the library, which takes
//! longer than a frame once there are thousands. `DeviceSync` sends what
//! changed and takes back what it has to act on (`DeviceSync::poll`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender};

use serde_json::Value;

use super::{sync, Blob, Done, Job, Outcome};
use crate::library::identity;
use crate::settings_sync::{self, State};
use crate::song_store::SongStore;
use crate::song_sync::{self, Moved, SyncedDevice, SyncedLibrary, Task};

/// The songs as the device has them, for the worker to go by.
pub(super) struct Songs {
    pub store: SongStore,
    /// The songs this device wants (`song_sync::wanted`).
    pub wanted: BTreeSet<String>,
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
    pub library: bool,
    pub transfers: bool,
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
    /// The devices in the transfers, as of now.
    Devices(Vec<SyncedDevice>),
    /// The songs in the library no device has (`song_sync::unheld`).
    Unheld(Vec<String>),
    /// The blob's sync didn't start: it has no state (turned off meanwhile).
    Skipped(Blob),
    /// A sync was taken in: the server's revision now, and for the library,
    /// its leaves to apply to the songs, read (only when they say more than
    /// the songs here already do).
    Taken { blob: Blob, revision: i64, library: Option<SyncedLibrary> },
    Error(String),
}

/// Starts the worker: syncs it starts report to `events`.
pub(super) fn spawn(events: Sender<Done>) -> (Sender<Request>, Receiver<Reply>) {
    let (requests, inbox) = mpsc::channel();
    let (replies, outbox) = mpsc::channel();
    std::thread::Builder::new()
        .name("song sync".into())
        .spawn(move || {
            let mut worker = Worker { library: None, transfers: None, events, replies };
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
    events: Sender<Done>,
    replies: Sender<Reply>,
}

impl Worker {
    fn handle(&mut self, request: Request) {
        match request {
            Request::Reload => {
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
                let devices = state.record_leaves(leaves, settings_sync::now()).then(|| song_sync::devices(&state.fields));
                if let Some(devices) = devices {
                    self.save(Blob::Transfers);
                    self.reply(Reply::Changed(Blob::Transfers));
                    self.reply(Reply::Devices(devices));
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

    fn reply(&self, reply: Reply) {
        let _ = self.replies.send(reply);
    }

    fn state(&mut self, blob: Blob) -> Option<&mut State> {
        match blob {
            Blob::Library => self.library.as_mut(),
            Blob::Transfers => self.transfers.as_mut(),
            Blob::Settings | Blob::Playlists | Blob::Wishlist => None,
        }
    }

    fn save(&mut self, blob: Blob) {
        if let Some(state) = self.state(blob)
            && let Err(e) = state.save(blob.name())
        {
            log::warn!("{} sync: {e}", blob.name());
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
    fn record_library(&mut self, songs: &Songs) {
        if !songs.library {
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
        let mut plan = song_sync::plan(&applied, &songs.device, &songs.store, &songs.wanted, &songs.busy, &songs.given);
        song_sync::note_device(&mut plan.leaves, &songs.device, &songs.device_name, now);
        song_sync::note_have(&mut plan.leaves, &songs.device, &songs.store);
        let changed = state.record_leaves(plan.leaves, now);
        let devices = song_sync::devices(&state.fields);
        let unheld = song_sync::unheld(&state.fields, &songs.device, &songs.store);
        if changed {
            self.save(Blob::Transfers);
            self.reply(Reply::Changed(Blob::Transfers));
        }
        self.reply(Reply::Devices(devices));
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
        state.revision = outcome.revision;
        state.base = outcome.merged;
        state.fields = result;
        self.save(blob);
        if changed {
            self.reply(Reply::Changed(blob));
        }
        self.reply(Reply::Taken { blob, revision: outcome.revision, library });
    }
}
