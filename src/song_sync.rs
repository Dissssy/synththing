//! Songs between devices (`settings_sync.rs` carries the lists, as blobs
//! of their own; `library/sync.rs` the files):
//!
//! - `library`: every song in any device's library, by key, with its name,
//!   tags, when it was first imported and what its file says of itself
//!   (`song_store::Song`: songs are only ever added: a song removed on one
//!   device stays in the others'), the tags themselves (`tag/<id>`), and
//!   the songs that play together (`group/<id>`: their keys, in order),
//!   the pictures picked for artists (`artist/<name>`: its key), and
//!   the songs removed from every device (`removed/<key>`: gone from
//!   the library, until a device imports one again).
//!   Each device knows them all (`SongStore::learn`), and forgets the
//!   removed ones (`SongStore::set_removed`) and those gone from it (no
//!   device had them, and one cleared them: `clear`).
//! - `transfers`: which songs each device wants (`want/<device>/<key>`)
//!   and which are waiting on the relay (`ready/<key>`), and the devices
//!   themselves (`device/<device>`: its name, and when it last said it's
//!   there, once a day), and the songs each has (`have/<device>`, by
//!   their keys' first characters: `note_have`), so a device can tell
//!   which songs none has (`unheld`). A device that says nothing for `FORGET_AFTER` is
//!   forgotten, wants and all, as is one removed by hand (`forget`); it
//!   comes back if it syncs again. A device wants a
//!   song it lacks and hasn't blocked that's in a playlist (or starred,
//!   taking starred songs), or, taking every song, in the library. A device that has a song another wants
//!   sends it to the relay and marks it ready; the one that wanted it
//!   fetches it and stops wanting it; once no one does, it's deleted from
//!   the relay.
//!
//! A device online listens for songs (`listen`), and a device sending one
//! to it streams it straight there instead, through the server but kept
//! nowhere (`run_all`): it's fetched as it's sent. Only for a device not
//! listening does a song wait on the relay, and only while the relay has
//! room for it, so a sender never gets further ahead than that.
//!
//! With a hub (`hub`: the device that keeps every song, its key and where
//! it's reached: `hub.rs`), a device that reaches it directly
//! (`hub_client.rs`) sends it the songs it wants straight there, the
//! rarest first, and takes the songs it wants from it (`Task::Pull`),
//! saying so in its device leaf (`DeviceLeaf::pulls`), so no one sends
//! those to it through the relay. The relay stays for the rest.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::library::client;
use crate::library::identity::Identity;
use crate::playlist::Library;
use crate::settings_sync::Fields;
use crate::song_store::{self, Removed, Song, SongStore, Tag};

/// The synced data's names on the server.
pub const LIBRARY: &str = "library";
pub const TRANSFERS: &str = "transfers";

const TAG: &str = "tag/";
const GROUP: &str = "group/";
const ARTIST: &str = "artist/";
const REMOVED: &str = "removed/";
const HAVE: &str = "have/";
/// How much of each key a device's list of the songs it has keeps: enough
/// to tell a library's songs apart, at a fifth of the size.
const HAVE_PREFIX: usize = 12;

/// The library's leaves: those synced so far (`applied`), with what this
/// device has of them, every song here, the tags, and the songs that play
/// together, and the songs removed from every device. (A song only known
/// here, not here nor synced yet, waits for the synced library: its tags
/// are there.)
pub fn library_leaves(store: &SongStore, applied: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut leaves: BTreeMap<String, Value> = applied
        .iter()
        .filter(|(k, _)| !k.starts_with(TAG) && !k.starts_with(GROUP) && !k.starts_with(ARTIST) && !k.starts_with(REMOVED) && !store.is_removed(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (key, song) in store.known() {
        if applied.contains_key(key) || store.has(key) {
            leaves.insert(key.to_string(), serde_json::to_value(song).unwrap_or_default());
        }
    }
    for (id, tag) in store.tags() {
        leaves.insert(format!("{TAG}{id}"), serde_json::to_value(tag).unwrap_or_default());
    }
    for (id, keys) in store.together() {
        leaves.insert(format!("{GROUP}{id}"), serde_json::to_value(keys).unwrap_or_default());
    }
    for (name, image) in store.artist_images() {
        leaves.insert(format!("{ARTIST}{name}"), Value::String(image.clone()));
    }
    for (key, removed) in store.removed() {
        leaves.insert(format!("{REMOVED}{key}"), serde_json::to_value(removed).unwrap_or_default());
    }
    leaves
}

/// The synced library, read from its leaves (on the sync worker: reading
/// every song takes longer than a frame once there are thousands).
#[derive(Debug, Default)]
pub struct SyncedLibrary {
    removed: BTreeMap<String, Removed>,
    tags: BTreeMap<String, Tag>,
    together: BTreeMap<String, Vec<String>>,
    artist_images: BTreeMap<String, String>,
    songs: Vec<(String, Song)>,
}

/// The library's leaves without the songs `keys` (no device has them).
pub fn clear(leaves: &BTreeMap<String, Value>, keys: &[String]) -> BTreeMap<String, Value> {
    leaves.iter().filter(|(path, _)| !keys.contains(path)).map(|(p, v)| (p.clone(), v.clone())).collect()
}

/// Read the synced library from its leaves.
pub fn read_library(fields: &Fields) -> SyncedLibrary {
    let mut library = SyncedLibrary::default();
    for (key, field) in fields {
        let value = || field.value.clone();
        if let Some(key) = key.strip_prefix(REMOVED) {
            if let Ok(removed) = serde_json::from_value(value()) {
                library.removed.insert(key.to_string(), removed);
            }
        } else if let Some(id) = key.strip_prefix(TAG) {
            if let Ok(tag) = serde_json::from_value(value()) {
                library.tags.insert(id.to_string(), tag);
            }
        } else if let Some(id) = key.strip_prefix(GROUP) {
            if let Ok(keys) = serde_json::from_value(value()) {
                library.together.insert(id.to_string(), keys);
            }
        } else if let Some(name) = key.strip_prefix(ARTIST) {
            if let Some(image) = field.value.as_str().filter(|k| song_store::is_key(k)) {
                library.artist_images.insert(name.to_string(), image.to_string());
            }
        } else if let Ok(song) = serde_json::from_value(value()) {
            library.songs.push((key.clone(), song));
        }
    }
    library
}

/// Take the synced library: every song's name, tags and what's known of
/// it (those gone from it and not here forgotten), the tags and the songs
/// that play together (those gone from it deleted here), and the songs
/// removed from every device. Whether any song was removed or forgotten.
pub fn apply_library(store: &mut SongStore, library: SyncedLibrary) -> bool {
    let SyncedLibrary { removed, tags, together, artist_images, songs } = library;
    let mut forgot = store.set_removed(removed);
    let listed: BTreeSet<String> = songs.iter().map(|(key, _)| key.clone()).collect();
    for (id, tag) in &tags {
        store.set_tag(id, tag.clone());
    }
    for (id, keys) in &together {
        store.set_together(id, keys.clone());
    }
    let gone: Vec<String> = store.artist_images().keys().filter(|name| !artist_images.contains_key(*name)).cloned().collect();
    for name in gone {
        store.set_artist_image(&name, None);
    }
    for (name, image) in artist_images {
        store.set_artist_image(&name, Some(image));
    }
    store.learn(songs);
    let gone: Vec<String> = store.known().map(|(key, _)| key).filter(|key| !listed.contains(*key)).map(str::to_string).collect();
    forgot |= store.forget_absent(gone.iter().map(String::as_str));
    let gone: Vec<String> = store.tags().keys().filter(|id| !tags.contains_key(*id)).cloned().collect();
    for id in gone {
        store.remove_tag(&id);
    }
    let gone: Vec<String> = store.together().keys().filter(|id| !together.contains_key(*id)).cloned().collect();
    for id in gone {
        store.set_together(&id, Vec::new());
    }
    forgot
}

/// Which songs a device takes from the others: those in its playlists,
/// and the starred ones too, or every one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wants {
    pub starred: bool,
    pub every: bool,
}

/// The songs this device wants: those it lacks and hasn't blocked (nor
/// were removed from every device), in a
/// playlist, starred, or anywhere in the library, as `wants` says.
pub fn wanted(store: &SongStore, playlists: &Library, wants: Wants) -> BTreeSet<String> {
    let lacks = |key: &str| song_store::is_key(key) && !store.has(key) && !store.is_blocked(key) && !store.is_removed(key);
    let known = store.known().filter(|(_, song)| wants.every || (wants.starred && song.tags.iter().any(|t| t == song_store::STARRED)));
    let listed = playlists.lists.iter().flat_map(|stored| &stored.playlist.entries).filter_map(|e| song_store::key_in(store.dir(), &e.path));
    known.map(|(key, _)| key).chain(listed).filter(|key| lacks(key)).map(str::to_string).collect()
}

/// What to do about the transfers.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// The transfers' leaves, with this device's wants and finished ones
    /// gone.
    pub leaves: BTreeMap<String, Value>,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Task {
    /// Send this song (by key) to the devices that want it (`to`):
    /// straight to the hub or one listening, or to the relay.
    Send { key: String, to: Vec<String> },
    /// Fetch it from the relay: this device wants it.
    Fetch(String),
    /// Fetch it from the hub, directly: this device wants it, and the hub
    /// has it.
    Pull(String),
    /// Delete it from the relay: no device wants it now.
    Delete(String),
}

impl Task {
    pub fn key(&self) -> &str {
        match self {
            Task::Send { key, .. } | Task::Fetch(key) | Task::Pull(key) | Task::Delete(key) => key,
        }
    }
}

/// The hub, as the transfers' `hub` leaf names it: the device that keeps
/// every song, its own key (what its `hello` answers are signed with) and
/// where it's reached, nearest first (its address on its network, then
/// those it was given: `hub.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HubLeaf {
    pub device: String,
    pub key: String,
    pub addrs: Vec<String>,
    /// Whether its addresses work from the internet, as the server last
    /// found (`hub.rs`: those on a network of their own aren't checked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<AddressCheck>,
}

impl HubLeaf {
    /// The same hub, at the same addresses (whatever its checks say).
    pub fn same_place(&self, other: &HubLeaf) -> bool {
        (&self.device, &self.key, &self.addrs) == (&other.device, &other.key, &other.addrs)
    }
}

/// Whether one of the hub's addresses works from the internet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddressCheck {
    pub url: String,
    /// When (Unix seconds).
    pub at: i64,
    pub works: bool,
    /// What to fix, where it doesn't (`library::reach::hint`), or why it
    /// couldn't be checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

const HUB: &str = "hub";

/// The hub the transfers name, if one does.
pub fn hub(fields: &Fields) -> Option<HubLeaf> {
    serde_json::from_value(fields.get(HUB)?.value.clone()).ok()
}

/// What planning needs to know of the hub.
#[derive(Clone, Debug)]
pub struct HubPlan {
    pub device: String,
    /// This device reaches it directly now.
    pub direct: bool,
    /// This device moves songs only to and from it (Only through my hub),
    /// directly or through the relay.
    pub only: bool,
}

/// The path of `device`'s want of the song `key`.
pub fn want(device: &str, key: &str) -> String {
    format!("want/{device}/{key}")
}

fn ready(key: &str) -> String {
    format!("ready/{key}")
}

/// What `device`, wanting `wanted` and having the files `here` says,
/// does about the transfers synced so far (`applied`), leaving alone the songs `busy` already being moved, and
/// the wants it already streamed a song for (`given`, as their paths: the
/// device it went to says it has it next), with the `hub` if there is one.
pub fn plan(
    applied: &BTreeMap<String, Value>,
    device: &str,
    here: impl Fn(&str) -> bool,
    wanted: &BTreeSet<String>,
    busy: &BTreeSet<String>,
    given: &BTreeSet<String>,
    hub: Option<&HubPlan>,
) -> Plan {
    let mine = format!("want/{device}/");
    let mut leaves: BTreeMap<String, Value> = applied.iter().filter(|(path, _)| !path.starts_with(&mine)).map(|(p, v)| (p.clone(), v.clone())).collect();
    leaves.extend(wanted.iter().map(|key| (want(device, key), Value::Bool(true))));
    // Who wants each song.
    let mut wanters: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for path in leaves.keys().filter(|path| !given.contains(*path)) {
        if let Some((who, key)) = path.strip_prefix("want/").and_then(|rest| rest.split_once('/')) {
            wanters.entry(key).or_default().insert(who);
        }
    }
    let waiting: BTreeSet<&str> = leaves.keys().filter_map(|path| path.strip_prefix("ready/")).collect();
    // The songs each device has (by their keys' first characters), and
    // the devices that take songs the hub has from it.
    let has: BTreeMap<&str, BTreeSet<String>> = leaves
        .iter()
        .filter_map(|(path, value)| Some((path.strip_prefix(HAVE)?, serde_json::from_value::<BTreeSet<String>>(value.clone()).ok()?)))
        .collect();
    let holds = |who: &str, key: &str| key.get(..HAVE_PREFIX).is_some_and(|p| has.get(who).is_some_and(|have| have.contains(p)));
    let hub_has = |key: &str| hub.is_some_and(|hub| holds(&hub.device, key));
    let devices: BTreeMap<&str, DeviceLeaf> = leaves
        .iter()
        .filter_map(|(path, value)| Some((path.strip_prefix("device/")?, serde_json::from_value::<DeviceLeaf>(value.clone()).ok()?)))
        .collect();
    let pulls = hub.is_some_and(|hub| hub.direct && hub.device != device);
    // (Songs go to a device that moves them only through the hub from the
    // hub alone, and from it to the hub alone.)
    let hub_device = hub.map(|hub| hub.device.as_str());
    let only_hub = |who: &str| hub_device.is_some_and(|hub| hub != who) && devices.get(who).is_some_and(|leaf| leaf.hub_only);
    let only = hub.is_some_and(|hub| hub.only && hub.device != device);
    let mut tasks = Vec::new();
    for &key in &waiting {
        if !wanters.contains_key(key) && !busy.contains(key) {
            tasks.push(Task::Delete(key.to_string()));
        }
    }
    let mut sends = Vec::new();
    for (&key, who) in &wanters {
        if busy.contains(key) {
            continue;
        }
        if waiting.contains(key) {
            if who.contains(device) {
                tasks.push(Task::Fetch(key.to_string()));
            }
        } else if who.contains(device) && pulls && hub_has(key) {
            tasks.push(Task::Pull(key.to_string()));
        } else if here(key) {
            // (A device taking songs from the hub takes this one there.)
            let pulled = |w: &str| devices.get(w).is_some_and(|leaf| leaf.pulls) && hub_has(key);
            let allowed = |w: &str| if only || only_hub(w) { hub_device == Some(device) || hub_device == Some(w) } else { true };
            let to: Vec<String> = who.iter().filter(|&&w| w != device && !pulled(w) && allowed(w)).map(|w| w.to_string()).collect();
            if !to.is_empty() {
                sends.push(Task::Send { key: key.to_string(), to });
            }
        }
    }
    // The songs fewest devices have go first: the hub gets those no one
    // else could send it before those several could.
    sends.sort_by_cached_key(|task| has.keys().filter(|who| holds(who, task.key())).count());
    tasks.extend(sends);
    for task in &tasks {
        if let Task::Delete(key) = task {
            leaves.remove(&ready(key));
        }
    }
    Plan { leaves, tasks }
}

/// The size of the song at `path`, sealed.
fn sealed_len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| crate::library::sync::sealed_len(m.len()))
}

/// Whether the song at `path`, sealed, is within a relay's `relay_song_mb`.
pub fn fits(path: &Path, relay_song_mb: u64) -> bool {
    sealed_len(path).is_none_or(|len| len <= relay_song_mb.saturating_mul(1024 * 1024))
}

/// The relay, as a device signed in to it reaches it.
#[derive(Clone)]
pub struct Relay {
    pub server: String,
    pub server_key: String,
    pub identity: Identity,
    pub sync_key: [u8; 32],
    /// The song library's folder.
    pub dir: PathBuf,
    /// This device (its ID).
    pub device: String,
    /// The hub, if this device reaches it directly.
    pub hub: Option<Hub>,
}

/// The hub, reached: where it answered.
#[derive(Clone, Debug, PartialEq)]
pub struct Hub {
    pub device: String,
    pub base: String,
    pub key: String,
}

/// Where `hub` answers, trying its addresses in order (those on this
/// network given less time), if anywhere.
pub fn reach(hub: &HubLeaf) -> Option<Hub> {
    hub.addrs.iter().find_map(|base| match client::hub_hello(base, &hub.key, crate::library::reach::local(base)) {
        Ok(()) => Some(Hub { device: hub.device.clone(), base: base.clone(), key: hub.key.clone() }),
        Err(e) => {
            log::info!("the hub at {base}: {e}");
            None
        }
    })
}

/// What came of a task.
#[derive(Clone, Debug, PartialEq)]
pub enum Moved {
    /// Sent to the relay: mark it ready.
    Sent(String),
    /// Streamed straight to the device (`to`) listening for it.
    Streamed { key: String, to: String },
    /// Fetched into the library.
    Fetched(String),
    /// It wasn't on the relay after all (it waited too long), or didn't
    /// open: unmark it, to be sent again.
    Gone(String),
    Deleted,
    Failed(String, String),
    /// The relay has no room for more of this identity's songs until the
    /// others take some: this many weren't sent.
    Full(usize),
    /// Too big for the relay to keep, with no device listening for it.
    TooBig(String),
    /// The hub has it already, or another device is sending it there: as
    /// if it went (`to`, the hub), but not by this device.
    Elsewhere { key: String, to: String },
    /// The hub couldn't be reached directly after all (why): the relay
    /// goes on in its place.
    HubLost(String),
}

/// The relay's room, as a device sending songs knows it.
pub struct Room {
    /// The biggest song it keeps (MB: `Info::relay_song_mb`).
    pub song_mb: u64,
    /// It was full a moment ago.
    pub full: bool,
}

fn failed(key: &str) -> impl FnOnce(anyhow::Error) -> Moved + '_ {
    move |e| Moved::Failed(key.to_string(), format!("{e:#}"))
}

/// Do `task` (off the main thread: songs are big), through the relay.
pub fn run(relay: &Relay, task: &Task) -> Moved {
    let key = task.key().to_string();
    let id = crate::library::sync::relay_id(&relay.sync_key, &key);
    let (server, server_key, identity) = (&relay.server, &relay.server_key, &relay.identity);
    let done = match task {
        Task::Send { .. } => client::relay_put(server, server_key, identity, &relay.sync_key, &id, &song_store::path_of(&relay.dir, &key)).map(|()| Moved::Sent(key.clone())),
        Task::Fetch(_) => {
            let mut incoming = match song_store::Incoming::new(&key, &relay.dir) {
                Ok(incoming) => incoming,
                Err(e) => return failed(&key)(e),
            };
            match client::relay_get(server, server_key, identity, &relay.sync_key, &id, &mut incoming) {
                Ok(true) => return incoming.finish().map_or_else(failed(&key), |()| Moved::Fetched(key.clone())),
                Ok(false) => Ok(Moved::Gone(key.clone())),
                Err(e) => Err(e),
            }
        }
        Task::Delete(_) => client::relay_delete(server, server_key, identity, &id).map(|()| Moved::Deleted),
        Task::Pull(_) => {
            let Some(hub) = &relay.hub else { return Moved::HubLost("not reached".into()) };
            let mut incoming = match song_store::Incoming::new(&key, &relay.dir) {
                Ok(incoming) => incoming,
                Err(e) => return failed(&key)(e),
            };
            match client::hub_get(&hub.base, &hub.key, identity, &relay.device, &relay.sync_key, &key, &mut incoming) {
                Ok(true) => return incoming.finish().map_or_else(failed(&key), |()| Moved::Fetched(key.clone())),
                Ok(false) => Err("the hub doesn't have it after all".to_string()),
                Err(e) => return Moved::HubLost(e),
            }
        }
    };
    done.unwrap_or_else(|e| Moved::Failed(key, e))
}

/// Do `tasks` in order, telling `done` what came of each (it says whether
/// to go on). A song goes straight to a device listening for it if one is;
/// otherwise to the relay, only while it has room: the rest wait
/// (`Moved::Full`), rather than pushing out songs not fetched yet.
pub fn run_all(relay: &Relay, tasks: &[Task], room: Room, mut done: impl FnMut(Moved) -> bool) {
    let (server, server_key, identity) = (&relay.server, &relay.server_key, &relay.identity);
    // (Once the hub can't be reached, the rest go by the relay.)
    let mut hub = relay.hub.clone();
    // Which of the devices songs are for are listening, by inbox.
    let mut inboxes: HashMap<String, String> = HashMap::new();
    for task in tasks {
        if let Task::Send { to, .. } = task {
            inboxes.extend(to.iter().map(|device| (device.clone(), crate::library::sync::inbox_id(&relay.sync_key, device))));
        }
    }
    let mut listening: BTreeSet<String> = BTreeSet::new();
    if !inboxes.is_empty() {
        let asked: Vec<String> = inboxes.values().cloned().collect();
        match client::relay_listening(server, server_key, identity, &asked) {
            Ok(found) => listening = inboxes.iter().filter(|(_, inbox)| found.contains(inbox)).map(|(device, _)| device.clone()).collect(),
            Err(e) => log::info!("which devices are listening for songs: {e}"),
        }
    }
    let streams_to = |to: &[String], listening: &BTreeSet<String>| to.iter().find(|d| listening.contains(*d)).cloned();
    // What's left on the relay (asked before the first song left there,
    // after this device's deletes), and whether the rest wait.
    let mut left = room.full.then_some(0);
    let mut held_back = false;
    for (i, task) in tasks.iter().enumerate() {
        let moved = match task {
            Task::Send { key, to } => {
                let file = song_store::path_of(&relay.dir, key);
                let mut streamed = None;
                if let Some(reached) = hub.clone().filter(|hub| to.contains(&hub.device)) {
                    streamed = match client::hub_put(&reached.base, &reached.key, identity, &relay.device, &relay.sync_key, key, &file) {
                        Ok(client::HubPut::Taken) => Some(Moved::Streamed { key: key.clone(), to: reached.device }),
                        Ok(client::HubPut::Has | client::HubPut::Busy) => Some(Moved::Elsewhere { key: key.clone(), to: reached.device }),
                        // (Through the relay, then: the hub listens there.)
                        Err(e) => {
                            hub = None;
                            if !done(Moved::HubLost(e)) {
                                return;
                            }
                            None
                        }
                    };
                }
                while streamed.is_none()
                    && let Some(device) = streams_to(to, &listening) {
                    let id = crate::library::sync::relay_id(&relay.sync_key, key);
                    streamed = match client::relay_send(server, server_key, identity, &relay.sync_key, &id, &inboxes[&device], &file) {
                        Ok(true) => Some(Moved::Streamed { key: key.clone(), to: device }),
                        Ok(false) => {
                            listening.remove(&device);
                            continue;
                        }
                        Err(e) => Some(Moved::Failed(key.clone(), e)),
                    };
                    break;
                }
                if let Some(moved) = streamed {
                    moved
                } else if held_back {
                    continue;
                } else if !fits(&file, room.song_mb) {
                    Moved::TooBig(key.clone())
                } else {
                    let room_left = match left {
                        Some(room_left) => room_left,
                        None => match client::relay_room(server, server_key, identity) {
                            Ok(room_left) => *left.insert(room_left),
                            Err(e) => {
                                held_back = true;
                                if !done(Moved::Failed(key.clone(), e)) {
                                    return;
                                }
                                continue;
                            }
                        },
                    };
                    let len = sealed_len(&file).unwrap_or_default();
                    if len > room_left {
                        held_back = true;
                        let unsent = tasks[i..].iter().filter(|t| matches!(t, Task::Send { to, .. } if streams_to(to, &listening).is_none())).count();
                        Moved::Full(unsent)
                    } else {
                        let moved = run(relay, task);
                        if matches!(moved, Moved::Sent(_)) {
                            left = Some(room_left - len);
                        }
                        moved
                    }
                }
            }
            _ => run(relay, task),
        };
        if !done(moved) {
            return;
        }
    }
}

/// Listen for a while (as `device`) for a song streamed to this device,
/// taking it if `wanted` gives its key for its relay ID (`wanted_ids`),
/// asked as the song comes: what came of it, if one came, or why it
/// couldn't listen.
pub fn listen(relay: &Relay, device: &str, wanted: impl Fn(&str) -> Option<String>) -> Result<Option<Moved>, String> {
    let inbox = crate::library::sync::inbox_id(&relay.sync_key, device);
    let mut key = String::new();
    let take = |id: &str| {
        key = wanted(id)?;
        song_store::Incoming::new(&key, &relay.dir).ok()
    };
    let Some((_, incoming)) = client::relay_inbox(&relay.server, &relay.server_key, &relay.identity, &relay.sync_key, &inbox, take)? else {
        return Ok(None);
    };
    Ok(Some(incoming.finish().map_or_else(failed(&key), |()| Moved::Fetched(key.clone()))))
}

/// The relay IDs of the songs this device wants: what a song streamed to
/// it comes as.
pub fn wanted_ids(sync_key: &[u8; 32], wanted: &BTreeSet<String>) -> HashMap<String, String> {
    wanted.iter().map(|key| (crate::library::sync::relay_id(sync_key, key), key.clone())).collect()
}

/// The transfers' leaves after `moved`.
pub fn after(mut leaves: BTreeMap<String, Value>, moved: &Moved) -> BTreeMap<String, Value> {
    match moved {
        Moved::Sent(key) => {
            leaves.insert(ready(key), Value::Bool(true));
        }
        Moved::Gone(key) => {
            leaves.remove(&ready(key));
        }
        Moved::Streamed { .. }
        | Moved::Fetched(_)
        | Moved::Deleted
        | Moved::Failed(..)
        | Moved::Full(_)
        | Moved::TooBig(_)
        | Moved::Elsewhere { .. }
        | Moved::HubLost(_) => {}
    }
    leaves
}

/// How long a device can go without syncing before the others forget it.
pub const FORGET_AFTER: i64 = 14 * 24 * 60 * 60;
/// How often a device says it's still there (`note_device`).
const SEEN_EVERY: i64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct DeviceLeaf {
    name: String,
    seen: i64,
    /// It takes the songs it wants that the hub has from the hub,
    /// directly: no one sends those to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pulls: bool,
    /// It moves songs only to and from the hub (Only through my hub): no
    /// one else sends it any.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    hub_only: bool,
}

/// A device in the transfers.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncedDevice {
    pub id: String,
    /// What it calls itself (none for one from before devices said).
    pub name: Option<String>,
    /// When it last said anything (Unix seconds).
    pub seen: i64,
}

/// The device a transfers leaf is about, if any.
fn device_of(path: &str) -> Option<&str> {
    path.strip_prefix("device/")
        .or_else(|| path.strip_prefix(HAVE))
        .or_else(|| path.strip_prefix("want/").and_then(|rest| rest.split_once('/')).map(|(device, _)| device))
}

/// The devices in the transfers (`fields`), the most recently seen first.
pub fn devices(fields: &Fields) -> Vec<SyncedDevice> {
    let mut found: BTreeMap<&str, SyncedDevice> = BTreeMap::new();
    for (path, field) in fields {
        let Some(id) = device_of(path) else { continue };
        let device = found.entry(id).or_insert_with(|| SyncedDevice { id: id.to_string(), name: None, seen: 0 });
        device.seen = device.seen.max(field.changed);
        if path.starts_with("device/")
            && let Ok(leaf) = serde_json::from_value::<DeviceLeaf>(field.value.clone())
        {
            device.name = Some(leaf.name);
            device.seen = device.seen.max(leaf.seen);
        }
    }
    let mut devices: Vec<SyncedDevice> = found.into_values().collect();
    devices.sort_by_key(|d| std::cmp::Reverse(d.seen));
    devices
}

/// The transfers' leaves without `device`'s: its wants, what it has, and
/// it.
pub fn forget(leaves: &BTreeMap<String, Value>, device: &str) -> BTreeMap<String, Value> {
    leaves.iter().filter(|(path, _)| device_of(path) != Some(device)).map(|(p, v)| (p.clone(), v.clone())).collect()
}

/// The devices in `fields` other than `me` that haven't said anything for
/// `FORGET_AFTER` as of `now`.
pub fn gone_quiet(fields: &Fields, me: &str, now: i64) -> Vec<String> {
    devices(fields).into_iter().filter(|d| d.id != me && now - d.seen > FORGET_AFTER).map(|d| d.id).collect()
}

/// Note in the transfers' `leaves` that `device` is there, as `name`,
/// and whether it `pulls` songs from the hub (once a day, or when one of
/// those changes: not to write the transfers over and over).
pub fn note_device(leaves: &mut BTreeMap<String, Value>, device: &str, name: &str, pulls: bool, hub_only: bool, now: i64) {
    let path = format!("device/{device}");
    let noted = leaves.get(&path).and_then(|v| serde_json::from_value::<DeviceLeaf>(v.clone()).ok());
    if noted.is_some_and(|leaf| leaf.name == name && leaf.pulls == pulls && leaf.hub_only == hub_only && now - leaf.seen < SEEN_EVERY) {
        return;
    }
    leaves.insert(path, serde_json::to_value(DeviceLeaf { name: name.to_string(), seen: now, pulls, hub_only }).unwrap_or_default());
}

/// Name `hub` the hub in the transfers' `leaves` (it's the only one: a
/// hub named before goes idle).
pub fn note_hub(leaves: &mut BTreeMap<String, Value>, hub: &HubLeaf) {
    leaves.insert(HUB.to_string(), serde_json::to_value(hub).unwrap_or_default());
}

/// Note in the transfers' `leaves` the songs `device` has (by their keys'
/// first `HAVE_PREFIX` characters).
pub fn note_have(leaves: &mut BTreeMap<String, Value>, device: &str, store: &SongStore) {
    let have: BTreeSet<&str> = store.known().filter(|(key, _)| store.has(key)).filter_map(|(key, _)| key.get(..HAVE_PREFIX)).collect();
    leaves.insert(format!("{HAVE}{device}"), serde_json::to_value(have).unwrap_or_default());
}

/// The songs in the library that no device has, as the transfers
/// (`fields`) say: not here (`me`), on no other device's list
/// (`note_have`), nor waiting on the relay. None while a device hasn't
/// said what it has (an older version): any could be on it.
pub fn unheld(fields: &Fields, me: &str, store: &SongStore) -> Vec<String> {
    let mut held: BTreeSet<String> = BTreeSet::new();
    for device in devices(fields).into_iter().filter(|d| d.id != me) {
        let have = fields.get(&format!("{HAVE}{}", device.id)).and_then(|f| serde_json::from_value::<Vec<String>>(f.value.clone()).ok());
        let Some(have) = have else { return Vec::new() };
        held.extend(have);
    }
    store
        .known()
        .map(|(key, _)| key)
        .filter(|key| !store.has(key) && !fields.contains_key(&ready(key)) && key.get(..HAVE_PREFIX).is_none_or(|p| !held.contains(p)))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_song_goes_from_one_device_to_another() {
        let root = std::env::temp_dir().join(format!("synththing-song-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (desktop_dir, phone_dir) = (root.join("desktop"), root.join("phone"));
        let mut desktop = SongStore::open(desktop_dir.clone());
        let song = song_store::write_in(b"song", "Song.mid", &desktop_dir).unwrap();
        desktop.add(song.clone());
        let mut phone = SongStore::open(phone_dir.clone());

        // The library goes around: the phone knows the song, and wants it.
        desktop.set_tag("t1", Tag { name: "Chill".into(), color: [1, 2, 3] });
        desktop.set_song_tags(&song.key, vec!["t1".into()]);
        let elsewhere = format!("{}.mid", "b".repeat(64));
        desktop.set_together("g1", vec![song.key.clone(), elsewhere.clone()]);
        let picture = format!("{}.jpg", "c".repeat(64));
        desktop.set_artist_image("aloboi", Some(picture.clone()));
        let library = library_leaves(&desktop, &BTreeMap::new());
        assert_eq!(library[&song.key]["name"], "Song.mid");
        desktop.note_meta([(song.key.clone(), song_store::Meta { title: Some("Song".into()), track: Some(3), size: 4, ..Default::default() })]);
        let library = library_leaves(&desktop, &BTreeMap::new());
        assert_eq!(library[&song.key]["meta"], serde_json::json!({ "t": "Song", "n": 3, "s": 4 }), "short names");
        let fields = library.iter().map(|(k, v)| (k.clone(), crate::settings_sync::Field { value: v.clone(), changed: 1 })).collect();
        apply_library(&mut phone, read_library(&fields));
        assert_eq!(phone.tags()["t1"].name, "Chill");
        assert_eq!(phone.together()["g1"], [song.key.clone(), elsewhere], "play together");
        assert_eq!(phone.artist_images()["aloboi"], picture, "an artist's picture");
        let known = phone.known().find(|(k, _)| *k == song.key).unwrap().1.clone();
        assert_eq!(known.tags, ["t1"]);
        assert_eq!(known.meta.unwrap().title.as_deref(), Some("Song"), "known without the file");
        assert_eq!(known.imported, desktop.known().next().unwrap().1.imported, "imported when it first was");
        assert_eq!(library_leaves(&phone, &library), library, "nothing new to say");
        let (playlists, _) = Library::load(root.join("playlists"), phone_dir.clone());
        let (only_listed, starred, every) =
            (Wants { starred: false, every: false }, Wants { starred: true, every: false }, Wants { starred: false, every: true });
        assert!(wanted(&phone, &playlists, only_listed).is_empty(), "in no playlist");
        assert!(wanted(&phone, &playlists, starred).is_empty(), "not starred");
        let mut starred_phone = phone.clone();
        starred_phone.set_song_tags(&song.key, vec!["t1".into(), song_store::STARRED.into()]);
        assert_eq!(wanted(&starred_phone, &playlists, starred), BTreeSet::from([song.key.clone()]));
        assert!(starred_phone.tags_of(&song_store::path_of(&phone_dir, &song.key)).iter().all(|(id, _)| *id != song_store::STARRED), "never a chip");
        let wants = wanted(&phone, &playlists, every);
        assert_eq!(wants, BTreeSet::from([song.key.clone()]));

        let none = BTreeSet::new();
        let asked = plan(&BTreeMap::new(), "phone", |k| phone.has(k), &wants, &none, &none, None);
        assert!(asked.tasks.is_empty(), "no one has it to send yet");
        // The desktop sends it, and marks it ready.
        let sending = plan(&asked.leaves, "desktop", |k| desktop.has(k), &none, &none, &none, None);
        assert_eq!(sending.tasks, [Task::Send { key: song.key.clone(), to: vec!["phone".into()] }]);
        assert!(plan(&asked.leaves, "desktop", |k| desktop.has(k), &none, &BTreeSet::from([song.key.clone()]), &none, None).tasks.is_empty(), "busy");
        let given = BTreeSet::from([want("phone", &song.key)]);
        assert!(plan(&asked.leaves, "desktop", |k| desktop.has(k), &none, &none, &given, None).tasks.is_empty(), "streamed to it already");
        let sent = after(sending.leaves, &Moved::Sent(song.key.clone()));
        // The phone fetches it; once it has it, it stops wanting it, and
        // deletes it from the relay, as no one wants it now.
        assert_eq!(plan(&sent, "phone", |k| phone.has(k), &wants, &none, &none, None).tasks, [Task::Fetch(song.key.clone())]);
        let mut incoming = song_store::Incoming::new(&song.key, &phone_dir).unwrap();
        std::io::Write::write_all(&mut incoming, b"song").unwrap();
        incoming.finish().unwrap();
        phone.add(song_store::Stored { key: song.key.clone(), name: "Song.mid".into() });
        let fetched = plan(&sent, "phone", |k| phone.has(k), &wanted(&phone, &playlists, every), &none, &none, None);
        assert_eq!(fetched.tasks, [Task::Delete(song.key.clone())]);
        assert!(fetched.leaves.is_empty());

        // Something else than the song, fetched as it, isn't kept.
        let mut incoming = song_store::Incoming::new(&song.key, &root.join("elsewhere")).unwrap();
        std::io::Write::write_all(&mut incoming, b"not it").unwrap();
        assert!(incoming.finish().is_err());
        assert_eq!(std::fs::read_dir(root.join("elsewhere")).unwrap().count(), 0, "nothing left behind");
        // Removed here: not wanted again.
        phone.remove(&song_store::path_of(&phone_dir, &song.key)).unwrap();
        assert!(wanted(&phone, &playlists, every).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_song_removed_everywhere_goes_from_every_device() {
        let root = std::env::temp_dir().join(format!("synththing-song-removed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (desktop_dir, phone_dir) = (root.join("desktop"), root.join("phone"));
        let fields = |leaves: &BTreeMap<String, Value>| leaves.iter().map(|(k, v)| (k.clone(), crate::settings_sync::Field { value: v.clone(), changed: 1 })).collect();
        let mut desktop = SongStore::open(desktop_dir.clone());
        let song = song_store::write_in(b"song", "Song.mid", &desktop_dir).unwrap();
        desktop.add(song.clone());
        let mut phone = SongStore::open(phone_dir.clone());
        phone.add(song_store::write_in(b"song", "Song.mid", &phone_dir).unwrap());
        let synced = library_leaves(&desktop, &BTreeMap::new());
        apply_library(&mut phone, read_library(&fields(&synced)));

        // The desktop removes it everywhere: the library says so, and no
        // longer has the song.
        desktop.remove_everywhere(&song.key).unwrap();
        let removed = library_leaves(&desktop, &synced);
        assert!(!removed.contains_key(&song.key));
        assert_eq!(removed[&format!("removed/{}", song.key)]["name"], "Song.mid");
        // The phone forgets it, and doesn't want it back.
        assert!(apply_library(&mut phone, read_library(&fields(&removed))));
        assert!(!phone.has(&song.key) && !song_store::path_of(&phone_dir, &song.key).exists());
        assert_eq!(phone.known().count(), 0);
        assert_eq!(library_leaves(&phone, &removed), removed, "nothing new to say");
        let (playlists, _) = Library::load(root.join("playlists"), phone_dir.clone());
        phone.know(&song.key, "Song.mid");
        assert!(wanted(&phone, &playlists, Wants { starred: false, every: true }).is_empty());
        assert!(!apply_library(&mut phone, read_library(&fields(&removed))), "already gone");
        // A song only removed here can be removed everywhere later.
        let other = song_store::write_in(b"other", "Other.mid", &phone_dir).unwrap();
        phone.add(other.clone());
        phone.remove(&song_store::path_of(&phone_dir, &other.key)).unwrap();
        phone.remove_everywhere(&other.key).unwrap();
        assert!(phone.is_removed(&other.key) && !phone.is_blocked(&other.key));
        assert_eq!(library_leaves(&phone, &removed)[&format!("removed/{}", other.key)]["name"], "Other.mid");

        // Imported again, it's back, everywhere.
        phone.add(song_store::write_in(b"song", "Again.mid", &phone_dir).unwrap());
        let back = library_leaves(&phone, &removed);
        assert!(back.contains_key(&song.key) && !back.contains_key(&format!("removed/{}", song.key)));
        apply_library(&mut desktop, read_library(&fields(&back)));
        assert!(!desktop.is_removed(&song.key));
        assert_eq!(wanted(&desktop, &playlists, Wants { starred: false, every: true }), BTreeSet::from([song.key.clone()]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_big_library_fits_one_blob() {
        // 5,000 songs, each with every field: deflated and sealed, well
        // within what a blob can be.
        let mut leaves = BTreeMap::new();
        for i in 0..5000u32 {
            // (Hashes, as keys are: they don't compress.)
            let key = format!("{}.mp3", crate::library::hex(&<sha2::Sha256 as sha2::Digest>::digest(i.to_le_bytes())));
            let meta = song_store::Meta {
                title: Some(format!("Song number {i}")),
                artist: Some(format!("Artist {}", i % 300)),
                album: Some(format!("Album {}", i % 400)),
                album_artist: Some(format!("Artist {}", i % 300)),
                composer: Some(format!("Composer {}", i % 200)),
                genre: Some(["Jazz", "Rock", "Game", "Classical"][i as usize % 4].into()),
                track: Some(i % 14 + 1),
                disc: Some(1),
                year: Some(1980 + i % 40),
                length: Some(f64::from(120 + i % 300) + 0.25),
                size: 4_000_000 + u64::from(i) * 977,
                compilation: true,
                ..Default::default()
            };
            let song = Song { name: format!("{:02} Song number {i}.mp3", i % 14 + 1), imported: 1_790_000_000 + i64::from(i), tags: vec![song_store::STARRED.into()], meta: Some(meta), ..Song::default() };
            leaves.insert(key, crate::settings_sync::Field { value: serde_json::to_value(song).unwrap(), changed: 1_790_000_000 });
        }
        let plain = serde_json::to_vec(&leaves).unwrap();
        let sealed = crate::library::sync::seal(&[1; 32], &crate::library::sync::pack(&plain)).unwrap();
        assert!(sealed.len() < crate::library::sync::MAX_DATA * 3 / 4, "{} KB sealed, from {} KB", sealed.len() / 1024, plain.len() / 1024);
    }

    #[test]
    fn songs_no_device_has_are_cleared() {
        let root = std::env::temp_dir().join(format!("synththing-song-unheld-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (desktop_dir, phone_dir) = (root.join("desktop"), root.join("phone"));
        let fields = |leaves: &BTreeMap<String, Value>| -> Fields {
            leaves.iter().map(|(k, v)| (k.clone(), crate::settings_sync::Field { value: v.clone(), changed: 1 })).collect()
        };
        let mut desktop = SongStore::open(desktop_dir.clone());
        let (kept, gone) = (song_store::write_in(b"kept", "Kept.mid", &desktop_dir).unwrap(), song_store::write_in(b"gone", "Gone.mid", &desktop_dir).unwrap());
        desktop.add(kept.clone());
        desktop.add(gone.clone());
        let mut phone = SongStore::open(phone_dir.clone());
        let synced = library_leaves(&desktop, &BTreeMap::new());
        apply_library(&mut phone, read_library(&fields(&synced)));

        // Each device says what it has: the phone, nothing.
        let mut transfers = BTreeMap::new();
        note_have(&mut transfers, "desktop", &desktop);
        assert!(unheld(&fields(&transfers), "phone", &phone).is_empty(), "the desktop has both");
        let mut older = fields(&transfers);
        older.insert(want("old", &kept.key), crate::settings_sync::Field { value: Value::Bool(true), changed: 1 });
        // The desktop removes one: only the phone could have it, and doesn't.
        desktop.remove(&song_store::path_of(&desktop_dir, &gone.key)).unwrap();
        note_have(&mut transfers, "desktop", &desktop);
        note_have(&mut transfers, "phone", &phone);
        assert_eq!(unheld(&fields(&transfers), "phone", &phone), [gone.key.clone()]);
        assert_eq!(unheld(&fields(&transfers), "desktop", &desktop), [gone.key.clone()], "removed here too");
        older.extend(fields(&transfers));
        assert!(unheld(&older, "phone", &phone).is_empty(), "a device that hasn't said could have it");
        let mut waiting = fields(&transfers);
        waiting.insert(ready(&gone.key), crate::settings_sync::Field { value: Value::Bool(true), changed: 1 });
        assert!(unheld(&waiting, "phone", &phone).is_empty(), "on the relay");

        // The phone clears it: it forgets it, the library drops it, and so
        // the desktop forgets it too.
        assert!(phone.forget_absent([gone.key.as_str()]));
        let cleared = library_leaves(&phone, &clear(&synced, &[gone.key.clone()]));
        assert!(cleared.contains_key(&kept.key) && !cleared.contains_key(&gone.key));
        assert!(apply_library(&mut desktop, read_library(&fields(&cleared))));
        assert!(desktop.get_by_key(&gone.key).is_none() && !desktop.is_blocked(&gone.key));
        assert!(desktop.has(&kept.key), "what it has stays");
        // A device that turns out to have one lists it again.
        let mut kept_cleared = clear(&cleared, &[kept.key.clone()]);
        assert!(!apply_library(&mut desktop, read_library(&fields(&kept_cleared))), "here: not forgotten");
        kept_cleared = library_leaves(&desktop, &kept_cleared);
        assert!(kept_cleared.contains_key(&kept.key));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn devices_say_they_are_there_and_go_quiet() {
        let field = |value: Value, changed| crate::settings_sync::Field { value, changed };
        let day = 24 * 60 * 60;
        let mut leaves = BTreeMap::new();
        note_device(&mut leaves, "desk", "Desktop", false, false, 10 * day);
        let noted = leaves.clone();
        note_device(&mut leaves, "desk", "Desktop", false, false, 10 * day + 60);
        assert_eq!(leaves, noted, "once a day");
        note_device(&mut leaves, "desk", "Desktop", false, false, 11 * day);
        assert_ne!(leaves, noted);
        let mut fields: Fields = leaves.iter().map(|(k, v)| (k.clone(), field(v.clone(), 11 * day))).collect();
        // One from before devices said: known by its wants alone.
        fields.insert(want("old", "a.mid"), field(Value::Bool(true), 2 * day));
        fields.insert(want("old", "b.mid"), field(Value::Bool(true), 3 * day));
        fields.insert(ready("a.mid"), field(Value::Bool(true), 3 * day));
        let found = devices(&fields);
        assert_eq!(found[0], SyncedDevice { id: "desk".into(), name: Some("Desktop".into()), seen: 11 * day });
        assert_eq!(found[1], SyncedDevice { id: "old".into(), name: None, seen: 3 * day });
        assert!(gone_quiet(&fields, "desk", 16 * day).is_empty());
        assert_eq!(gone_quiet(&fields, "desk", 18 * day), ["old"]);
        assert!(gone_quiet(&fields, "old", 18 * day).is_empty(), "never itself");
        let applied: BTreeMap<String, Value> = fields.iter().map(|(k, f)| (k.clone(), f.value.clone())).collect();
        let left = forget(&applied, "old");
        assert_eq!(left.keys().collect::<Vec<_>>(), ["device/desk", "ready/a.mid"]);
    }

    #[test]
    fn through_the_relay() {
        let root = std::env::temp_dir().join(format!("synththing-relay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("server.json"), r#"{ "previews": false, "check_uploads": false, "sync": true, "relay_song_mb": 1, "relay_mb_per_identity": 1 }"#).unwrap();
        let running = crate::library::server::start(&root, Some("127.0.0.1:0".into())).unwrap();
        let server = format!("http://{}", running.addr);
        let server_key = client::info(&server).unwrap().key;
        let identity = Identity::generate().unwrap();
        let relay = |dir: &str| Relay { server: server.clone(), server_key: server_key.clone(), identity: identity.clone(), sync_key: [7; 32], dir: root.join(dir), device: dir.to_string(), hub: None };
        let song = song_store::write_in(b"a song", "Song.ogg", &root.join("desktop")).unwrap();
        let send = |key: &str| Task::Send { key: key.to_string(), to: vec!["phone".into()] };
        let run_all = |dir: &str, tasks: &[Task]| {
            let mut moved = Vec::new();
            super::run_all(&relay(dir), tasks, Room { song_mb: 1, full: false }, |m| {
                moved.push(m);
                true
            });
            moved
        };

        assert_eq!(run(&relay("desktop"), &send(&song.key)), Moved::Sent(song.key.clone()));
        assert_eq!(run(&relay("phone"), &Task::Fetch(song.key.clone())), Moved::Fetched(song.key.clone()));
        assert_eq!(std::fs::read(song_store::path_of(&root.join("phone"), &song.key)).unwrap(), b"a song");
        // Someone else's ID for it, or a key that can't open it: not theirs.
        let stranger = Relay { identity: Identity::generate().unwrap(), ..relay("stranger") };
        assert_eq!(run(&stranger, &Task::Fetch(song.key.clone())), Moved::Gone(song.key.clone()));
        let other_key = Relay { sync_key: [8; 32], ..relay("stranger") };
        assert_eq!(run(&other_key, &Task::Fetch(song.key.clone())), Moved::Gone(song.key.clone()), "another ID");
        assert_eq!(run(&relay("desktop"), &Task::Delete(song.key.clone())), Moved::Deleted);
        assert_eq!(run(&relay("phone"), &Task::Fetch(song.key.clone())), Moved::Gone(song.key.clone()));
        // Too big.
        let big = song_store::write_in(&vec![1; 2 * 1024 * 1024], "Big.wav", &root.join("desktop")).unwrap();
        let info = client::info(&server).unwrap();
        assert_eq!(info.relay_song_mb, 1);
        assert!(fits(&song_store::path_of(&root.join("desktop"), &song.key), info.relay_song_mb));
        assert!(!fits(&song_store::path_of(&root.join("desktop"), &big.key), info.relay_song_mb), "known too big before sending");
        assert!(matches!(run(&relay("desktop"), &send(&big.key)), Moved::Failed(..)));
        assert_eq!(run_all("desktop", &[send(&big.key)]), [Moved::TooBig(big.key.clone())], "not tried");
        // Past the identity's room: the oldest go.
        let first = song_store::write_in(&vec![2; 600 * 1024], "First.wav", &root.join("desktop")).unwrap();
        let second = song_store::write_in(&vec![3; 600 * 1024], "Second.wav", &root.join("desktop")).unwrap();
        assert_eq!(run(&relay("desktop"), &send(&first.key)), Moved::Sent(first.key.clone()));
        assert_eq!(run(&relay("desktop"), &send(&second.key)), Moved::Sent(second.key.clone()));
        assert_eq!(run(&relay("phone"), &Task::Fetch(first.key.clone())), Moved::Gone(first.key.clone()));
        assert_eq!(run(&relay("phone"), &Task::Fetch(second.key.clone())), Moved::Fetched(second.key.clone()));
        // Sent only while there's room, so none go unfetched: the rest wait,
        // and fetching goes on.
        let mb = 1024 * 1024;
        let sealed = crate::library::sync::sealed_len(600 * 1024);
        assert_eq!(client::relay_room(&server, &server_key, &identity).unwrap(), mb - sealed);
        let third = song_store::write_in(&vec![4; 600 * 1024], "Third.wav", &root.join("desktop")).unwrap();
        let fourth = song_store::write_in(&vec![5; 500 * 1024], "Fourth.wav", &root.join("desktop")).unwrap();
        let tasks = [send(&third.key), Task::Fetch(second.key.clone()), send(&fourth.key)];
        assert_eq!(run_all("desktop", &tasks), [Moved::Full(2), Moved::Fetched(second.key.clone())]);
        let tasks = [Task::Delete(second.key.clone()), send(&third.key), send(&fourth.key)];
        assert_eq!(run_all("desktop", &tasks), [Moved::Deleted, Moved::Sent(third.key.clone()), Moved::Full(1)]);
        assert_eq!(run(&relay("phone"), &Task::Fetch(third.key.clone())), Moved::Fetched(third.key.clone()), "not pushed out");

        // A device listening gets songs straight from the sender, through
        // the server but kept nowhere: of any size, whatever room is left.
        let huge = song_store::write_in(&vec![6; 3 * mb as usize], "Huge.wav", &root.join("desktop")).unwrap();
        let wanted = wanted_ids(&[7; 32], &BTreeSet::from([huge.key.clone()]));
        let phone = relay("phone");
        let listener = std::thread::spawn(move || listen(&phone, "phone", |id| wanted.get(id).cloned()));
        let inbox = crate::library::sync::inbox_id(&[7; 32], "phone");
        while client::relay_listening(&server, &server_key, &identity, std::slice::from_ref(&inbox)).unwrap().is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let tasks = [Task::Send { key: huge.key.clone(), to: vec!["tablet".into(), "phone".into()] }];
        assert_eq!(run_all("desktop", &tasks), [Moved::Streamed { key: huge.key.clone(), to: "phone".into() }]);
        assert_eq!(listener.join().unwrap(), Ok(Some(Moved::Fetched(huge.key.clone()))));
        assert_eq!(std::fs::metadata(song_store::path_of(&root.join("phone"), &huge.key)).unwrap().len(), 3 * mb);
        let kept: u64 = std::fs::read_dir(root.join("relay")).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        assert!(kept < mb, "not kept on the server");
        // Still listening a moment after (between songs). The tablet never
        // was: a song for it goes to the relay, which keeps none this big.
        assert_eq!(client::relay_listening(&server, &server_key, &identity, &[inbox.clone()]).unwrap(), [inbox]);
        let tasks = [Task::Send { key: huge.key.clone(), to: vec!["tablet".into()] }];
        assert_eq!(run_all("desktop", &tasks), [Moved::TooBig(huge.key.clone())]);
        running.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hubs_checks_ride_along_in_its_leaf() {
        let leaf: HubLeaf = serde_json::from_str(r#"{"device":"hub","key":"k","addrs":["http://203.0.113.5:7390"]}"#).unwrap();
        assert!(leaf.checks.is_empty(), "a leaf from before checks");
        let checked = HubLeaf { checks: vec![AddressCheck { url: leaf.addrs[0].clone(), at: 1, works: true, hint: None }], ..leaf.clone() };
        assert!(checked.same_place(&leaf), "new checks don't make it another hub to reach");
        assert!(!HubLeaf { addrs: Vec::new(), ..leaf.clone() }.same_place(&leaf));
        assert_eq!(serde_json::from_value::<HubLeaf>(serde_json::to_value(&checked).unwrap()).unwrap(), checked);
    }

    #[test]
    fn the_hub_takes_songs_and_hands_them_out() {
        let root = std::env::temp_dir().join(format!("synththing-hub-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut desktop = SongStore::open(root.join("desktop"));
        let (common, rare) = (song_store::write_in(b"common", "Common.mid", desktop.dir()).unwrap(), song_store::write_in(b"rare", "Rare.mid", desktop.dir()).unwrap());
        desktop.add(common.clone());
        desktop.add(rare.clone());
        let (laptop, phone) = (SongStore::open(root.join("laptop")), SongStore::open(root.join("phone")));
        let none = BTreeSet::new();
        let have = |keys: &[&str]| serde_json::json!(keys.iter().map(|k| &k[..HAVE_PREFIX]).collect::<Vec<_>>());
        let device = |pulls: bool| serde_json::to_value(DeviceLeaf { name: "Phone".into(), seen: 1, pulls, hub_only: false }).unwrap();
        let mut leaves = BTreeMap::from([
            (want("hub", &common.key), Value::Bool(true)),
            (want("hub", &rare.key), Value::Bool(true)),
            (want("phone", &rare.key), Value::Bool(true)),
            (format!("{HAVE}desktop"), have(&[&common.key, &rare.key])),
            (format!("{HAVE}laptop"), have(&[&common.key])),
            (format!("{HAVE}hub"), have(&[])),
            ("device/phone".into(), device(true)),
        ]);
        let hub_plan = |direct: bool| HubPlan { device: "hub".into(), direct, only: false };

        // The hub lacks both: the desktop sends them, the one only it has
        // first, and the rare one to the phone too, the hub not having it.
        let tasks = plan(&leaves, "desktop", |k| desktop.has(k), &none, &none, &none, Some(&hub_plan(true))).tasks;
        assert_eq!(tasks, [Task::Send { key: rare.key.clone(), to: vec!["hub".into(), "phone".into()] }, Task::Send { key: common.key.clone(), to: vec!["hub".into()] }]);
        assert!(plan(&leaves, "laptop", |k| laptop.has(k), &none, &none, &none, Some(&hub_plan(true))).tasks.is_empty(), "the laptop has nothing it wants");

        // Once the hub has them, the phone takes the rare one from it, and
        // no one sends it there.
        leaves.retain(|path, _| !path.starts_with("want/hub/"));
        leaves.insert(format!("{HAVE}hub"), have(&[&common.key, &rare.key]));
        let wants = BTreeSet::from([rare.key.clone()]);
        assert!(plan(&leaves, "desktop", |k| desktop.has(k), &none, &none, &none, Some(&hub_plan(true))).tasks.is_empty());
        assert!(plan(&leaves, "hub", |k| desktop.has(k), &none, &none, &none, Some(&hub_plan(true))).tasks.is_empty(), "the hub doesn't send it either");
        assert_eq!(plan(&leaves, "phone", |k| phone.has(k), &wants, &none, &none, Some(&hub_plan(true))).tasks, [Task::Pull(rare.key.clone())]);
        // A phone that doesn't reach the hub gets it sent, from either.
        assert!(plan(&leaves, "phone", |k| phone.has(k), &wants, &none, &none, Some(&hub_plan(false))).tasks.is_empty());
        leaves.insert("device/phone".into(), device(false));
        let to_phone = [Task::Send { key: rare.key.clone(), to: vec!["phone".into()] }];
        assert_eq!(plan(&leaves, "desktop", |k| desktop.has(k), &none, &none, &none, Some(&hub_plan(true))).tasks, to_phone);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_device_only_through_the_hub_trades_with_it_alone() {
        let root = std::env::temp_dir().join(format!("synththing-hub-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut desktop = SongStore::open(root.join("desktop"));
        let mut phone = SongStore::open(root.join("phone"));
        let mut hub = SongStore::open(root.join("hub"));
        let ours = song_store::write_in(b"desktop's", "Desk.mid", desktop.dir()).unwrap();
        let theirs = song_store::write_in(b"phone's", "Phone.mid", phone.dir()).unwrap();
        desktop.add(ours.clone());
        phone.add(theirs.clone());
        let none = BTreeSet::new();
        let leaves = BTreeMap::from([
            (want("hub", &ours.key), Value::Bool(true)),
            (want("phone", &ours.key), Value::Bool(true)),
            (want("hub", &theirs.key), Value::Bool(true)),
            (want("desktop", &theirs.key), Value::Bool(true)),
            ("device/phone".into(), serde_json::to_value(DeviceLeaf { name: "Phone".into(), seen: 1, pulls: false, hub_only: true }).unwrap()),
        ]);
        let hub_plan = |only: bool| HubPlan { device: "hub".into(), direct: false, only };
        let sends = |device: &str, store: &SongStore, only: bool| plan(&leaves, device, |k| store.has(k), &none, &none, &none, Some(&hub_plan(only))).tasks;
        let to = |key: &str, to: &[&str]| vec![Task::Send { key: key.to_string(), to: to.iter().map(|d| d.to_string()).collect() }];
        assert_eq!(sends("desktop", &desktop, false), to(&ours.key, &["hub"]), "not to the phone");
        assert_eq!(sends("phone", &phone, true), to(&theirs.key, &["hub"]), "nor from it but to the hub");
        hub.add(song_store::write_in(b"desktop's", "Desk.mid", hub.dir()).unwrap());
        assert_eq!(sends("hub", &hub, false), to(&ours.key, &["phone"]), "the hub sends it on");
        // With no hub, it trades as any device does.
        assert_eq!(plan(&leaves, "desktop", |k| desktop.has(k), &none, &none, &none, None).tasks, to(&ours.key, &["hub", "phone"]));
        let _ = std::fs::remove_dir_all(&root);
    }

}
