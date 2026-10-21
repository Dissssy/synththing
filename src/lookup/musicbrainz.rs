//! Asking MusicBrainz for an album's releases: a search by its name and
//! artist, then the track lists of the few that might be it. Requests are
//! paced to one a second, as MusicBrainz asks. What it answers is kept as
//! it came (`Cache`, in the config folder's `musicbrainz`) for six months,
//! so looking an album up again, or its details later, needn't ask again.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use musicbrainz_rs::MusicBrainzClient;
use musicbrainz_rs::entity::artist::Artist as ArtistEntity;
use musicbrainz_rs::entity::artist_credit::ArtistCredit;
use musicbrainz_rs::entity::genre::Genre;
use musicbrainz_rs::entity::recording::Recording;
use musicbrainz_rs::entity::release::{Release as Entity, ReleaseStatus};
use musicbrainz_rs::entity::release_group::{ReleaseGroupPrimaryType, ReleaseGroupSecondaryType};
use musicbrainz_rs::entity::search::SearchResult;
use musicbrainz_rs::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Digest;

use super::Outcome;
use super::artists::Artist;
use super::matching::{self, Album, Credit, Hit, HitRelease, Release, Track};

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
        self.get_any(kind, name)
    }

    /// What's kept under `name`, however old.
    fn get_any<T: DeserializeOwned>(&self, kind: &str, name: &str) -> Option<T> {
        serde_json::from_slice(&std::fs::read(self.path(kind, name)?).ok()?).ok()
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

/// The release `id` as it was kept, if it was (however long ago: it's
/// asked for again when its match is refreshed, `lookup::REFRESH`).
pub fn kept_release(id: &str) -> Option<Release> {
    cache().get_any::<Entity>("release", id).map(|e| converted(&e))
}

/// The artist `id` as they were kept, if they were.
pub fn kept_artist(id: &str) -> Option<Artist> {
    cache().get_any::<ArtistEntity>("artist", id).map(|a| artist(&a))
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

    /// The hits of a search for recordings by `query`: kept, unless
    /// `fresh`.
    fn search_recordings(&mut self, query: String, fresh: bool) -> Result<Vec<Recording>> {
        let name = crate::library::hex(&sha2::Sha256::digest(query.as_bytes())[..16]);
        if !fresh && let Some(hits) = self.cache.get::<Vec<Recording>>("recording-search", &name) {
            return Ok(hits);
        }
        self.wait_turn();
        let found: SearchResult<Recording> = Recording::search(query).limit(HITS).execute_with_client(&self.client).context("searching MusicBrainz")?;
        self.cache.put("recording-search", &name, &found.entities);
        Ok(found.entities)
    }

    /// What a song on no album (`album`'s one song) is
    /// (`matching::pick_recording`): a track of the release it's surely
    /// on, or, if it's only on compilations, those it might be put on.
    pub fn recording(&mut self, album: &Album, fresh: bool) -> Result<Outcome> {
        let song = &album.songs[0];
        let Some(artist) = &song.artist else { return Ok(Outcome::NotFound) };
        let query = format!("recording:{} AND artist:{}", quoted(&song.title), quoted(matching::first_name(artist)));
        let hits: Vec<Hit> = self
            .search_recordings(query, fresh)?
            .iter()
            .map(hit)
            .map(|mut h| {
                h.releases.retain(|r| !album.rejected.contains(&r.id));
                h
            })
            .collect();
        let picked = matching::pick_recording(song, &hits);
        match picked.first() {
            None => Ok(Outcome::NotFound),
            Some((i, first)) if first.kind != matching::COMPILATION => {
                let release = converted(&self.release(&first.id, fresh)?);
                Ok(match release.tracks.iter().position(|t| t.recording.as_deref() == Some(hits[*i].recording.as_str())) {
                    Some(track) => Outcome::Matched(Box::new(release), vec![Some(track)]),
                    None => Outcome::NotFound,
                })
            }
            Some(_) => Ok(Outcome::Unsure(picked.iter().take(FETCHED).map(|(_, r)| self.release(&r.id, fresh).map(|e| converted(&e))).collect::<Result<_>>()?)),
        }
    }

    /// The release `id` as MusicBrainz has it now (and kept).
    pub fn fresh_release(&mut self, id: &str) -> Result<Release> {
        Ok(converted(&self.release(id, true)?))
    }

    /// The artist `id`, with their aliases (and kept).
    pub fn artist(&mut self, id: &str) -> Result<Artist> {
        self.wait_turn();
        let entity = ArtistEntity::fetch().id(id).with_aliases().execute_with_client(&self.client).with_context(|| format!("fetching artist {id}"))?;
        self.cache.put("artist", id, &entity);
        Ok(artist(&entity))
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
pub(super) const VARIOUS_ARTISTS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

fn artist(entity: &ArtistEntity) -> Artist {
    Artist { id: entity.id.clone(), name: entity.name.clone(), aliases: entity.aliases.iter().flatten().map(|a| a.name.clone()).collect() }
}

/// A credit as it reads: each name, then its join phrase.
fn credit(credits: &[ArtistCredit]) -> String {
    credits.iter().map(|c| format!("{}{}", c.name, c.joinphrase.as_deref().unwrap_or(""))).collect()
}

/// Each artist credited.
fn credits(credits: &[ArtistCredit]) -> Vec<Credit> {
    credits
        .iter()
        .map(|c| Credit { id: c.artist.id.clone(), credited: c.name.clone(), name: c.artist.name.clone(), sort: c.artist.sort_name.clone() })
        .collect()
}

/// A recording a search found, as matching sees it.
fn hit(recording: &Recording) -> Hit {
    let releases = recording
        .releases
        .iter()
        .flatten()
        .map(|r| {
            let group = r.release_group.as_ref();
            let compilation = group.is_some_and(|g| g.secondary_types.contains(&ReleaseGroupSecondaryType::Compilation))
                || r.artist_credit.iter().flatten().any(|c| c.artist.id == VARIOUS_ARTISTS);
            let kind = match group.and_then(|g| g.primary_type.as_ref()) {
                _ if compilation => 4,
                Some(ReleaseGroupPrimaryType::Album) => 0,
                Some(ReleaseGroupPrimaryType::Single) => 1,
                Some(ReleaseGroupPrimaryType::Ep) => 2,
                _ => 3,
            };
            HitRelease {
                id: r.id.clone(),
                official: r.status == Some(ReleaseStatus::Official),
                kind,
                date: r.date.as_ref().map(|d| d.0.clone()).filter(|d| !d.is_empty()),
            }
        })
        .collect();
    Hit {
        recording: recording.id.clone(),
        title: recording.title.clone(),
        artists: recording.artist_credit.iter().flatten().map(|c| c.name.clone()).collect(),
        length: recording.length.map(|ms| f64::from(ms) / 1000.0),
        releases,
    }
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
    fn recording_hits_read_as_they_come() {
        let json = r#"{"id":"c1","title":"Feather","length":175000,"video":false,
            "artist-credit":[{"name":"Nujabes","joinphrase":" feat. ","artist":{"id":"a1","name":"Nujabes","sort-name":"Nujabes"}},
                {"name":"Cise Starr","artist":{"id":"a2","name":"Cise Starr","sort-name":"Starr, Cise"}}],
            "releases":[{"id":"r1","title":"Modal Soul","status":"Official","date":"2005-11-11",
                "release-group":{"id":"g1","title":"Modal Soul","primary-type":"Album","secondary-types":[]},
                "media":[{"id":"m1","position":1,"track-count":14,"track":[{"id":"t1","number":"2","title":"Feather","length":175000}]}]},
                {"id":"r2","title":"Hydeout Productions","status":"Official",
                "release-group":{"id":"g2","title":"Hydeout Productions","primary-type":"Album","secondary-types":["Compilation"]}}]}"#;
        let recording: Recording = serde_json::from_str(json).expect("a recording");
        let hit = hit(&recording);
        assert_eq!((hit.artists.as_slice(), hit.length), (["Nujabes".to_string(), "Cise Starr".to_string()].as_slice(), Some(175.0)));
        assert_eq!(hit.releases.iter().map(|r| (r.id.as_str(), r.kind, r.official)).collect::<Vec<_>>(), [("r1", 0, true), ("r2", 4, true)]);
    }

    #[test]
    fn phrases_escape_quotes() {
        assert_eq!(quoted(r#"The "Best" \ Of"#), r#""The \"Best\" \\ Of""#);
    }
}
