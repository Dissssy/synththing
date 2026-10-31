//! Filling in missing art, a background job (`background.rs`) the user
//! turns on beside online lookups. An album with no art of its own (none
//! chosen, none in its files, none at the Cover Art Archive) is shown with
//! the iTunes Store's cover of the album named just like it, by the artist
//! named just like its. An artist tied to MusicBrainz (`lookup::artists`)
//! with no picture is shown with their picture at Wikidata. Each is kept
//! as a picture (`images.rs`), so it syncs like one the user picked.
//! It's worked out and asked for on a thread of its own, one album or
//! artist at a time. What's been tried is noted (`art_fill.json`, on this
//! device): one that was filled in is never filled in again (the user may
//! have changed it since), and one that wasn't is tried again a month
//! later (an hour, if asking failed).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::fields;
use crate::lookup::artists::Tied;
use crate::lookup::matching::{self, Album};
use crate::lookup::musicbrainz::MusicBrainz;
use crate::lookup::{self, Kept, art, details};
use crate::song_store::{Art, SongStore};

const FILE: &str = "art_fill.json";

/// How long until one that wasn't filled in is tried again, in seconds.
const RETRY: i64 = 30 * 24 * 60 * 60;

/// How long until one that couldn't be asked for is tried again.
const RETRY_FAILED: i64 = 60 * 60;

/// Filling in the art of the songs here, into the song store (and so
/// synced), while `Config::fill_art` (and online lookups) are on.
pub struct Filling {
    done: Arc<dyn Fn() + Send + Sync>,
    filler: Option<Filler>,
    /// The songs revision the filler was last given, while it's working.
    asking: Option<u64>,
    /// The songs revision the filler found nothing left to do at.
    idle_at: Option<u64>,
}

impl Filling {
    /// `done` is called after each album or artist's been tried (to `tick`
    /// soon).
    pub fn new(done: impl Fn() + Send + Sync + 'static) -> Self {
        Self { done: Arc::new(done), filler: None, asking: None, idle_at: None }
    }

    /// Save what's been filled in to `songs`, and ask for the next once
    /// it's in. Whether anything was filled in.
    pub fn tick(&mut self, songs: &mut SongStore, config: &Config) -> bool {
        let mut filled = false;
        while let Some(found) = self.filler.as_ref().and_then(|f| f.answers.try_recv().ok()) {
            let asked = self.asking.take();
            match found {
                Found::Album { keys, image } => {
                    // (Unless art's been chosen for it since.)
                    if keys.iter().all(|key| songs.get_by_key(key).is_none_or(|s| s.online.as_ref().is_none_or(|o| o.art.is_none()))) {
                        songs.set_art(&keys, Some(Art::Image(image)));
                        filled = true;
                    }
                }
                Found::Artist { name, image } => {
                    if !songs.artist_images().contains_key(&name) {
                        songs.set_artist_image(&name, Some(image));
                        filled = true;
                    }
                }
                Found::Tried => {}
                Found::Nothing => self.idle_at = asked,
            }
        }
        let on = config.fill_art && config.online_lookup && crate::build_info::online(crate::build_info::Online::Lookup);
        if !on || self.asking.is_some() || self.idle_at == Some(songs.revision()) {
            return filled;
        }
        if self.filler.is_none() {
            let done = Arc::clone(&self.done);
            match Filler::start(move || done()) {
                Ok(filler) => self.filler = Some(filler),
                Err(e) => {
                    log::warn!("couldn't start filling in missing art: {e}");
                    return filled;
                }
            }
        }
        if let Some(filler) = &self.filler
            && filler.plans.send(Plan { store: songs.clone(), ignore: config.group_ignore.clone() }).is_ok()
        {
            self.asking = Some(songs.revision());
        }
        filled
    }
}

/// What the filler works out its next ask from: a copy of the songs.
struct Plan {
    store: SongStore,
    ignore: String,
}

/// What it came to.
enum Found {
    /// The picture (`images.rs` key) to show the album of the songs
    /// `keys` with.
    Album { keys: Vec<String>, image: String },
    /// The picture to show the artist `name` (a `fields::group_key`) with.
    Artist { name: String, image: String },
    /// Something was tried, and nothing came of it.
    Tried,
    /// There's nothing left to try, with the songs as they are.
    Nothing,
}

/// The filler thread: given a copy of the songs, it tries the next album
/// or artist.
struct Filler {
    plans: mpsc::Sender<Plan>,
    answers: mpsc::Receiver<Found>,
}

impl Filler {
    fn start(done: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (plans, planned) = mpsc::channel::<Plan>();
        let (answer, answers) = mpsc::channel();
        thread::Builder::new().name("art fill".into()).spawn(move || {
            let mut work = Work { musicbrainz: MusicBrainz::new(), kept: Kept::default(), tried: Tried::load() };
            for plan in planned {
                let found = work.next(&plan);
                if answer.send(found).is_err() {
                    break;
                }
                done();
            }
        })?;
        Ok(Self { plans, answers })
    }
}

/// When each album (as "album:{id}") and artist (as "artist:{name}") is
/// to be tried again, in Unix seconds.
#[derive(Default)]
struct Tried {
    path: Option<PathBuf>,
    due: BTreeMap<String, i64>,
}

impl Tried {
    fn load() -> Self {
        let path = crate::config::config_dir().ok().map(|d| d.join(FILE));
        let due = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { path, due }
    }

    fn due(&self, id: &str, now: i64) -> bool {
        self.due.get(id).is_none_or(|&at| at <= now)
    }

    /// Note what came of trying `id`; what was filled in, if anything.
    fn note<T>(&mut self, id: String, tried: Result<Option<T>>, now: i64) -> Option<T> {
        let (at, found) = match tried {
            Ok(Some(found)) => (i64::MAX, Some(found)),
            Ok(None) => (now + RETRY, None),
            Err(e) => {
                log::info!("couldn't fill in the art of {id}: {e:#}");
                (now + RETRY_FAILED, None)
            }
        };
        self.due.insert(id, at);
        if let Some(path) = &self.path
            && let Err(e) = serde_json::to_string(&self.due).map_err(std::io::Error::other).and_then(|t| std::fs::write(path, t))
        {
            log::warn!("couldn't keep {}: {e}", path.display());
        }
        found
    }
}

struct Work {
    musicbrainz: MusicBrainz,
    kept: Kept,
    tried: Tried,
}

impl Work {
    /// Try the next album without art, else the next release to tie
    /// artists by, else the next artist without a picture.
    fn next(&mut self, plan: &Plan) -> Found {
        let (store, ignore) = (&plan.store, plan.ignore.as_str());
        let now = crate::settings_sync::now();
        for album in lookup::albums(store, ignore) {
            let id = format!("album:{}", album.id);
            if album.loose || album.artist.is_none() || !self.tried.due(&id, now) {
                continue;
            }
            let keys: Vec<String> = album.songs.iter().map(|s| s.key.clone()).collect();
            // (Its files are needed, to see they've no art; and art chosen
            // for it is never replaced.)
            let chosen = keys.iter().any(|key| store.get_by_key(key).is_some_and(|s| s.online.as_ref().is_some_and(|o| o.art.is_some())));
            if chosen || !keys.iter().all(|key| store.has(key)) {
                continue;
            }
            let found = album_picture(store, &album, &keys, ignore);
            return match self.tried.note(id, found, now) {
                Some(image) => Found::Album { keys, image },
                None => Found::Tried,
            };
        }
        let matched = lookup::matched(store, |_| true);
        let releases: BTreeSet<&str> = matched.iter().map(|(_, release, _)| release.as_str()).collect();
        for release in releases {
            let id = format!("release:{release}");
            if self.kept.release(release).is_some() || !self.tried.due(&id, now) {
                continue;
            }
            match self.musicbrainz.kept_or_fetched_release(release) {
                Ok(release) => self.kept.keep_release(release),
                Err(e) => _ = self.tried.note::<()>(id, Err(e), now),
            }
            return Found::Tried;
        }
        let matched: Vec<_> = matched
            .iter()
            .filter_map(|(key, release, recording)| Some((store.get_by_key(key)?, self.kept.release(release)?, recording.as_deref())))
            .collect();
        let tied = Tied::of(matched.iter().map(|(song, release, recording)| (*song, &**release, details::track(release, *recording))), ignore);
        for (name, artist) in &tied.names {
            let id = format!("artist:{name}");
            if store.artist_images().contains_key(name) || !self.tried.due(&id, now) {
                continue;
            }
            let found = self.artist_picture(artist);
            return match self.tried.note(id, found, now) {
                Some(image) => Found::Artist { name: name.clone(), image },
                None => Found::Tried,
            };
        }
        Found::Nothing
    }

    /// The picture of the artist `id` (at MusicBrainz), from Wikidata.
    fn artist_picture(&mut self, id: &str) -> Result<Option<String>> {
        let Some(item) = self.musicbrainz.wikidata(id)? else { return Ok(None) };
        let Some(url) = art::wikidata_picture(&item)? else { return Ok(None) };
        keep(&url)
    }
}

/// The picture `album` (of the songs `keys`) is to be shown with, if it
/// has no art of its own: the iTunes Store's cover of it.
fn album_picture(store: &SongStore, album: &Album, keys: &[String], ignore: &str) -> Result<Option<String>> {
    if keys.iter().any(|key| crate::album_art::embedded_picture(&store.path(key)).is_some()) {
        return Ok(None);
    }
    if let Some((release, group)) = keys.iter().find_map(|key| lookup::release_of(store.get_by_key(key)?))
        && !art::cover_art_archive(release, group)?.is_empty()
    {
        return Ok(None);
    }
    let Some(artist) = &album.artist else { return Ok(None) };
    let found = art::itunes(&format!("{} {artist}", matching::search_name(&album.name)))?;
    let Some(listed) = found.iter().find(|l| same_album(l, &album.name, artist, ignore)) else { return Ok(None) };
    keep(&listed.url)
}

/// Whether the iTunes Store's `listed` is the album `name` by `artist`.
fn same_album(listed: &art::Listed, name: &str, artist: &str, ignore: &str) -> bool {
    // (Its singles and EPs are named so.)
    let listed_name = [" - Single", " - EP"].iter().fold(listed.name.as_str(), |n, suffix| n.strip_suffix(suffix).unwrap_or(n));
    fields::group_key(listed_name, ignore) == fields::group_key(name, ignore) && fields::group_key(&listed.artist, ignore) == fields::group_key(artist, ignore)
}

/// The picture at `url`, kept (`images.rs`): its key.
fn keep(url: &str) -> Result<Option<String>> {
    let Some(bytes) = crate::album_art::download(url).with_context(|| format!("downloading {url}"))? else { return Ok(None) };
    crate::images::add(&bytes).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_album_named_just_so_is_the_same() {
        let listed = |name: &str, artist: &str| art::Listed { name: name.into(), artist: artist.into(), url: String::new() };
        assert!(same_album(&listed("Moonmadness", "Camel"), "moonmadness", "CAMEL", ""));
        assert!(same_album(&listed("Breathe - Single", "Camel"), "Breathe", "Camel", ""));
        assert!(!same_album(&listed("Moonmadness (Deluxe)", "Camel"), "Moonmadness", "Camel", ""));
        assert!(!same_album(&listed("Moonmadness", "Camel Tribute Band"), "Moonmadness", "Camel", ""));
    }

    #[test]
    fn what_was_filled_in_is_never_tried_again() {
        let mut tried = Tried::default();
        assert_eq!(tried.note("album:a".into(), Ok(Some(1)), 100), Some(1));
        assert!(!tried.due("album:a", i64::MAX - 1));
        assert_eq!(tried.note::<()>("album:b".into(), Ok(None), 100), None);
        assert!(!tried.due("album:b", 100 + RETRY - 1) && tried.due("album:b", 100 + RETRY));
        tried.note::<()>("album:c".into(), Err(anyhow::anyhow!("offline")), 100);
        assert!(tried.due("album:c", 100 + RETRY_FAILED));
    }
}
