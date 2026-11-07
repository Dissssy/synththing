//! Lyrics from LRCLIB (lrclib.net: free, no key), for songs with none of
//! their own. The device that looks songs up finds each song's record by
//! its title, artist, album and length (`find`); the record's id is kept
//! on the song (`song_store::Online::lyrics`) and so synced, and every
//! device reads the record by that id (`record`), keeping it in
//! `<config>/lrclib/` so it's only asked for once.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::art::get;

const API: &str = "https://lrclib.net/api";
/// How far off a record's length can be from the song's, in seconds.
const LENGTH_LEEWAY: f64 = 2.0;

/// A song's lyrics as LRCLIB has them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: i64,
    #[serde(default)]
    pub synced_lyrics: Option<String>,
    #[serde(default)]
    pub plain_lyrics: Option<String>,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(default)]
    pub duration: Option<f64>,
}

impl Record {
    /// Its lyrics: timed if it has them, else plain.
    pub fn text(&self) -> Option<&str> {
        self.synced_lyrics.as_deref().or(self.plain_lyrics.as_deref()).filter(|t| !t.trim().is_empty())
    }
}

/// What's looked up for a song.
#[derive(Clone, Debug)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Seconds.
    pub length: f64,
}

/// The record for `song`, if LRCLIB has one with lyrics (or that says
/// it's instrumental): the one of its exact title, artist, album and
/// length, else the closest of a search with a length that fits.
pub fn find(song: &Song) -> Result<Option<Record>> {
    let length = format!("{:.0}", song.length);
    let mut query = vec![("track_name", song.title.as_str()), ("artist_name", song.artist.as_str()), ("duration", length.as_str())];
    if let Some(album) = &song.album {
        query.push(("album_name", album));
    }
    if let Some(found) = get(&format!("{API}/get"), &query)?.and_then(parse).filter(useful) {
        keep(&found);
        return Ok(Some(found));
    }
    let found = get(&format!("{API}/search"), &[("track_name", &song.title), ("artist_name", &song.artist)])?;
    let records = found.and_then(|v| v.as_array().cloned()).unwrap_or_default().into_iter().filter_map(parse);
    let fits = |r: &Record| r.duration.is_some_and(|d| (d - song.length).abs() <= LENGTH_LEEWAY) && useful(r);
    // (Timed lyrics over plain.)
    let best = records.filter(fits).max_by_key(|r| (r.synced_lyrics.is_some(), -r.id));
    if let Some(record) = &best {
        keep(record);
    }
    Ok(best)
}

/// The record `id`: as kept, else asked for (when `online`).
pub fn record(id: i64, online: bool) -> Option<Record> {
    if let Some(kept) = kept(id) {
        return Some(kept);
    }
    if !online {
        return None;
    }
    match get(&format!("{API}/get/{id}"), &[]) {
        Ok(found) => {
            let record = found.and_then(parse)?;
            keep(&record);
            Some(record)
        }
        Err(e) => {
            log::warn!("couldn't get LRCLIB's lyrics {id}: {e:#}");
            None
        }
    }
}

fn parse(value: Value) -> Option<Record> {
    serde_json::from_value(value).ok()
}

fn useful(record: &Record) -> bool {
    record.instrumental || record.text().is_some()
}

fn dir() -> Option<PathBuf> {
    Some(crate::config::config_dir().ok()?.join("lrclib"))
}

fn kept(id: i64) -> Option<Record> {
    serde_json::from_slice(&std::fs::read(dir()?.join(format!("{id}.json"))).ok()?).ok()
}

fn keep(record: &Record) {
    let Some(dir) = dir() else { return };
    let saved = std::fs::create_dir_all(&dir)
        .map_err(anyhow::Error::from)
        .and_then(|()| Ok(std::fs::write(dir.join(format!("{}.json", record.id)), serde_json::to_vec(record)?)?));
    if let Err(e) = saved {
        log::warn!("couldn't keep LRCLIB's lyrics {}: {e:#}", record.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_records() {
        let value = serde_json::json!({
            "id": 3396226, "trackName": "I Want to Live", "artistName": "Borislav Slavov", "albumName": "Baldur's Gate 3",
            "duration": 233.0, "instrumental": false, "plainLyrics": "I feel your breath", "syncedLyrics": "[00:17.12] I feel your breath"
        });
        let record = parse(value).unwrap();
        assert_eq!((record.id, record.duration), (3396226, Some(233.0)));
        assert_eq!(record.text(), Some("[00:17.12] I feel your breath"));
        let instrumental = parse(serde_json::json!({"id": 1, "instrumental": true, "plainLyrics": null, "syncedLyrics": null})).unwrap();
        assert!(useful(&instrumental) && instrumental.text().is_none());
    }

    /// Asks lrclib.net (run by hand: `cargo test lrclib -- --ignored`).
    #[test]
    #[ignore]
    fn finds_a_song() {
        let song = Song {
            title: "I Want to Live".into(),
            artist: "Borislav Slavov".into(),
            album: Some("Baldur's Gate 3 (Original Game Soundtrack)".into()),
            length: 233.0,
        };
        let record = find(&song).unwrap().unwrap();
        assert!(record.text().is_some());
        assert_eq!(super::record(record.id, true).unwrap().id, record.id);
    }
}
