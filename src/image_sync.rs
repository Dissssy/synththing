//! Pictures between devices (`settings_sync.rs` carries the transfers, as
//! a blob of their own; the files go through the relay). A playlist's
//! picture syncs with it by its key (`playlist_sync.rs`), an album's with
//! its songs (`song_store::Art::Image`) and an artist's in the library
//! (`song_sync.rs`); a device that shows a picture it lacks wants it (`want/<device>/<key>`), a
//! device that has it sends it to the relay and marks it ready
//! (`ready/<key>`), and once it's fetched and no one wants it, it's
//! deleted from the relay: as songs go (`song_sync::plan`), without the
//! hub or streaming, pictures being small.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::playlist::Library;
use crate::song_store::{Art, SongStore};
use crate::song_sync::{self, Plan};

/// The synced data's name on the server.
pub const NAME: &str = "images";

/// Whether the picture `key` is here.
fn here(key: &str) -> bool {
    crate::images::path(key).is_ok_and(|p| p.exists())
}

/// The pictures this device wants: those its playlists, albums and
/// artists show that aren't here.
pub fn wanted(playlists: &Library, songs: &SongStore) -> BTreeSet<String> {
    let lists = playlists.lists.iter().filter_map(|l| l.playlist.image.as_deref());
    let albums = songs.known().filter_map(|(_, song)| match song.online.as_ref()?.art.as_ref()? {
        Art::Image(key) => Some(key.as_str()),
        _ => None,
    });
    let artists = songs.artist_images().values().map(String::as_str);
    lists.chain(albums).chain(artists).filter(|key| crate::song_store::is_key(key) && !here(key)).map(str::to_string).collect()
}

/// What `device`, wanting `wanted`, does about the transfers synced so far
/// (`applied`), leaving alone the pictures `busy` already being moved.
pub fn plan(applied: &BTreeMap<String, Value>, device: &str, wanted: &BTreeSet<String>, busy: &BTreeSet<String>) -> Plan {
    song_sync::plan(applied, device, here, wanted, busy, &BTreeSet::new(), None)
}
