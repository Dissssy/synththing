//! Duplicate songs: the same recording in several files (another rip,
//! format or bitrate), for Clean up (`cleanup`). The library keys songs
//! by their file's hash, so these are songs of their own; they're found
//! as songs of one MusicBrainz recording (`Facts::recording`: its match,
//! else its file's tag), or guessed as songs of one title and artist and
//! about as long. MIDI files are never one with an audio file.
//! Each group's copies are ranked, the one to keep first: lossless
//! before lossy; lossless by sample rate, then bits per sample; lossy by
//! bitrate, then sample rate; then the one imported first. What the
//! others have that it lacks is merged onto it (`merge`) before they're
//! removed.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::fields::{self, Facts, Field};
use crate::song_store::{Meta, Online, Song, SongStore};

/// How far apart (seconds) two copies' lengths may be and still be
/// guessed one song.
const LEEWAY: f64 = 2.0;

/// Songs that are one recording: their keys, the one to keep first.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub title: String,
    pub keys: Vec<String>,
    /// Known to be one (a MusicBrainz recording), not guessed from their
    /// title, artist and length.
    pub sure: bool,
}

/// The library's duplicate songs (names grouped as by
/// `fields::group_key`, past `ignore`), the largest groups first. Songs
/// of one recording are sure; songs of one title and artist, as long as
/// each other give or take `LEEWAY`, of no more than one recording and
/// not all of it, are guessed (which takes in a sure group they reach).
pub fn groups(store: &SongStore, ignore: &str) -> Vec<Group> {
    let mut by_recording: BTreeMap<&str, Vec<(&str, &Song)>> = BTreeMap::new();
    let mut by_name: BTreeMap<(String, String), Vec<(f64, &str, &Song)>> = BTreeMap::new();
    for (key, song) in store.known().filter(|(key, _)| !store.is_blocked(key)) {
        let facts = Facts::of(song);
        if facts.is_midi() {
            continue;
        }
        if let Some(recording) = facts.recording() {
            by_recording.entry(recording).or_default().push((key, song));
        }
        if let (Some(title), Some(length)) = (facts.text(Field::Title), song.meta.as_ref().and_then(|m| m.length)) {
            let artist = facts.first(Field::Artist).map(|a| fields::group_key(&a, ignore)).unwrap_or_default();
            by_name.entry((fields::group_key(&title, ignore), artist)).or_default().push((length, key, song));
        }
    }
    let mut guessed: Vec<Vec<(&str, &Song)>> = Vec::new();
    for mut songs in by_name.into_values().filter(|songs| songs.len() > 1) {
        songs.sort_by(|a, b| a.0.total_cmp(&b.0));
        for run in songs.chunk_by(|a, b| b.0 - a.0 <= LEEWAY) {
            let recordings: BTreeSet<Option<&str>> = run.iter().map(|(_, _, s)| Facts::of(s).recording()).collect();
            if run.len() > 1 && recordings.contains(&None) && recordings.len() <= 2 {
                guessed.push(run.iter().map(|(_, key, song)| (*key, *song)).collect());
            }
        }
    }
    let taken: BTreeSet<&str> = guessed.iter().flatten().filter_map(|(_, s)| Facts::of(s).recording()).collect();
    let sure = by_recording.into_iter().filter(|(recording, songs)| songs.len() > 1 && !taken.contains(recording)).map(|(_, songs)| (songs, true));
    let mut groups: Vec<Group> = sure
        .chain(guessed.into_iter().map(|songs| (songs, false)))
        .map(|(mut songs, sure)| {
            songs.sort_by_cached_key(|(key, song)| (Reverse(rank(song)), *key));
            let title = Facts::of(songs[0].1).text(Field::Title).map_or_else(|| songs[0].1.name.clone(), |t| t.into_owned());
            Group { title, keys: songs.into_iter().map(|(key, _)| key.to_string()).collect(), sure }
        })
        .collect();
    groups.sort_by(|a, b| b.keys.len().cmp(&a.keys.len()).then_with(|| a.title.cmp(&b.title)));
    groups
}

/// How good a song's copy is, the better the greater.
fn rank(song: &Song) -> (bool, u32, u32, u32, Reverse<i64>) {
    let Some(meta) = &song.meta else {
        return (false, 0, 0, 0, Reverse(song.imported));
    };
    let rate = meta.sample_rate.unwrap_or(0);
    let kbps = kbps(meta).unwrap_or(0);
    if meta.lossless { (true, rate, meta.bits.unwrap_or(0), kbps, Reverse(song.imported)) } else { (false, kbps, rate, 0, Reverse(song.imported)) }
}

/// A file's bitrate (kb/s), from its size and length.
fn kbps(meta: &Meta) -> Option<u32> {
    let length = meta.length.filter(|l| *l > 0.0)?;
    Some((meta.size as f64 * 8.0 / length / 1000.0).round() as u32)
}

/// A copy, as Clean up describes it: "FLAC, 44.1 kHz, 16 bit", "MP3,
/// 320 kb/s, 44.1 kHz".
pub fn describe(song: &Song) -> String {
    let format = std::path::Path::new(&song.name).extension().and_then(|e| e.to_str()).unwrap_or("?").to_uppercase();
    let Some(meta) = &song.meta else {
        return format;
    };
    let rate = meta.sample_rate.map(|r| format!("{} kHz", f64::from(r) / 1000.0));
    let mut parts = vec![format];
    if meta.lossless {
        parts.extend(rate);
        parts.extend(meta.bits.map(|b| format!("{b} bit")));
    } else {
        parts.extend(kbps(meta).map(|k| format!("{k} kb/s")));
        parts.extend(rate);
    }
    parts.join(", ")
}

/// What the song kept gets from the others of its group.
#[derive(Debug, Default, PartialEq)]
pub struct Merged {
    /// Its tags, if it gets any (stars are a tag).
    pub tags: Option<Vec<String>>,
    /// Edits of fields it hasn't edited, as `SongStore::edit` takes them.
    pub edits: Vec<(String, Field, Option<String>)>,
    /// A match, if it has none.
    pub online: Option<Online>,
    /// Where it was left, if it remembers none.
    pub position: Option<f64>,
}

/// What `keep` gets from `others` (the better first wins where they
/// differ).
pub fn merge(store: &SongStore, keep: &str, others: &[String]) -> Merged {
    let Some(kept) = store.get_by_key(keep) else {
        return Merged::default();
    };
    let others: Vec<&Song> = others.iter().filter(|k| *k != keep).filter_map(|k| store.get_by_key(k)).collect();
    let mut tags = kept.tags.clone();
    let mut edits: BTreeMap<Field, String> = BTreeMap::new();
    for other in &others {
        tags.extend(other.tags.iter().filter(|t| !kept.tags.contains(t)).cloned());
        for (field, text) in &other.edits {
            if !kept.edits.contains_key(field) {
                edits.entry(*field).or_insert_with(|| text.clone());
            }
        }
    }
    tags.sort();
    tags.dedup();
    Merged {
        tags: (tags.len() > kept.tags.len()).then_some(tags),
        edits: edits.into_iter().map(|(field, text)| (keep.to_string(), field, Some(text))).collect(),
        online: if kept.online.as_ref().is_some_and(|o| o.release.is_some()) {
            None
        } else {
            others.iter().find_map(|o| o.online.clone().filter(|o| o.release.is_some()))
        },
        position: if kept.position.is_some() { None } else { others.iter().find_map(|o| o.position) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::{self, STARRED};

    fn song(name: &str, recording: &str, meta: Meta) -> Song {
        Song {
            name: name.into(),
            meta: Some(Meta { musicbrainz_recording: Some(recording.into()), version: song_store::META_VERSION, ..meta }),
            ..Song::default()
        }
    }

    /// A store of `songs`, each keyed by its letter (a hex digit) 64 times.
    fn store(name: &str, songs: Vec<(&str, Song)>) -> SongStore {
        let dir = std::env::temp_dir().join(format!("synththing-duplicates-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir);
        store.learn(songs.into_iter().map(|(key, song)| (key_of(key), song)));
        store
    }

    fn key_of(letter: &str) -> String {
        format!("{}.ogg", letter.repeat(64))
    }

    fn short(keys: &[String]) -> Vec<&str> {
        keys.iter().map(|k| &k[..1]).collect()
    }

    #[test]
    fn groups_rank_lossless_then_quality() {
        let mp3 = |kbps: u64| Meta { length: Some(100.0), size: kbps * 100 * 1000 / 8, sample_rate: Some(44100), ..Meta::default() };
        let flac = |rate, bits| Meta { length: Some(100.0), size: 30_000_000, sample_rate: Some(rate), bits: Some(bits), lossless: true, ..Meta::default() };
        let store = store(
            "rank",
            vec![
                ("a", song("Song.mp3", "r1", mp3(128))),
                ("b", song("Song.flac", "r1", flac(44100, 16))),
                ("c", song("Song.mp3", "r1", mp3(320))),
                ("d", song("Song.flac", "r1", flac(96000, 24))),
                ("e", song("Other.ogg", "r2", mp3(160))),
                ("f", song("Song.mid", "r1", Meta::default())),
            ],
        );
        let groups = groups(&store, "");
        assert_eq!(groups.len(), 1, "one copy alone, and MIDI, aren't duplicates");
        assert!(groups[0].sure);
        assert_eq!(short(&groups[0].keys), ["d", "b", "c", "a"]);
        assert_eq!(describe(store.get_by_key(&groups[0].keys[0]).unwrap()), "FLAC, 96 kHz, 24 bit");
        assert_eq!(describe(store.get_by_key(&groups[0].keys[2]).unwrap()), "MP3, 320 kb/s, 44.1 kHz");
    }

    #[test]
    fn guesses_by_title_artist_and_length() {
        let meta = |title: &str, artist: &str, length| Meta {
            title: Some(title.into()),
            artist: Some(artist.into()),
            length: Some(length),
            size: 1_000_000,
            ..Meta::default()
        };
        let untagged = |name: &str, meta: Meta| Song { name: name.into(), meta: Some(meta), ..Song::default() };
        let store = store(
            "guess",
            vec![
                ("a", untagged("a.mp3", meta("Smart Race", "Toby Fox", 100.0))),
                ("b", untagged("b.mp3", meta("smart race", "Toby Fox", 101.5))),
                ("c", untagged("c.mp3", meta("Smart Race", "Toby Fox", 140.0))),
                ("d", untagged("d.mp3", meta("Smart Race", "Someone Else", 100.0))),
                ("e", song("e.flac", "r1", meta("Spamton", "Toby Fox", 80.0))),
                ("f", untagged("f.mp3", meta("Spamton", "Toby Fox", 80.5))),
                ("1", song("1.flac", "r1", meta("Spamton", "Toby Fox", 79.0))),
                ("2", song("2.flac", "r2", meta("KEYGEN", "Toby Fox", 60.0))),
                ("3", song("3.flac", "r3", meta("KEYGEN", "Toby Fox", 60.0))),
                ("4", untagged("4.mp3", meta("KEYGEN", "Toby Fox", 60.0))),
            ],
        );
        let groups = groups(&store, "");
        let found: Vec<(Vec<&str>, bool)> = groups.iter().map(|g| (short(&g.keys), g.sure)).collect();
        assert_eq!(
            found,
            [(vec!["1", "e", "f"], false), (vec!["a", "b"], false)],
            "a guess takes in its recording's sure group; another length, artist or two recordings aren't one"
        );
    }

    #[test]
    fn merge_fills_what_the_kept_lacks() {
        let mut starred = song("A.mp3", "r1", Meta::default());
        starred.tags = vec![STARRED.into(), "t1".into()];
        starred.edits.insert(Field::Title, "Fixed".into());
        starred.edits.insert(Field::Genre, "Rock".into());
        starred.position = Some(12.0);
        starred.online = Some(Online { release: Some("rel".into()), ..Online::default() });
        let mut kept = song("A.flac", "r1", Meta { lossless: true, ..Meta::default() });
        kept.tags = vec!["t1".into()];
        kept.edits.insert(Field::Genre, "Metal".into());
        let store = store("merge", vec![("a", kept), ("b", starred)]);
        let (keep, other) = (key_of("a"), key_of("b"));
        let merged = merge(&store, &keep, &[other]);
        assert_eq!(merged.tags, Some(vec![STARRED.to_string(), "t1".into()]));
        assert_eq!(merged.edits, [(keep.clone(), Field::Title, Some("Fixed".into()))], "its own genre stays");
        assert_eq!(merged.online.and_then(|o| o.release), Some("rel".into()));
        assert_eq!(merged.position, Some(12.0));
    }
}
