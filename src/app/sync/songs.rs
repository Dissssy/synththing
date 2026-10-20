//! The song library's and its transfers' sync states, kept on a thread of
//! their own. Noting the songs here in them, planning what to move and
//! taking in each sync go over every song in the library, which takes
//! longer than a frame once there are thousands. The app sends what
//! changed and takes back what it has to act on (`App::poll_sync`).

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, Sender};

use super::{sync, Blob, Done, Job, Outcome};
use crate::library::identity;
use crate::settings_sync::{self, Fields, State};
use crate::song_store::SongStore;
use crate::song_sync::{self, Moved, Task};

/// The songs as the app has them, for the worker to go by.
pub(super) struct Songs {
    pub store: SongStore,
    /// The songs this device wants (`song_sync::wanted`).
    pub wanted: BTreeSet<String>,
    /// This device's ID (`Device::id`).
    pub device: String,
    /// Songs being moved now: left alone.
    pub busy: BTreeSet<String>,
    /// A move failed a moment ago: plan, but move nothing yet.
    pub paused: bool,
    /// The relay's limit on a song, in MB, once known.
    pub relay_song_mb: Option<u64>,
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
    /// What to move now (nothing while moves are going or paused), and the
    /// names of the songs too big for the relay.
    Planned { tasks: Vec<Task>, too_big: Vec<String> },
    /// The blob's sync didn't start: it has no state (turned off meanwhile).
    Skipped(Blob),
    /// A sync was taken in: the server's revision now, and for the library,
    /// its leaves to apply to the songs.
    Taken { blob: Blob, revision: i64, library: Option<Fields> },
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
            Blob::Settings | Blob::Playlists => None,
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
        let nothing = Reply::Planned { tasks: Vec::new(), too_big: Vec::new() };
        if !songs.transfers {
            return nothing;
        }
        let Some(state) = &mut self.transfers else { return nothing };
        let plan = song_sync::plan(&state.applied, &songs.device, &songs.store, &songs.wanted, &songs.busy);
        if state.record_leaves(plan.leaves, settings_sync::now()) {
            self.save(Blob::Transfers);
            self.reply(Reply::Changed(Blob::Transfers));
        }
        if plan.tasks.is_empty() || !songs.busy.is_empty() || songs.paused {
            return nothing;
        }
        // Songs the server won't take stay here, not holding up the rest.
        // (A limit of 0 is a server that can't move songs at all: the app
        // says so.)
        let (tasks, too_big): (Vec<Task>, Vec<Task>) = plan.tasks.into_iter().partition(|task| match (task, songs.relay_song_mb) {
            (Task::Send(key), Some(mb)) if mb > 0 => song_sync::fits(&songs.store.dir().join(key), mb),
            _ => true,
        });
        let name = |key: &str| songs.store.known().find(|(k, _)| *k == key).map_or_else(|| key.to_string(), |(_, s)| s.name.clone());
        Reply::Planned { tasks, too_big: too_big.iter().map(|t| name(t.key())).collect() }
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
        state.applied = result.iter().map(|(k, f)| (k.clone(), f.value.clone())).collect();
        state.revision = outcome.revision;
        state.base = outcome.merged;
        let library = (blob == Blob::Library).then(|| result.clone());
        state.fields = result;
        self.save(blob);
        if changed {
            self.reply(Reply::Changed(blob));
        }
        self.reply(Reply::Taken { blob, revision: outcome.revision, library });
    }
}
