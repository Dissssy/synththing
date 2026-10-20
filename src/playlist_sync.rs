//! Playlist sync (`settings_sync.rs` carries it, as a blob of its own):
//! the playlists, the same on every device signed in as one identity that
//! syncs them, as leaves by playlist ID. A leaf is a whole playlist (its
//! name and songs), so a playlist changed on two devices takes the newer
//! change. Which soundfont an entry uses isn't synced: soundfonts are each
//! device's own.
//!
//! Each song goes by its key in the song library, with its name, so a
//! device that doesn't have it yet knows what it is (`SongStore::know`),
//! and shows it as unavailable until it's there.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::playlist::{self, Library, Playlist, PlaylistEntry};
use crate::settings_sync::Fields;
use crate::song_store::{self, SongStore};

/// The synced data's name on the server.
pub const NAME: &str = "playlists";

/// A playlist as synced.
#[derive(Serialize, Deserialize)]
struct SyncedPlaylist {
    name: String,
    songs: Vec<SyncedSong>,
}

#[derive(Serialize, Deserialize)]
struct SyncedSong {
    key: String,
    name: String,
}

/// The playlists as leaves: each by its ID.
pub fn leaves(library: &Library, store: &SongStore) -> BTreeMap<String, Value> {
    library
        .lists
        .iter()
        .map(|stored| {
            let songs = stored
                .playlist
                .entries
                .iter()
                .filter_map(|e| {
                    let key = song_store::key_in(library.songs_dir(), &e.path)?.to_string();
                    let name = store.get(&e.path).map_or_else(|| key.clone(), |s| s.name.clone());
                    Some(SyncedSong { key, name })
                })
                .collect();
            let synced = SyncedPlaylist { name: stored.playlist.name.clone(), songs };
            (stored.id.clone(), serde_json::to_value(synced).unwrap_or_default())
        })
        .collect()
}

/// Make `library` what `fields` say: playlists added, changed (an entry
/// keeping this device's soundfont while its song's still there) and
/// removed. The IDs of those changed or removed.
pub fn apply(library: &mut Library, store: &mut SongStore, fields: &Fields) -> Result<Vec<String>, String> {
    let current = leaves(library, store);
    let mut touched = Vec::new();
    for (id, field) in fields {
        let Ok(mut synced) = serde_json::from_value::<SyncedPlaylist>(field.value.clone()) else { continue };
        synced.songs.retain(|s| song_store::is_key(&s.key));
        if !playlist::is_id(id) || current.get(id) == serde_json::to_value(&synced).ok().as_ref() {
            continue;
        }
        let mut soundfonts: Vec<(std::path::PathBuf, std::path::PathBuf)> = library
            .find(id)
            .map(|idx| {
                let entries = &library.lists[idx].playlist.entries;
                entries.iter().filter_map(|e| Some((e.path.clone(), e.soundfont.clone()?))).collect()
            })
            .unwrap_or_default();
        let mut entries = Vec::new();
        for song in &synced.songs {
            store.know(&song.key, &song.name);
            let mut entry = PlaylistEntry::new(store.path(&song.key));
            if let Some(at) = soundfonts.iter().position(|(path, _)| *path == entry.path) {
                entry.soundfont = Some(soundfonts.remove(at).1);
            }
            entries.push(entry);
        }
        library.put(id, Playlist { name: synced.name, entries }).map_err(|e| format!("{e:#}"))?;
        touched.push(id.clone());
    }
    for id in current.keys().filter(|id| !fields.contains_key(*id)) {
        if let Some(idx) = library.find(id) {
            library.delete(idx).map_err(|e| format!("{e:#}"))?;
            touched.push(id.clone());
        }
    }
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_sync::Field;

    #[test]
    fn playlists_come_and_go_whole() {
        let root = std::env::temp_dir().join(format!("synththing-playlist-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (music, lists) = (root.join("music"), root.join("playlists"));
        let mut store = SongStore::open(music.clone());
        let here = song_store::write_in(b"here", "Here.mid", &music).unwrap();
        store.add(here.clone());
        let (mut library, _) = Library::load(lists.clone(), music.clone());
        let mine = library.create("Mine", vec![song_store::path_of(&music, &here.key)]).unwrap();
        library.lists[mine].playlist.entries[0].soundfont = Some("/piano.sf2".into());
        library.save(mine).unwrap();
        let mine = library.lists[mine].id.clone();

        let sent = leaves(&library, &store);
        assert_eq!(sent[&mine]["songs"][0]["name"], "Here.mid");
        assert!(sent[&mine]["songs"][0].get("soundfont").is_none(), "the device's own");

        // From another device: Mine renamed with a song this one lacks, and
        // a new playlist; one this device had is gone.
        let elsewhere = format!("{}.ogg", "b".repeat(64));
        let gone = library.create("Gone", Vec::new()).unwrap();
        let gone = library.lists[gone].id.clone();
        let field = |value: Value| Field { value, changed: 1 };
        let song = |key: &str, name: &str| serde_json::json!({ "key": key, "name": name });
        let mut fields = Fields::new();
        fields.insert(mine.clone(), field(serde_json::json!({ "name": "Ours", "songs": [song(&here.key, "Here.mid"), song(&elsewhere, "Far.ogg")] })));
        fields.insert("00000000000000aa".into(), field(serde_json::json!({ "name": "New", "songs": [song("../escape", "x")] })));
        let mut touched = apply(&mut library, &mut store, &fields).unwrap();
        touched.sort();
        let mut expected = vec![mine.clone(), "00000000000000aa".to_string(), gone.clone()];
        expected.sort();
        assert_eq!(touched, expected);

        let ours = &library.lists[library.find(&mine).unwrap()].playlist;
        assert_eq!(ours.name, "Ours");
        assert_eq!(ours.entries[0].soundfont.as_deref(), Some(std::path::Path::new("/piano.sf2")), "kept here");
        assert!(ours.entries[1].missing);
        assert_eq!(store.get(&ours.entries[1].path).unwrap().name, "Far.ogg");
        assert!(library.find(&gone).is_none());
        assert!(library.lists[library.find("00000000000000aa").unwrap()].playlist.entries.is_empty());
        // Applying the same again changes nothing.
        assert!(apply(&mut library, &mut store, &fields).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
