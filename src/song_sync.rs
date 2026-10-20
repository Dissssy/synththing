//! Songs between devices (`settings_sync.rs` carries the lists, as blobs
//! of their own; `library/sync.rs` the files):
//!
//! - `library`: every song in any device's library, by key, with its name
//!   and tags (songs are only ever added: a song removed on one device
//!   stays in the others'), and the tags themselves (`tag/<id>`). Each
//!   device knows them all (`SongStore::learn`).
//! - `transfers`: which songs each device wants (`want/<device>/<key>`)
//!   and which are waiting on the relay (`ready/<key>`). A device wants a
//!   song it lacks and hasn't blocked that's in a playlist, or, taking
//!   every song, in the library. A device that has a song another wants
//!   sends it to the relay and marks it ready; the one that wanted it
//!   fetches it and stops wanting it; once no one does, it's deleted from
//!   the relay.

use std::collections::{BTreeMap, BTreeSet};
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
    for (key, field) in fields {
        if let Some(id) = key.strip_prefix(TAG) {
            if let Ok(tag) = serde_json::from_value::<Tag>(field.value.clone()) {
                store.set_tag(id, tag);
            }
        } else if let Ok(song) = serde_json::from_value::<SyncedSong>(field.value.clone()) {
            store.learn(key, &song.name, song.tags);
        }
    }
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
        .filter(|e| e.path.parent() == Some(store.dir()))
        .filter_map(|e| e.path.file_name()?.to_str())
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
    /// Send this song (by key) to the relay: another device wants it.
    Send(String),
    /// Fetch it from the relay: this device wants it.
    Fetch(String),
    /// Delete it from the relay: no device wants it now.
    Delete(String),
}

impl Task {
    pub fn key(&self) -> &str {
        match self {
            Task::Send(key) | Task::Fetch(key) | Task::Delete(key) => key,
        }
    }
}

fn want(device: &str, key: &str) -> String {
    format!("want/{device}/{key}")
}

fn ready(key: &str) -> String {
    format!("ready/{key}")
}

/// What `device`, wanting `wanted`, does about the transfers synced so far
/// (`applied`), leaving alone the songs `busy` already being moved.
pub fn plan(applied: &BTreeMap<String, Value>, device: &str, store: &SongStore, wanted: &BTreeSet<String>, busy: &BTreeSet<String>) -> Plan {
    let mine = format!("want/{device}/");
    let mut leaves: BTreeMap<String, Value> = applied.iter().filter(|(path, _)| !path.starts_with(&mine)).map(|(p, v)| (p.clone(), v.clone())).collect();
    leaves.extend(wanted.iter().map(|key| (want(device, key), Value::Bool(true))));
    // Who wants each song.
    let mut wanters: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for path in leaves.keys() {
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
            tasks.push(Task::Send(key.to_string()));
        }
    }
    for task in &tasks {
        if let Task::Delete(key) = task {
            leaves.remove(&ready(key));
        }
    }
    Plan { leaves, tasks }
}

/// Whether the song at `path`, sealed, is within a relay's `relay_song_mb`.
pub fn fits(path: &Path, relay_song_mb: u64) -> bool {
    // (Sealing adds a nonce and a tag.)
    std::fs::metadata(path).map_or(true, |m| m.len() + 28 <= relay_song_mb * 1024 * 1024)
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
#[derive(Debug, PartialEq)]
pub enum Moved {
    /// Sent: mark it ready.
    Sent(String),
    /// Fetched into the library.
    Fetched(String),
    /// It wasn't on the relay after all (it waited too long): unmark it,
    /// to be sent again.
    Gone(String),
    Deleted,
    Failed(String, String),
}

/// Do `task` (off the main thread: songs are big).
pub fn run(relay: &Relay, task: &Task) -> Moved {
    let key = task.key().to_string();
    let id = crate::library::sync::relay_id(&relay.sync_key, &key);
    let (server, server_key, identity) = (&relay.server, &relay.server_key, &relay.identity);
    let done = match task {
        Task::Send(_) => std::fs::read(relay.dir.join(&key))
            .map_err(|e| format!("couldn't read it: {e}"))
            .and_then(|bytes| client::relay_put(server, server_key, identity, &relay.sync_key, &id, &bytes))
            .map(|()| Moved::Sent(key.clone())),
        Task::Fetch(_) => match client::relay_get(server, server_key, identity, &relay.sync_key, &id) {
            Ok(Some(bytes)) => song_store::write_as(&bytes, &key, &relay.dir).map(|()| Moved::Fetched(key.clone())).map_err(|e| format!("{e:#}")),
            Ok(None) => Ok(Moved::Gone(key.clone())),
            Err(e) => Err(e),
        },
        Task::Delete(_) => client::relay_delete(server, server_key, identity, &id).map(|()| Moved::Deleted),
    };
    done.unwrap_or_else(|e| Moved::Failed(key, e))
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
        Moved::Fetched(_) | Moved::Deleted | Moved::Failed(..) => {}
    }
    leaves
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
        let asked = plan(&BTreeMap::new(), "phone", &phone, &wants, &none);
        assert!(asked.tasks.is_empty(), "no one has it to send yet");
        // The desktop sends it, and marks it ready.
        let sending = plan(&asked.leaves, "desktop", &desktop, &BTreeSet::new(), &none);
        assert_eq!(sending.tasks, [Task::Send(song.key.clone())]);
        assert!(plan(&asked.leaves, "desktop", &desktop, &BTreeSet::new(), &BTreeSet::from([song.key.clone()])).tasks.is_empty(), "busy");
        let sent = after(sending.leaves, &Moved::Sent(song.key.clone()));
        // The phone fetches it; once it has it, it stops wanting it, and
        // deletes it from the relay, as no one wants it now.
        assert_eq!(plan(&sent, "phone", &phone, &wants, &none).tasks, [Task::Fetch(song.key.clone())]);
        song_store::write_as(b"song", &song.key, &phone_dir).unwrap();
        phone.add(song_store::Stored { key: song.key.clone(), name: "Song.mid".into() });
        let fetched = plan(&sent, "phone", &phone, &wanted(&phone, &playlists, true), &none);
        assert_eq!(fetched.tasks, [Task::Delete(song.key.clone())]);
        assert!(fetched.leaves.is_empty());

        // Something else than the song, fetched as it, isn't kept.
        assert!(song_store::write_as(b"not it", &song.key, &root.join("elsewhere")).is_err());
        // Removed here: not wanted again.
        phone.remove(&phone_dir.join(&song.key)).unwrap();
        assert!(wanted(&phone, &playlists, true).is_empty());
        let _ = std::fs::remove_dir_all(&root);
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

        assert_eq!(run(&relay("desktop"), &Task::Send(song.key.clone())), Moved::Sent(song.key.clone()));
        assert_eq!(run(&relay("phone"), &Task::Fetch(song.key.clone())), Moved::Fetched(song.key.clone()));
        assert_eq!(std::fs::read(root.join("phone").join(&song.key)).unwrap(), b"a song");
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
        assert!(fits(&root.join("desktop").join(&song.key), info.relay_song_mb));
        assert!(!fits(&root.join("desktop").join(&big.key), info.relay_song_mb), "known too big before sending");
        assert!(matches!(run(&relay("desktop"), &Task::Send(big.key)), Moved::Failed(..)));
        // Past the identity's room: the oldest go.
        let first = song_store::write_in(&vec![2; 600 * 1024], "First.wav", &root.join("desktop")).unwrap();
        let second = song_store::write_in(&vec![3; 600 * 1024], "Second.wav", &root.join("desktop")).unwrap();
        assert_eq!(run(&relay("desktop"), &Task::Send(first.key.clone())), Moved::Sent(first.key.clone()));
        assert_eq!(run(&relay("desktop"), &Task::Send(second.key.clone())), Moved::Sent(second.key.clone()));
        assert_eq!(run(&relay("phone"), &Task::Fetch(first.key.clone())), Moved::Gone(first.key.clone()));
        assert_eq!(run(&relay("phone"), &Task::Fetch(second.key.clone())), Moved::Fetched(second.key.clone()));
        running.stop();
        let _ = std::fs::remove_dir_all(&root);
    }
}
