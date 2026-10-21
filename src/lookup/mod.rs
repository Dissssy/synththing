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
mod musicbrainz;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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

/// The release `id`, with its tracks, as MusicBrainz said when it was
/// last asked (on this device).
pub fn kept_release(id: &str) -> Option<Release> {
    musicbrainz::kept_release(id)
}

/// The artist `id`, with their aliases, as MusicBrainz said when they
/// were asked for (on this device).
pub fn kept_artist(id: &str) -> Option<artists::Artist> {
    musicbrainz::kept_artist(id)
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

/// What the lookup thread is asked to do.
pub enum Ask {
    /// Look an album up, asking MusicBrainz anew (not what's kept) if
    /// `fresh`.
    Album(Album, bool),
    /// Ask for a matched release again, for what's changed since.
    Release(String),
    /// Ask for an artist (by id), for the names they go by.
    Artist(String),
}

/// What it did.
pub enum Done {
    Album(Album, Outcome),
    Release(String, Result<Release, String>),
    Artist(String, Result<artists::Artist, String>),
}

/// The lookup thread: an album (or release) at a time, asked for and
/// answered.
pub struct Worker {
    ask: mpsc::Sender<Ask>,
    answers: mpsc::Receiver<Done>,
    /// What it's doing: the album (by id) being looked up, or the release.
    pub busy: Option<String>,
}

impl Worker {
    pub fn start() -> std::io::Result<Self> {
        let (ask, asked) = mpsc::channel::<Ask>();
        let (answer, answers) = mpsc::channel();
        std::thread::Builder::new().name("online lookup".into()).spawn(move || {
            let mut musicbrainz = musicbrainz::MusicBrainz::new();
            for ask in asked {
                let done = match ask {
                    Ask::Album(album, fresh) => {
                        let outcome = look_up(&mut musicbrainz, &album, fresh);
                        Done::Album(album, outcome)
                    }
                    Ask::Release(id) => {
                        let release = musicbrainz.fresh_release(&id).map_err(|e| format!("{e:#}"));
                        Done::Release(id, release)
                    }
                    Ask::Artist(id) => {
                        let artist = musicbrainz.artist(&id).map_err(|e| format!("{e:#}"));
                        Done::Artist(id, artist)
                    }
                };
                if answer.send(done).is_err() {
                    return;
                }
            }
        })?;
        Ok(Self { ask, answers, busy: None })
    }

    pub fn ask(&mut self, ask: Ask) {
        self.busy = Some(match &ask {
            Ask::Album(album, _) => album.id.clone(),
            Ask::Release(id) => format!("release:{id}"),
            Ask::Artist(id) => format!("artist:{id}"),
        });
        if self.ask.send(ask).is_err() {
            self.busy = None;
        }
    }

    pub fn answer(&mut self) -> Option<Done> {
        let answer = self.answers.try_recv().ok()?;
        self.busy = None;
        Some(answer)
    }
}

/// What a lookup makes of `album`: a song on no album by its recording,
/// else by its releases.
fn look_up(musicbrainz: &mut musicbrainz::MusicBrainz, album: &Album, fresh: bool) -> Outcome {
    if album.loose {
        return match musicbrainz.recording(album, fresh) {
            Ok(Some((release, track))) => Outcome::Matched(Box::new(release), vec![Some(track)]),
            Ok(None) => Outcome::NotFound,
            Err(e) => Outcome::Failed(format!("{e:#}")),
        };
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
