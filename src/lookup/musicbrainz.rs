//! Asking MusicBrainz for an album's releases: a search by its name and
//! artist, then the track lists of the few that might be it. Requests are
//! paced to one a second, as MusicBrainz asks.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use musicbrainz_rs::MusicBrainzClient;
use musicbrainz_rs::entity::artist_credit::ArtistCredit;
use musicbrainz_rs::entity::release::{Release as Entity, ReleaseStatus};
use musicbrainz_rs::prelude::*;

use super::matching::{self, Album, Release, Track};

/// How many search hits are looked at.
const HITS: u8 = 10;
/// How many releases' track lists are fetched at most, for one album.
const FETCHED: usize = 3;
/// The least time between two requests.
const PACE: Duration = Duration::from_millis(1100);

pub struct MusicBrainz {
    client: MusicBrainzClient,
    last: Option<Instant>,
}

impl MusicBrainz {
    pub fn new() -> Self {
        let agent = format!("synththing/{} ( https://github.com/Dissssy/synththing )", crate::build_info::VERSION);
        Self { client: MusicBrainzClient::new(&agent), last: None }
    }

    fn wait_turn(&mut self) {
        if let Some(last) = self.last {
            std::thread::sleep(PACE.saturating_sub(last.elapsed()));
        }
        self.last = Some(Instant::now());
    }

    /// The releases that might be `album`, with their tracks: the search's
    /// hits that are named like it, those most like it first.
    pub fn releases(&mut self, album: &Album) -> Result<Vec<Release>> {
        let mut query = format!("release:{}", quoted(matching::search_name(&album.name)));
        if let Some(artist) = &album.artist {
            query += &format!(" AND artist:{}", quoted(artist));
        }
        self.wait_turn();
        let hits = Entity::search(query).limit(HITS).execute_with_client(&self.client).context("searching MusicBrainz")?.entities;
        let mut hits: Vec<Release> = hits.iter().map(release).filter(|r| matching::similar(&album.name, &r.title) >= 0.85).collect();
        // The ones with as many tracks as there are songs or more first.
        let songs = album.songs.len();
        hits.sort_by_key(|r| r.tracks.len() < songs);
        let mut fetched = Vec::new();
        for hit in hits.into_iter().filter(|r| !album.rejected.contains(&r.id)).take(FETCHED) {
            self.wait_turn();
            let full = Entity::fetch()
                .id(&hit.id)
                .with_recordings()
                .with_artist_credits()
                .with_release_groups()
                .execute_with_client(&self.client)
                .with_context(|| format!("fetching release {}", hit.id))?;
            let full = release(&full);
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

/// A credit as it reads: each name, then its join phrase.
fn credit(credits: &[ArtistCredit]) -> String {
    credits.iter().map(|c| format!("{}{}", c.name, c.joinphrase.as_deref().unwrap_or(""))).collect()
}

/// A release as matching sees it (its tracks only if they were asked
/// for; a search hit's are left out, but counted as untitled ones).
fn release(entity: &Entity) -> Release {
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
            })),
            None => tracks.extend((1..=medium.track_count).map(|position| Track { disc, position, ..Track::default() })),
        }
    }
    Release {
        id: entity.id.clone(),
        group: entity.release_group.as_ref().map(|g| g.id.clone()),
        title: entity.title.clone(),
        artist: entity.artist_credit.as_deref().map(credit).unwrap_or_default(),
        official: entity.status == Some(ReleaseStatus::Official),
        date: entity.date.as_ref().map(|d| d.0.clone()).filter(|d| !d.is_empty()),
        front: entity.cover_art_archive.as_ref().is_some_and(|c| c.front),
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_escape_quotes() {
        assert_eq!(quoted(r#"The "Best" \ Of"#), r#""The \"Best\" \\ Of""#);
    }
}
