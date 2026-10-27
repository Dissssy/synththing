//! Looking the library's albums up at MusicBrainz (1.7), on a thread of
//! its own, one album at a time: an album whose songs all line up with
//! one release is matched (`song_store::Online`, synced), so its art can
//! come from the Cover Art Archive (`album_art`). One the lookup isn't
//! sure of, or doesn't find, is noted here (`Tried`, in `lookup.json`, on
//! this device only) and tried again later.
//! A song on no album is looked up by its recording instead
//! (`matching::pick_recording`), and a match's release is asked for again
//! every six months (`REFRESH`), for what's changed since. Each artist
//! matched songs credit is asked for once, for the names they go by
//! (`artists`).

pub mod art;
pub mod artists;
pub mod details;
pub mod matching;
pub mod musicbrainz;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Instant;

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::fields::{self, Facts, Field};
use crate::song_store::{self, SongStore};
use matching::{Album, Release, Verdict};

/// How long an album that wasn't found (or wasn't surely found) waits
/// to be tried again.
const RETRY: i64 = 90 * 24 * 60 * 60;
/// How long one whose lookup failed (offline, MusicBrainz busy) waits.
const RETRY_FAILED: i64 = 60 * 60;
/// How long a match goes before its release is asked for again.
pub const REFRESH: i64 = 182 * 24 * 60 * 60;

/// What a lookup made of an album.
#[derive(Debug)]
pub enum Outcome {
    /// This release, each song's track (by index).
    Matched(Box<Release>, Vec<Option<usize>>),
    /// These might be it.
    Unsure(Vec<Release>),
    NotFound,
    Failed(String),
}

/// What's known of an album, by its id, for the album: what its songs
/// say it's called and who by.
pub fn album_id(album: &str, album_artist: Option<&str>, ignore: &str) -> String {
    let mut id = fields::group_key(album, ignore);
    if let Some(artist) = album_artist {
        id.push('\u{1f}');
        id.push_str(&fields::group_key(artist, ignore));
    }
    id
}

/// The album `song` is on, if it's on one of its own (one its match
/// filled in isn't).
fn own_album<'s>(song: &song_store::Song, facts: &Facts<'s>, ignore: &str) -> Option<std::borrow::Cow<'s, str>> {
    let filled = song.online.as_ref().and_then(|o| o.filled.as_ref()).is_some_and(|f| f.contains(&Field::Album));
    facts.text(Field::Album).filter(|n| !filled && !fields::group_key(n, ignore).is_empty())
}

/// What the lookup knows the song `key` by: its album's id, or (on none)
/// its own.
pub fn album_of(key: &str, song: &song_store::Song, ignore: &str) -> String {
    let facts = Facts::of(song);
    match own_album(song, &facts, ignore) {
        Some(name) => album_id(&name, facts.text(Field::AlbumArtist).as_deref(), ignore),
        None => format!("song:{key}"),
    }
}

fn is_midi(name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"))
}

/// The library's albums (of songs here that aren't MIDI), the largest
/// first.
pub fn albums(store: &SongStore, ignore: &str) -> Vec<Album> {
    let mut albums: HashMap<String, Album> = HashMap::new();
    let mut artists: HashMap<String, Vec<String>> = HashMap::new();
    let mut loose = Vec::new();
    for (key, song) in store.known() {
        if !store.has(key) || is_midi(&song.name) || store.is_blocked(key) {
            continue;
        }
        let facts = Facts::of(song);
        let title = facts.text(Field::Title).map_or_else(|| song.name.clone(), |t| t.into_owned());
        let artist = facts.text(Field::Artist).map(|a| a.into_owned());
        let rejected = song.online.as_ref().map(|o| o.rejected.clone()).unwrap_or_default();
        let ours = matching::Song {
            key: key.to_string(),
            title,
            artist,
            track: facts.number(Field::Track).map(|n| n as u32),
            disc: facts.number(Field::Disc).map(|n| n as u32),
            length: facts.number(Field::Length),
        };
        let Some(name) = own_album(song, &facts, ignore) else {
            // (On no album: looked up by itself, if it says who it's by.)
            if ours.artist.is_some() && facts.text(Field::Title).is_some() {
                loose.push(Album {
                    id: format!("song:{key}"),
                    // (Its title, for the log.)
                    name: ours.title.clone(),
                    artist: ours.artist.clone(),
                    songs: vec![ours],
                    rejected,
                    loose: true,
                });
            }
            continue;
        };
        let album_artist = facts.text(Field::AlbumArtist);
        let id = album_id(&name, album_artist.as_deref(), ignore);
        if let Some(artist) = &ours.artist {
            artists.entry(id.clone()).or_default().push(artist.clone());
        }
        let album = albums.entry(id.clone()).or_insert_with(|| Album {
            id,
            name: name.into_owned(),
            artist: album_artist.map(|a| a.into_owned()),
            songs: Vec::new(),
            rejected: Vec::new(),
            loose: false,
        });
        for rejected in rejected {
            if !album.rejected.contains(&rejected) {
                album.rejected.push(rejected);
            }
        }
        album.songs.push(ours);
    }
    let mut albums: Vec<Album> = albums.into_values().collect();
    for album in &mut albums {
        // No album artist: its songs' one artist, if they have one.
        if album.artist.is_none()
            && let Some(names) = artists.get(&album.id)
            && names.len() == album.songs.len()
            && names.iter().all(|n| fields::group_key(n, ignore) == fields::group_key(&names[0], ignore))
        {
            album.artist = Some(names[0].clone());
        }
    }
    albums.sort_by(|a, b| b.songs.len().cmp(&a.songs.len()).then_with(|| a.id.cmp(&b.id)));
    // (Songs on no album last.)
    loose.sort_by(|a, b| a.id.cmp(&b.id));
    albums.extend(loose);
    albums
}

/// Whether the lookup has something to do for `album`: a song of it
/// isn't matched, and its file doesn't say its release either.
pub fn wanted(store: &SongStore, album: &Album) -> bool {
    album.songs.iter().any(|s| {
        let song = store.get_by_key(&s.key);
        let matched = song.and_then(|s| s.online.as_ref()).is_some_and(|o| o.release.is_some());
        let tagged = song.and_then(|s| s.meta.as_ref()).is_some_and(|m| m.musicbrainz_release.is_some());
        !matched && !tagged
    })
}

/// The release a song's art and details come from: its match, else the
/// one its file says.
pub fn release_of(song: &song_store::Song) -> Option<(&str, Option<&str>)> {
    match &song.online {
        Some(song_store::Online { release: Some(release), group, .. }) => Some((release, group.as_deref())),
        _ => song.meta.as_ref()?.musicbrainz_release.as_deref().map(|r| (r, None)),
    }
}

/// The matches `album`'s songs get from `release` (`by_user`: picked by
/// the user): each song's track. What's rejected and the art chosen stay.
pub fn matches(album: &Album, release: &Release, tracks: &[Option<usize>], by_user: bool, store: &SongStore) -> Vec<(String, Option<song_store::Online>)> {
    let at = crate::settings_sync::now();
    album
        .songs
        .iter()
        .zip(tracks)
        .map(|(song, track)| {
            let mut was = store.get_by_key(&song.key).and_then(|s| s.online.clone()).unwrap_or_default();
            // (Another release: its blanks are filled anew.)
            if was.release.as_ref() != Some(&release.id) {
                was.filled = None;
            }
            let online = song_store::Online {
                release: Some(release.id.clone()),
                group: release.group.clone(),
                recording: track.and_then(|t| release.tracks[t].recording.clone()),
                by_user,
                at,
                ..was
            };
            (song.key.clone(), Some(online))
        })
        .collect()
}

/// What the lookup kept of MusicBrainz's answers on this device:
/// releases (with their tracks) and artists (with their aliases), by id,
/// as they were when last asked for (`None`: never). Read from disk as
/// they're first wanted, and shared with work off the UI thread.
#[derive(Clone, Default)]
pub struct Kept(Arc<Mutex<KeptById>>);

#[derive(Default)]
struct KeptById {
    releases: HashMap<String, Option<Arc<Release>>>,
    artists: HashMap<String, Option<Arc<artists::Artist>>>,
}

impl Kept {
    fn lock(&self) -> MutexGuard<'_, KeptById> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn release(&self, id: &str) -> Option<Arc<Release>> {
        if let Some(kept) = self.lock().releases.get(id) {
            return kept.clone();
        }
        // (Read without holding the others up.)
        let read = musicbrainz::kept_release(id).map(Arc::new);
        self.lock().releases.entry(id.to_string()).or_insert(read).clone()
    }

    pub fn keep_release(&self, release: Release) {
        self.lock().releases.insert(release.id.clone(), Some(Arc::new(release)));
    }

    pub fn artist(&self, id: &str) -> Option<Arc<artists::Artist>> {
        if let Some(kept) = self.lock().artists.get(id) {
            return kept.clone();
        }
        let read = musicbrainz::kept_artist(id).map(Arc::new);
        self.lock().artists.entry(id.to_string()).or_insert(read).clone()
    }

    pub fn keep_artist(&self, artist: artists::Artist) {
        self.lock().artists.insert(artist.id.clone(), Some(Arc::new(artist)));
    }
}

/// An album the lookup tried and didn't match, when, and what it found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tried {
    pub at: i64,
    pub found: Answer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    /// These releases (by id) might be it.
    Unsure(Vec<String>),
    NotFound,
    Failed,
}

impl Tried {
    /// It's time to try again.
    pub fn due(&self, now: i64) -> bool {
        let wait = if self.found == Answer::Failed { RETRY_FAILED } else { RETRY };
        now - self.at >= wait
    }
}

/// The albums tried and not matched, by id (`lookup.json`).
#[derive(Default)]
pub struct State {
    path: Option<PathBuf>,
    pub tried: BTreeMap<String, Tried>,
}

impl State {
    pub fn load() -> Self {
        let path = crate::config::config_dir().ok().map(|d| d.join("lookup.json"));
        let tried = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, tried }
    }

    pub fn note(&mut self, album: &str, tried: Option<Tried>) {
        match tried {
            Some(tried) => self.tried.insert(album.to_string(), tried),
            None => self.tried.remove(album),
        };
        if let Some(path) = &self.path
            && let Err(e) = serde_json::to_vec(&self.tried).map_err(anyhow::Error::from).and_then(|b| Ok(std::fs::write(path, b)?))
        {
            log::warn!("couldn't save {}: {e}", path.display());
        }
    }
}

/// Note in the log how each release fit `album`, when none was taken:
/// what's needed to tell why.
fn log_candidates(album: &Album, releases: &[Release], verdict: Verdict) -> Verdict {
    if !matches!(verdict, Verdict::Match(..)) {
        let searched = matching::search_name(&album.name);
        log::info!("{:?} (searched as {searched:?}, {} songs): {} releases looked at", album.name, album.songs.len(), releases.len());
        for release in releases {
            let fit = matching::fit(album, release);
            let missing: Vec<&str> = album.songs.iter().zip(&fit.tracks).filter(|(_, t)| t.is_none()).map(|(s, _)| s.title.as_str()).take(5).collect();
            log::info!(
                "  {} {:?} by {:?} ({} tracks, {}): {}/{} songs fit, name {:.2}, score {:.2}{}",
                release.id,
                release.title,
                release.artist,
                release.tracks.len(),
                release.date.as_deref().unwrap_or("no date"),
                fit.tracks.iter().flatten().count(),
                fit.tracks.len(),
                matching::similar_album(&album.name, &release.title),
                fit.score,
                if missing.is_empty() { String::new() } else { format!(", missing {missing:?}") },
            );
        }
    }
    verdict
}

/// What the lookup thread works out its next ask from: a copy of the
/// songs, and what the UI thread knows.
pub struct Plan {
    pub store: SongStore,
    /// The albums tried and not matched (`State::tried`).
    pub tried: BTreeMap<String, Tried>,
    /// An album (by id) to look up first, asking MusicBrainz anew.
    pub refresh: Option<String>,
    /// Releases and artists (as "artist:{id}") that couldn't be asked
    /// for, and when to try again.
    pub failed: HashMap<String, Instant>,
    pub ignore: String,
}

/// What the lookup thread is asked to do.
enum Ask {
    /// Look an album up, asking MusicBrainz anew (not what's kept) if
    /// `fresh`.
    Album(Album, bool),
    /// Ask for a matched release again, for what's changed since.
    Release(String),
    /// Ask for an artist (by id), for the names they go by.
    Artist(String),
}

impl Ask {
    /// What it's about, for `Worker::busy`: the album's id, or the
    /// release or artist.
    fn id(&self) -> String {
        match self {
            Ask::Album(album, _) => album.id.clone(),
            Ask::Release(id) => format!("release:{id}"),
            Ask::Artist(id) => format!("artist:{id}"),
        }
    }
}

/// A matched song's fields as its release has them, to fill its blanks
/// in from, if it's still matched to it and not filled in yet.
pub struct Fill {
    pub key: String,
    pub release: String,
    pub said: Vec<(Field, String)>,
}

/// What a plan came to.
pub struct Planned {
    /// The songs revision it was made at.
    pub revision: u64,
    /// The library's albums, as of then.
    pub albums: Arc<Vec<Album>>,
    /// Matched songs' blanks to fill in.
    pub fills: Vec<Fill>,
    /// What's being asked for now (`Ask::id`); `None`: nothing to do.
    pub asking: Option<String>,
}

/// What it did.
pub enum Done {
    Planned(Planned),
    /// An album looked up, and (if it matched) its songs' blanks to fill.
    Album(Album, Outcome, Vec<Fill>),
    Release(String, Result<Release, String>),
    Artist(String, Result<artists::Artist, String>),
}

/// The lookup thread: given a copy of the songs, it works out what to
/// ask for next (and the blanks to fill in) and asks for it, so none of
/// that holds the window up.
pub struct Worker {
    plan: mpsc::Sender<Plan>,
    answers: mpsc::Receiver<Done>,
    /// A plan's being worked out.
    planning: bool,
    /// What it's asking for: the album (by id) being looked up, or the
    /// release or artist.
    pub busy: Option<String>,
}

impl Worker {
    pub fn start(kept: Kept, repaint: egui::Context) -> std::io::Result<Self> {
        let (plan, plans) = mpsc::channel::<Plan>();
        let (answer, answers) = mpsc::channel();
        std::thread::Builder::new().name("online lookup".into()).spawn(move || {
            let mut musicbrainz = musicbrainz::MusicBrainz::new();
            let send = |done: Done| {
                let sent = answer.send(done).is_ok();
                repaint.request_repaint();
                sent
            };
            for plan in plans {
                let genres = details::Genres::of(&plan.store, &plan.ignore);
                let fills = unfilled(&plan.store, &kept, &genres, &plan.ignore);
                let albums = Arc::new(albums(&plan.store, &plan.ignore));
                let ask = next(&plan, &kept, &albums);
                let planned = Planned { revision: plan.store.revision(), albums, fills, asking: ask.as_ref().map(Ask::id) };
                let ignore = plan.ignore;
                // (The copy of the songs goes before the UI thread changes
                // them, so it doesn't have to copy them for itself.)
                drop(plan.store);
                if !send(Done::Planned(planned)) {
                    return;
                }
                let Some(ask) = ask else { continue };
                let done = match ask {
                    Ask::Album(album, fresh) => {
                        log::info!("looking up {:?} by {:?} ({} songs{})", album.name, album.artist, album.songs.len(), if fresh { ", anew" } else { "" });
                        let outcome = look_up(&mut musicbrainz, &album, fresh);
                        let fills = match &outcome {
                            Outcome::Matched(release, tracks) => album_fills(&album, release, tracks, &genres, &ignore),
                            _ => Vec::new(),
                        };
                        Done::Album(album, outcome, fills)
                    }
                    Ask::Release(id) => {
                        log::info!("asking for release {id} again");
                        let release = musicbrainz.fresh_release(&id).map_err(|e| format!("{e:#}"));
                        Done::Release(id, release)
                    }
                    Ask::Artist(id) => {
                        log::info!("asking for artist {id}");
                        let artist = musicbrainz.artist(&id).map_err(|e| format!("{e:#}"));
                        Done::Artist(id, artist)
                    }
                };
                if !send(done) {
                    return;
                }
            }
        })?;
        Ok(Self { plan, answers, planning: false, busy: None })
    }

    /// Work out what to do next, and do it.
    pub fn plan(&mut self, plan: Plan) {
        self.planning = self.plan.send(plan).is_ok();
    }

    /// It's planning or asking.
    pub fn working(&self) -> bool {
        self.planning || self.busy.is_some()
    }

    pub fn answer(&mut self) -> Option<Done> {
        let answer = self.answers.try_recv().ok()?;
        match &answer {
            Done::Planned(planned) => {
                self.planning = false;
                self.busy = planned.asking.clone();
            }
            _ => self.busy = None,
        }
        Some(answer)
    }
}

/// `store`'s matched songs (by key), with their releases and
/// recordings: those `wanted`.
pub fn matched(store: &SongStore, wanted: impl Fn(&song_store::Online) -> bool) -> Vec<(String, String, Option<String>)> {
    store
        .known()
        .filter_map(|(key, song)| {
            let online = song.online.as_ref().filter(|o| wanted(o))?;
            Some((key.to_string(), online.release.clone()?, online.recording.clone()))
        })
        .collect()
}

/// The fills for `store`'s matched songs not filled in yet (from their
/// releases as `kept`).
fn unfilled(store: &SongStore, kept: &Kept, genres: &details::Genres, ignore: &str) -> Vec<Fill> {
    matched(store, |o| o.filled.is_none())
        .into_iter()
        .filter_map(|(key, release, recording)| {
            let release = kept.release(&release)?;
            let said = details::said(&release, details::track(&release, recording.as_deref()), genres, ignore);
            Some(Fill { key, release: release.id.clone(), said })
        })
        .collect()
}

/// The fills for `album`'s songs just matched to `release` (each to its
/// track, by index). Those already filled in from it are passed over as
/// they're applied.
fn album_fills(album: &Album, release: &Release, tracks: &[Option<usize>], genres: &details::Genres, ignore: &str) -> Vec<Fill> {
    album
        .songs
        .iter()
        .zip(tracks)
        .map(|(song, track)| {
            let recording = track.and_then(|t| release.tracks[t].recording.as_deref());
            let said = details::said(release, details::track(release, recording), genres, ignore);
            Fill { key: song.key.clone(), release: release.id.clone(), said }
        })
        .collect()
}

/// What to ask for next: the album to look up anew, else the next album
/// due, then artists matched songs credit not asked for yet, then
/// matches due for a refresh, the oldest first.
fn next(plan: &Plan, kept: &Kept, albums: &[Album]) -> Option<Ask> {
    let now = crate::settings_sync::now();
    let not_failed = |id: &str| plan.failed.get(id).is_none_or(|until| Instant::now() >= *until);
    if let Some(album) = plan.refresh.as_ref().and_then(|id| albums.iter().find(|a| a.id == *id)) {
        return Some(Ask::Album(album.clone(), true));
    }
    let store = &plan.store;
    albums
        .iter()
        .find(|a| plan.tried.get(&a.id).is_none_or(|t| t.due(now)) && wanted(store, a))
        .map(|album| Ask::Album(album.clone(), false))
        .or_else(|| {
            let ids: BTreeSet<String> = matched(store, |_| true).into_iter().map(|(_, release, _)| release).collect();
            let releases: Vec<Arc<Release>> = ids.iter().filter_map(|id| kept.release(id)).collect();
            artists::credited(releases.iter().map(|r| &**r))
                .into_iter()
                .find(|id| kept.artist(id).is_none() && not_failed(&format!("artist:{id}")))
                .map(|id| Ask::Artist(id.to_string()))
        })
        .or_else(|| {
            store
                .known()
                .filter_map(|(_, s)| s.online.as_ref().filter(|o| now - o.at >= REFRESH).and_then(|o| Some((o.at, o.release.clone()?))))
                .filter(|(_, release)| not_failed(release))
                .min()
                .map(|(_, release)| Ask::Release(release))
        })
}

/// What a lookup makes of `album`: a song on no album by its recording,
/// else by its releases.
fn look_up(musicbrainz: &mut musicbrainz::MusicBrainz, album: &Album, fresh: bool) -> Outcome {
    if album.loose {
        return musicbrainz.recording(album, fresh).unwrap_or_else(|e| Outcome::Failed(format!("{e:#}")));
    }
    match musicbrainz.releases(album, fresh) {
        Ok(releases) => match log_candidates(album, &releases, matching::decide(album, &releases)) {
            Verdict::Match(i, tracks) => Outcome::Matched(Box::new(releases[i].clone()), tracks),
            Verdict::Unsure(found) => Outcome::Unsure(found.into_iter().map(|i| releases[i].clone()).collect()),
            Verdict::NotFound => Outcome::NotFound,
        },
        Err(e) => Outcome::Failed(format!("{e:#}")),
    }
}
