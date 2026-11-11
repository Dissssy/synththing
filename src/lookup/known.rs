//! Songs that belong on releases the lookup already knows: once one song
//! of an album is matched, its release's track list is kept, and songs
//! on no release whose titles are its missing tracks' (with one more clue
//! that they're those tracks) are offered in Clean up, a row for each
//! release, to match to it.

use std::collections::{BTreeMap, HashMap};

use crate::cleanup::{Kind, Suggestion, What};
use crate::fields::{Facts, Field};
use crate::file_tags;
use crate::song_store::{self, SongStore};

use super::matching::{self, Release};
use super::{Kept, details};

/// How alike a song's title and a track's must be.
const SAME_TITLE: f64 = 0.9;
/// The longest a gap can be between two songs coming in for them to have
/// come in together, in one import (seconds).
const BATCH_GAP: i64 = 10 * 60;

/// The tracks of `release` (by index) that none of `on_it` (the songs
/// matched to it) is: by its recording, or by its disc and place.
pub fn missing(release: &Release, on_it: &[Facts]) -> Vec<usize> {
    (0..release.tracks.len())
        .filter(|&t| {
            let track = &release.tracks[t];
            let recorded = |f: &Facts| track.recording.is_some() && f.recording() == track.recording.as_deref();
            let placed =
                |f: &Facts| f.number(Field::Disc).unwrap_or(1.0) as u32 == track.disc && f.number(Field::Track).map(|n| n as u32) == Some(track.position);
            !on_it.iter().any(|f| recorded(f) || placed(f))
        })
        .collect()
}

/// Which import each of `store`'s songs came in with: songs that came in
/// no more than `BATCH_GAP` apart (in a chain) are one batch.
pub fn batches(store: &SongStore) -> HashMap<&str, usize> {
    let mut songs: Vec<(i64, &str)> = store.known().map(|(key, song)| (song.imported, key)).collect();
    songs.sort_unstable();
    let mut batch = 0;
    let mut last = None;
    songs
        .into_iter()
        .map(|(at, key)| {
            if last.is_some_and(|last| at - last > BATCH_GAP) {
                batch += 1;
            }
            last = Some(at);
            (key, batch)
        })
        .collect()
}

/// A song of the library that's on no release, as it's compared with
/// tracks.
struct Loose<'s> {
    key: &'s str,
    title: String,
    track: Option<u32>,
    disc: Option<u32>,
    length: Option<f64>,
    batch: Option<usize>,
    /// Releases it isn't on (Wrong match).
    rejected: &'s [String],
}

/// Clean up's On a matched album rows: for each release `kept` that
/// songs of `store` are matched to, the songs on none whose titles are
/// its missing tracks', each to its track.
pub fn suggestions(store: &SongStore, kept: &Kept) -> Vec<Suggestion> {
    let batches = batches(store);
    // (Each release's songs, and the batches they came in with.)
    let mut on: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut loose = Vec::new();
    for (key, song) in store.known() {
        if store.is_blocked(key) || super::is_midi(&song.name) {
            continue;
        }
        let facts = Facts::of(song);
        if let Some(release) = facts.release() {
            on.entry(release).or_default().push(key);
            continue;
        }
        let title = facts.text(Field::Title).map_or_else(|| file_stem(&song.name).to_string(), |t| t.into_owned());
        let numbered = file_tags::number_in_title(&title).map(|(n, _)| n);
        loose.push(Loose {
            key,
            track: facts.number(Field::Track).map(|n| n as u32).or(numbered),
            disc: facts.number(Field::Disc).map(|n| n as u32),
            length: facts.number(Field::Length),
            batch: batches.get(key).copied(),
            rejected: song.online.as_ref().map_or(&[][..], |o: &song_store::Online| &o.rejected),
            title,
        });
    }
    if loose.is_empty() {
        return Vec::new();
    }
    // Each loose song's places: (release, track) it fits.
    let mut fits: HashMap<&str, Vec<(String, usize)>> = HashMap::new();
    let mut releases: BTreeMap<String, std::sync::Arc<Release>> = BTreeMap::new();
    for (id, keys) in on {
        let Some(release) = kept.release(id) else { continue };
        let facts: Vec<Facts> = keys.iter().filter_map(|k| store.get_by_key(k)).map(Facts::of).collect();
        let missing = missing(&release, &facts);
        if missing.is_empty() {
            continue;
        }
        let album_batches: Vec<usize> = keys.iter().filter_map(|k| batches.get(k).copied()).collect();
        for song in loose.iter().filter(|s| !s.rejected.iter().any(|r| r == id)) {
            for &t in &missing {
                if fits_track(song, &release, t, &album_batches) {
                    fits.entry(song.key).or_default().push((id.to_string(), t));
                }
            }
        }
        releases.insert(id.to_string(), release);
    }
    // (A song that fits more than one track, or a track more than one
    // song fits, is left for a person to match.)
    let mut taken: HashMap<(&str, usize), usize> = HashMap::new();
    for places in fits.values().filter(|p| p.len() == 1) {
        *taken.entry((places[0].0.as_str(), places[0].1)).or_default() += 1;
    }
    let mut by_release: BTreeMap<&str, Vec<(&str, usize)>> = BTreeMap::new();
    for (key, places) in &fits {
        if let [(release, t)] = places.as_slice()
            && taken.get(&(release.as_str(), *t)) == Some(&1)
        {
            by_release.entry(release).or_default().push((key, *t));
        }
    }
    by_release
        .into_iter()
        .map(|(id, mut songs)| {
            let release = &releases[id];
            songs.sort_by_key(|(_, t)| *t);
            let n = songs.len();
            Suggestion {
                kind: Kind::OnMatchedAlbum,
                what: What::Tracks { release: id.to_string(), album: release.title.clone(), tracks: songs.iter().map(|(_, t)| *t).collect() },
                to: format!("{n} song{} on {}", if n == 1 { "" } else { "s" }, release.title),
                keys: songs.into_iter().map(|(k, _)| k.to_string()).collect(),
            }
        })
        .collect()
}

/// Whether `song` is `release`'s track `t`: alike in title, and alike in
/// one more way (its number, its length, or coming in with the songs of
/// the release here, in `album_batches`).
fn fits_track(song: &Loose, release: &Release, t: usize, album_batches: &[usize]) -> bool {
    let track = &release.tracks[t];
    let title = matching::similar(&song.title, &track.title).max(matching::similar(matching::track_title(&song.title, &release.title), &track.title));
    if title < SAME_TITLE {
        return false;
    }
    let numbered = song.track == Some(track.position) && song.disc.unwrap_or(track.disc) == track.disc;
    let long = song.length.zip(track.length).is_some_and(|(a, b)| (a - b).abs() <= details::LENGTH_LEEWAY);
    let together = song.batch.is_some_and(|b| album_batches.contains(&b));
    numbered || long || together
}

/// A file name without its extension.
fn file_stem(name: &str) -> &str {
    std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::matching::Track;

    fn track(position: u32, title: &str, length: f64) -> Track {
        Track { disc: 1, position, title: title.into(), length: Some(length), recording: Some(format!("rec{position}")), credits: Vec::new() }
    }

    fn loose<'s>(title: &str, number: Option<u32>, length: Option<f64>, batch: Option<usize>) -> Loose<'s> {
        Loose { key: "k", title: title.into(), track: number, disc: None, length, batch, rejected: &[] }
    }

    #[test]
    fn a_title_needs_another_clue() {
        let release = Release {
            title: "Mr. Sleepy Man Soundtrack".into(),
            tracks: vec![track(1, "Mr. Sleepy Man Theme", 151.0), track(2, "Intro", 60.0)],
            ..Default::default()
        };
        assert!(fits_track(&loose("Mr. Sleepy Man Theme", Some(1), None, None), &release, 0, &[]), "its number");
        assert!(fits_track(&loose("Mr Sleepy Man Theme", None, Some(155.0), None), &release, 0, &[]), "its length");
        assert!(fits_track(&loose("Mr. Sleepy Man Theme", None, None, Some(3)), &release, 0, &[3]), "came in together");
        assert!(!fits_track(&loose("Mr. Sleepy Man Theme", None, None, Some(2)), &release, 0, &[3]), "a title alone");
        assert!(!fits_track(&loose("Intro", Some(5), Some(200.0), None), &release, 1, &[]), "clues that don't fit");
        assert!(!fits_track(&loose("Outro", Some(2), None, None), &release, 1, &[]), "another title");
        assert!(fits_track(&loose("02 - Intro", None, Some(61.0), None), &release, 1, &[]), "numbered in its title");
    }

    #[test]
    fn loose_songs_are_offered_to_the_release_one_of_them_is_on() {
        use crate::song_store::{META_VERSION, Meta, Online, Song};
        let song = |imported, title: &str, number| Song {
            name: format!("{title}.ogg"),
            imported,
            meta: Some(Meta { title: Some(title.into()), track: number, version: META_VERSION, ..Meta::default() }),
            ..Song::default()
        };
        let key = |letter: &str| format!("{}.ogg", letter.repeat(64));
        let mut matched = song(100, "Theme", Some(1));
        matched.online = Some(Online { release: Some("r".into()), recording: Some("rec1".into()), ..Online::default() });
        let mut rejected = song(5_000, "Credits", Some(4));
        rejected.online = Some(Online { rejected: vec!["r".into()], ..Online::default() });
        let dir = std::env::temp_dir().join(format!("synththing-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir);
        store.learn([
            (key("a"), matched),
            (key("b"), song(160, "Town", None)),
            (key("c"), song(9_000, "Forest", Some(3))),
            (key("d"), song(9_000, "Forest", Some(3))),
            (key("e"), song(9_000, "Town", None)),
            (key("f"), rejected),
        ]);
        let kept = Kept::default();
        let tracks = ["Theme", "Town", "Forest", "Credits"].iter().enumerate().map(|(i, t)| Track { length: None, ..track(i as u32 + 1, t, 0.0) }).collect();
        kept.keep_release(Release { id: "r".into(), title: "Game OST".into(), tracks, ..Default::default() });
        let found = suggestions(&store, &kept);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys, [key("b")], "the one that came in with Theme; two copies of Forest, and a song not on it, are left");
        assert_eq!(found[0].what, What::Tracks { release: "r".into(), album: "Game OST".into(), tracks: vec![1] });
    }
}
