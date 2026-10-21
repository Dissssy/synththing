//! Asking MusicBrainz for an album's releases: a search by its name and
//! artist, then the track lists of the few that might be it. Requests are
//! paced to one a second, as MusicBrainz asks. What it answers is kept as
//! it came (`Cache`, in the config folder's `musicbrainz`) for six months,
//! so looking an album up again, or its details later, needn't ask again.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use musicbrainz_rs::MusicBrainzClient;
use musicbrainz_rs::entity::artist_credit::ArtistCredit;
use musicbrainz_rs::entity::genre::Genre;
use musicbrainz_rs::entity::release::{Release as Entity, ReleaseStatus};
use musicbrainz_rs::entity::release_group::ReleaseGroupSecondaryType;
use musicbrainz_rs::entity::search::SearchResult;
use musicbrainz_rs::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Digest;

use super::matching::{self, Album, Credit, Release, Track};

/// How many search hits are looked at.
const HITS: u8 = 10;
/// How many releases' track lists are fetched at most, for one album.
const FETCHED: usize = 4;
/// The least time between two requests.
const PACE: Duration = Duration::from_millis(1100);
/// How long an answer is kept before it's asked for again.
const KEPT: Duration = Duration::from_secs(182 * 24 * 60 * 60);

/// MusicBrainz's answers, each in a file of its own: releases by id,
/// searches by a hash of the query.
struct Cache {
    dir: Option<PathBuf>,
}

impl Cache {
    fn path(&self, kind: &str, name: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(kind).join(format!("{name}.json")))
    }

    /// What's kept under `name`, if it's younger than `KEPT`.
    fn get<T: DeserializeOwned>(&self, kind: &str, name: &str) -> Option<T> {
        let path = self.path(kind, name)?;
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
        if SystemTime::now().duration_since(modified).unwrap_or_default() > KEPT {
            return None;
        }
        serde_json::from_slice(&std::fs::read(&path).ok()?).ok()
    }

    fn put<T: Serialize>(&self, kind: &str, name: &str, value: &T) {
        let Some(path) = self.path(kind, name) else { return };
        let written = serde_json::to_vec(value)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::create_dir_all(path.parent().expect("in a folder")).and_then(|()| std::fs::write(&path, bytes)));
        if let Err(e) = written {
            log::warn!("couldn't keep {}: {e}", path.display());
        }
    }
}

pub struct MusicBrainz {
    client: MusicBrainzClient,
    last: Option<Instant>,
    cache: Cache,
}

fn cache() -> Cache {
    Cache { dir: crate::config::config_dir().ok().map(|d| d.join("musicbrainz")) }
}

/// The release `id` as it was kept, if it was.
pub fn kept_release(id: &str) -> Option<Release> {
    cache().get::<Entity>("release", id).map(|e| converted(&e))
}

impl MusicBrainz {
    pub fn new() -> Self {
        let agent = format!("synththing/{} ( https://github.com/Dissssy/synththing )", crate::build_info::VERSION);
        Self { client: MusicBrainzClient::new(&agent), last: None, cache: cache() }
    }

    fn wait_turn(&mut self) {
        if let Some(last) = self.last {
            std::thread::sleep(PACE.saturating_sub(last.elapsed()));
        }
        self.last = Some(Instant::now());
    }

    /// The hits of a search for `query`: kept, unless `fresh`.
    fn search(&mut self, query: String, fresh: bool) -> Result<Vec<Entity>> {
        let name = crate::library::hex(&sha2::Sha256::digest(query.as_bytes())[..16]);
        if !fresh && let Some(hits) = self.cache.get::<Vec<Entity>>("search", &name) {
            return Ok(hits);
        }
        self.wait_turn();
        let found: SearchResult<Entity> = Entity::search(query).limit(HITS).execute_with_client(&self.client).context("searching MusicBrainz")?;
        self.cache.put("search", &name, &found.entities);
        Ok(found.entities)
    }

    /// The release `id`, with its tracks and genres: kept, unless `fresh`.
    fn release(&mut self, id: &str, fresh: bool) -> Result<Entity> {
        if !fresh && let Some(release) = self.cache.get::<Entity>("release", id) {
            return Ok(release);
        }
        self.wait_turn();
        let release = Entity::fetch()
            .id(id)
            .with_recordings()
            .with_artist_credits()
            .with_release_groups()
            .with_genres()
            .execute_with_client(&self.client)
            .with_context(|| format!("fetching release {id}"))?;
        self.cache.put("release", id, &release);
        Ok(release)
    }

    /// The releases that might be `album`, with their tracks: the search's
    /// hits that are named like it, those most like it first. Asked for
    /// again if `fresh`, else from what's kept where it can be.
    pub fn releases(&mut self, album: &Album, fresh: bool) -> Result<Vec<Release>> {
        let name = format!("release:{}", quoted(&matching::search_name(&album.name)));
        let named_like =
            |hits: Vec<Entity>| -> Vec<Release> { hits.iter().map(converted).filter(|r| matching::similar_album(&album.name, &r.title) >= 0.85).collect() };
        let mut hits = Vec::new();
        if let Some(artist) = &album.artist {
            hits = named_like(self.search(format!("{name} AND artist:{}", quoted(matching::first_name(artist))), fresh)?);
        }
        // (Credited otherwise there, or a compilation: by its name alone.)
        if hits.is_empty() {
            hits = named_like(self.search(name, fresh)?);
        }
        // Named a little otherwise ("Music Records - Masta" for "Music
        // Record: Masta"): by its words.
        if hits.is_empty() {
            let words: Vec<String> =
                matching::search_name(&album.name).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
            if words.len() > 1 {
                hits = named_like(self.search(format!("release:({})", words.join(" ")), fresh)?);
            }
        }
        // The ones with as many tracks as it has songs or more first.
        let songs = album.distinct_songs();
        hits.sort_by_key(|r| r.tracks.len() < songs);
        let mut fetched = Vec::new();
        for hit in hits.into_iter().filter(|r| !album.rejected.contains(&r.id)).take(FETCHED) {
            let full = self.release(&hit.id, fresh)?;
            let full = converted(&full);
            let fits = matching::fit(album, &full).complete();
            fetched.push(full);
            // One that fits every song: editions of other albums are only
            // worth a look if they'd fit as well.
            if fits && fetched.len() >= 2 {
                break;
            }
        }
        Ok(fetched)
    }
}

/// `text` as a Lucene phrase.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// MusicBrainz's Various Artists.
const VARIOUS_ARTISTS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// A credit as it reads: each name, then its join phrase.
fn credit(credits: &[ArtistCredit]) -> String {
    credits.iter().map(|c| format!("{}{}", c.name, c.joinphrase.as_deref().unwrap_or(""))).collect()
}

/// Each artist credited, by name.
fn credits(credits: &[ArtistCredit]) -> Vec<Credit> {
    credits.iter().map(|c| Credit { name: c.name.clone(), sort: c.artist.sort_name.clone() }).collect()
}

/// A release as matching sees it (its tracks only if they were asked
/// for; a search hit's are left out, but counted as untitled ones).
fn converted(entity: &Entity) -> Release {
    let mut tracks = Vec::new();
    for (i, medium) in entity.media.iter().flatten().enumerate() {
        let disc = medium.position.unwrap_or(i as u32 + 1);
        match &medium.tracks {
            Some(list) => tracks.extend(list.iter().map(|t| Track {
                disc,
                position: t.position,
                title: t.title.clone(),
                length: t.length.map(|ms| f64::from(ms) / 1000.0),
                recording: t.recording.as_ref().map(|r| r.id.clone()),
                credits: t.artist_credit.as_deref().map(credits).unwrap_or_default(),
            })),
            None => tracks.extend((1..=medium.track_count).map(|position| Track { disc, position, ..Track::default() })),
        }
    }
    let group = entity.release_group.as_ref();
    let mut genres: Vec<&Genre> = entity.genres.iter().flatten().collect();
    if genres.is_empty() {
        genres = group.and_then(|g| g.genres.as_ref()).into_iter().flatten().collect();
    }
    genres.sort_by_key(|g| std::cmp::Reverse(g.count.unwrap_or(0)));
    let various = entity.artist_credit.iter().flatten().any(|c| c.artist.id == VARIOUS_ARTISTS);
    Release {
        id: entity.id.clone(),
        group: entity.release_group.as_ref().map(|g| g.id.clone()),
        title: entity.title.clone(),
        artist: entity.artist_credit.as_deref().map(credit).unwrap_or_default(),
        official: entity.status == Some(ReleaseStatus::Official),
        date: entity.date.as_ref().map(|d| d.0.clone()).filter(|d| !d.is_empty()),
        front: entity.cover_art_archive.as_ref().is_some_and(|c| c.front),
        discs: entity.media.as_ref().map_or(0, |m| m.len() as u32),
        tracks,
        credits: entity.artist_credit.as_deref().map(credits).unwrap_or_default(),
        first_released: group.and_then(|g| g.first_release_date.as_ref()).map(|d| d.0.clone()).filter(|d| !d.is_empty()),
        genres: genres.into_iter().map(|g| g.name.clone()).collect(),
        compilation: various || group.is_some_and(|g| g.secondary_types.contains(&ReleaseGroupSecondaryType::Compilation)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_releases_read_back_as_they_came() {
        let json = r#"{"id":"r1","title":"Modal Soul","status":"Official","date":"2005-11-11",
            "artist-credit":[{"name":"Nujabes","joinphrase":"","artist":{"id":"a1","name":"Nujabes","sort-name":"Nujabes"}}],
            "release-group":{"id":"g1","title":"Modal Soul"},
            "cover-art-archive":{"artwork":true,"back":false,"count":1,"darkened":false,"front":true},
            "media":[{"id":"m1","position":1,"track-count":1,"tracks":[{"id":"t1","title":"Feather","number":"1","length":175000,"position":1,
                "recording":{"id":"c1","title":"Feather"}}]}]}"#;
        let entity: Entity = serde_json::from_str(json).expect("a release");
        let dir = std::env::temp_dir().join(format!("synththing-mb-cache-{}", std::process::id()));
        let cache = Cache { dir: Some(dir.clone()) };
        cache.put("release", "r1", &entity);
        let kept: Entity = cache.get("release", "r1").expect("kept");
        std::fs::remove_dir_all(&dir).ok();
        let release = converted(&kept);
        assert_eq!((release.group.as_deref(), release.artist.as_str(), release.official, release.front), (Some("g1"), "Nujabes", true, true));
        assert_eq!(release.tracks[0].recording.as_deref(), Some("c1"));
        assert_eq!(release.tracks[0].length, Some(175.0));
    }

    #[test]
    fn phrases_escape_quotes() {
        assert_eq!(quoted(r#"The "Best" \ Of"#), r#""The \"Best\" \\ Of""#);
    }
}
