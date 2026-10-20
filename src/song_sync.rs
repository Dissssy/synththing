//! Songs between devices (`settings_sync.rs` carries the lists, as blobs
//! of their own; `library/sync.rs` the files):
//!
//! - `library`: every song in any device's library, by key, with its name
//!   and tags (songs are only ever added: a song removed on one device
//!   stays in the others'), and the tags themselves (`tag/<id>`). Each
//!   device knows them all (`SongStore::learn`).
//! - `transfers`: which songs each device wants (`want/<device>/<key>`)
//!   and which are waiting on the relay (`ready/<key>`), and the devices
//!   themselves (`device/<device>`: its name, and when it last said it's
//!   there, once a day). A device that says nothing for `FORGET_AFTER` is
//!   forgotten, wants and all, as is one removed by hand (`forget`); it
//!   comes back if it syncs again. A device wants a
//!   song it lacks and hasn't blocked that's in a playlist, or, taking
//!   every song, in the library. A device that has a song another wants
//!   sends it to the relay and marks it ready; the one that wanted it
//!   fetches it and stops wanting it; once no one does, it's deleted from
//!   the relay.
//!
//! A device online listens for songs (`listen`), and a device sending one
//! to it streams it straight there instead, through the server but kept
//! nowhere (`run_all`): it's fetched as it's sent. Only for a device not
//! listening does a song wait on the relay, and only while the relay has
//! room for it, so a sender never gets further ahead than that.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::library::client;
use crate::library::identity::Identity;
use crate::playlist::Library;
use crate::settings_sync::Fields;
use crate::song_store::{self, SongStore, Tag};

/// The synced data's names on the server.
pub const LIBRARY: &str = "library";
pub const TRANSFERS: &str = "transfers";

/// A song in the synced library.
#[derive(Serialize, Deserialize)]
struct SyncedSong {
    name: String,
    #[serde(default)]
    tags: Vec<String>,
}

const TAG: &str = "tag/";

/// The library's leaves: those synced so far (`applied`), with what this
/// device has of them, every song here, and the tags. (A song only known
/// here, not here nor synced yet, waits for the synced library: its tags
/// are there.)
pub fn library_leaves(store: &SongStore, applied: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut leaves: BTreeMap<String, Value> = applied.iter().filter(|(k, _)| !k.starts_with(TAG)).map(|(k, v)| (k.clone(), v.clone())).collect();
    for (key, song) in store.known() {
        if applied.contains_key(key) || store.has(key) {
            let synced = SyncedSong { name: song.name.clone(), tags: song.tags.clone() };
            leaves.insert(key.to_string(), serde_json::to_value(synced).unwrap_or_default());
        }
    }
    for (id, tag) in store.tags() {
        leaves.insert(format!("{TAG}{id}"), serde_json::to_value(tag).unwrap_or_default());
    }
    leaves
}

/// Take the synced library: every song's name and tags, and the tags
/// (those gone from it deleted here).
pub fn apply_library(store: &mut SongStore, fields: &Fields) {
    let mut songs = Vec::new();
    for (key, field) in fields {
        if let Some(id) = key.strip_prefix(TAG) {
            if let Ok(tag) = serde_json::from_value::<Tag>(field.value.clone()) {
                store.set_tag(id, tag);
            }
        } else if let Ok(song) = serde_json::from_value::<SyncedSong>(field.value.clone()) {
            songs.push((key.clone(), song.name, song.tags));
        }
    }
    store.learn(songs);
    let gone: Vec<String> = store.tags().keys().filter(|id| !fields.contains_key(&format!("{TAG}{id}"))).cloned().collect();
    for id in gone {
        store.remove_tag(&id);
    }
}

/// The songs this device wants: those it lacks and hasn't blocked, in a
/// playlist or (`every`) anywhere in the library.
pub fn wanted(store: &SongStore, playlists: &Library, every: bool) -> BTreeSet<String> {
    let lacks = |key: &str| song_store::is_key(key) && !store.has(key) && !store.is_blocked(key);
    if every {
        return store.known().map(|(key, _)| key).filter(|key| lacks(key)).map(str::to_string).collect();
    }
    playlists
        .lists
        .iter()
        .flat_map(|stored| &stored.playlist.entries)
        .filter_map(|e| song_store::key_in(store.dir(), &e.path))
        .filter(|key| lacks(key))
        .map(str::to_string)
        .collect()
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
    /// straight to one listening, or to the relay.
    Send { key: String, to: Vec<String> },
    /// Fetch it from the relay: this device wants it.
    Fetch(String),
    /// Delete it from the relay: no device wants it now.
    Delete(String),
}

impl Task {
    pub fn key(&self) -> &str {
        match self {
            Task::Send { key, .. } | Task::Fetch(key) | Task::Delete(key) => key,
        }
    }
}

/// The path of `device`'s want of the song `key`.
pub fn want(device: &str, key: &str) -> String {
    format!("want/{device}/{key}")
}

fn ready(key: &str) -> String {
    format!("ready/{key}")
}

/// What `device`, wanting `wanted`, does about the transfers synced so far
/// (`applied`), leaving alone the songs `busy` already being moved, and
/// the wants it already streamed a song for (`given`, as their paths: the
/// device it went to says it has it next).
pub fn plan(
    applied: &BTreeMap<String, Value>,
    device: &str,
    store: &SongStore,
    wanted: &BTreeSet<String>,
    busy: &BTreeSet<String>,
    given: &BTreeSet<String>,
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
    let mut tasks = Vec::new();
    for &key in &waiting {
        if !wanters.contains_key(key) && !busy.contains(key) {
            tasks.push(Task::Delete(key.to_string()));
        }
    }
    for (&key, who) in &wanters {
        if busy.contains(key) {
            continue;
        }
        if waiting.contains(key) {
            if who.contains(device) {
                tasks.push(Task::Fetch(key.to_string()));
            }
        } else if store.has(key) && who.iter().any(|&w| w != device) {
            let to = who.iter().filter(|&&w| w != device).map(|w| w.to_string()).collect();
            tasks.push(Task::Send { key: key.to_string(), to });
        }
    }
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
    };
    done.unwrap_or_else(|e| Moved::Failed(key, e))
}

/// Do `tasks` in order, telling `done` what came of each (it says whether
/// to go on). A song goes straight to a device listening for it if one is;
/// otherwise to the relay, only while it has room: the rest wait
/// (`Moved::Full`), rather than pushing out songs not fetched yet.
pub fn run_all(relay: &Relay, tasks: &[Task], room: Room, mut done: impl FnMut(Moved) -> bool) {
    let (server, server_key, identity) = (&relay.server, &relay.server_key, &relay.identity);
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
                while let Some(device) = streams_to(to, &listening) {
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
/// taking it if it's one of `wanted` (by relay ID: `wanted_ids`): what came
/// of it, if one came, or why it couldn't listen.
pub fn listen(relay: &Relay, device: &str, wanted: &HashMap<String, String>) -> Result<Option<Moved>, String> {
    let inbox = crate::library::sync::inbox_id(&relay.sync_key, device);
    let take = |id: &str| song_store::Incoming::new(wanted.get(id)?, &relay.dir).ok();
    let Some((id, incoming)) = client::relay_inbox(&relay.server, &relay.server_key, &relay.identity, &relay.sync_key, &inbox, take)? else {
        return Ok(None);
    };
    let key = wanted.get(&id).cloned().unwrap_or_default();
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
        Moved::Streamed { .. } | Moved::Fetched(_) | Moved::Deleted | Moved::Failed(..) | Moved::Full(_) | Moved::TooBig(_) => {}
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

/// The transfers' leaves without `device`'s: its wants, and it.
pub fn forget(leaves: &BTreeMap<String, Value>, device: &str) -> BTreeMap<String, Value> {
    leaves.iter().filter(|(path, _)| device_of(path) != Some(device)).map(|(p, v)| (p.clone(), v.clone())).collect()
}

/// The devices in `fields` other than `me` that haven't said anything for
/// `FORGET_AFTER` as of `now`.
pub fn gone_quiet(fields: &Fields, me: &str, now: i64) -> Vec<String> {
    devices(fields).into_iter().filter(|d| d.id != me && now - d.seen > FORGET_AFTER).map(|d| d.id).collect()
}

/// Note in the transfers' `leaves` that `device` is there, as `name`
/// (once a day: not to write the transfers over and over).
pub fn note_device(leaves: &mut BTreeMap<String, Value>, device: &str, name: &str, now: i64) {
    let path = format!("device/{device}");
    let noted = leaves.get(&path).and_then(|v| serde_json::from_value::<DeviceLeaf>(v.clone()).ok());
    if noted.is_some_and(|leaf| leaf.name == name && now - leaf.seen < SEEN_EVERY) {
        return;
    }
    leaves.insert(path, serde_json::to_value(DeviceLeaf { name: name.to_string(), seen: now }).unwrap_or_default());
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
        let library = library_leaves(&desktop, &BTreeMap::new());
        assert_eq!(library[&song.key]["name"], "Song.mid");
        let fields = library.iter().map(|(k, v)| (k.clone(), crate::settings_sync::Field { value: v.clone(), changed: 1 })).collect();
        apply_library(&mut phone, &fields);
        assert_eq!(phone.tags()["t1"].name, "Chill");
        assert_eq!(phone.known().find(|(k, _)| *k == song.key).unwrap().1.tags, ["t1"]);
        assert_eq!(library_leaves(&phone, &library), library, "nothing new to say");
        let (playlists, _) = Library::load(root.join("playlists"), phone_dir.clone());
        assert!(wanted(&phone, &playlists, false).is_empty(), "in no playlist");
        let wants = wanted(&phone, &playlists, true);
        assert_eq!(wants, BTreeSet::from([song.key.clone()]));

        let none = BTreeSet::new();
        let asked = plan(&BTreeMap::new(), "phone", &phone, &wants, &none, &none);
        assert!(asked.tasks.is_empty(), "no one has it to send yet");
        // The desktop sends it, and marks it ready.
        let sending = plan(&asked.leaves, "desktop", &desktop, &none, &none, &none);
        assert_eq!(sending.tasks, [Task::Send { key: song.key.clone(), to: vec!["phone".into()] }]);
        assert!(plan(&asked.leaves, "desktop", &desktop, &none, &BTreeSet::from([song.key.clone()]), &none).tasks.is_empty(), "busy");
        let given = BTreeSet::from([want("phone", &song.key)]);
        assert!(plan(&asked.leaves, "desktop", &desktop, &none, &none, &given).tasks.is_empty(), "streamed to it already");
        let sent = after(sending.leaves, &Moved::Sent(song.key.clone()));
        // The phone fetches it; once it has it, it stops wanting it, and
        // deletes it from the relay, as no one wants it now.
        assert_eq!(plan(&sent, "phone", &phone, &wants, &none, &none).tasks, [Task::Fetch(song.key.clone())]);
        let mut incoming = song_store::Incoming::new(&song.key, &phone_dir).unwrap();
        std::io::Write::write_all(&mut incoming, b"song").unwrap();
        incoming.finish().unwrap();
        phone.add(song_store::Stored { key: song.key.clone(), name: "Song.mid".into() });
        let fetched = plan(&sent, "phone", &phone, &wanted(&phone, &playlists, true), &none, &none);
        assert_eq!(fetched.tasks, [Task::Delete(song.key.clone())]);
        assert!(fetched.leaves.is_empty());

        // Something else than the song, fetched as it, isn't kept.
        let mut incoming = song_store::Incoming::new(&song.key, &root.join("elsewhere")).unwrap();
        std::io::Write::write_all(&mut incoming, b"not it").unwrap();
        assert!(incoming.finish().is_err());
        assert_eq!(std::fs::read_dir(root.join("elsewhere")).unwrap().count(), 0, "nothing left behind");
        // Removed here: not wanted again.
        phone.remove(&song_store::path_of(&phone_dir, &song.key)).unwrap();
        assert!(wanted(&phone, &playlists, true).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn devices_say_they_are_there_and_go_quiet() {
        let field = |value: Value, changed| crate::settings_sync::Field { value, changed };
        let day = 24 * 60 * 60;
        let mut leaves = BTreeMap::new();
        note_device(&mut leaves, "desk", "Desktop", 10 * day);
        let noted = leaves.clone();
        note_device(&mut leaves, "desk", "Desktop", 10 * day + 60);
        assert_eq!(leaves, noted, "once a day");
        note_device(&mut leaves, "desk", "Desktop", 11 * day);
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
        let relay = |dir: &str| Relay { server: server.clone(), server_key: server_key.clone(), identity: identity.clone(), sync_key: [7; 32], dir: root.join(dir) };
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
        let listener = std::thread::spawn(move || listen(&phone, "phone", &wanted));
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
}
