//! The wishlist: music the user wants and doesn't have yet. A wish is an
//! album (a MusicBrainz release group, so any edition of it does), a song
//! (a MusicBrainz recording, on whatever album it comes), an artist (the
//! albums of theirs picked from their albums at MusicBrainz, each an album
//! wish `under` them: got once those are), or, for what MusicBrainz
//! doesn't have, just a name. An album's songs can be `skipped`: it's got
//! with the rest. Kept in the config folder's
//! `wishlist.json` and synced as a blob of its own, a leaf per wish
//! (`leaves`, `apply`).
//!
//! What of a wish is here isn't kept or synced: each device works it out
//! from its own library (`progress`), as songs get matched at MusicBrainz.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fields::{Facts, Field};
use crate::lookup::Kept;
use crate::lookup::matching::{ArtistAlbum, Release};
use crate::settings_sync::Fields;
use crate::song_store::{Song, SongStore};

/// The synced data's name on the server.
pub const NAME: &str = "wishlist";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Album,
    Song,
    Artist,
    /// Just a name (a title, an artist, or both): an album or a song by
    /// it does.
    Text,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Album => "Album",
            Kind::Song => "Song",
            Kind::Artist => "Artist",
            Kind::Text => "By name",
        }
    }
}

/// The kinds of an artist's albums (`lookup::matching::ArtistAlbum::kind`),
/// and those shown of them unless asked otherwise.
pub const KINDS: [&str; 6] = ["Album", "EP", "Single", "Compilation", "Live", "Other"];
pub const SHOWN: [&str; 2] = ["Album", "EP"];

/// Something wished for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wish {
    pub kind: Kind,
    /// Its name: an artist's own.
    pub title: String,
    /// Who it's by; for an artist, what tells them apart at MusicBrainz
    /// ("Japanese vocaloid producer").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    /// An album's release group, a song's recording, or an artist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub musicbrainz: Option<String>,
    /// A release it's on: an album's (its cover, and how many tracks it
    /// has), or the album a song is on (its cover).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// An album's type at MusicBrainz ("Album", "EP", "Single").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
    /// How many tracks an album's `release` has.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tracks: u32,
    /// The songs (recordings) of an album not wished for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// The artist's wish (by ID) an album's is one of the albums of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// When it was wished for (Unix seconds).
    #[serde(default)]
    pub added: i64,
    /// The program that wished for it through the local API; `None` for
    /// the user's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Wish {
    /// A wish of `kind` named `title`, and nothing else yet.
    pub fn new(kind: Kind, title: &str) -> Self {
        Self {
            kind,
            title: title.to_string(),
            artist: None,
            year: None,
            musicbrainz: None,
            release: None,
            release_type: None,
            tracks: 0,
            skipped: Vec::new(),
            under: None,
            note: String::new(),
            added: 0,
            added_by: None,
        }
    }

    /// Whether `other` wishes for the same thing.
    pub fn same(&self, other: &Wish) -> bool {
        match (&self.musicbrainz, &other.musicbrainz) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.kind == other.kind && crate::matching::close(&self.title, &other.title) && close_or_none(&self.artist, &other.artist),
            _ => false,
        }
    }
}

fn close_or_none(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => crate::matching::close(a, b),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Default)]
pub struct Wishlist {
    pub wishes: BTreeMap<String, Wish>,
    path: Option<PathBuf>,
    revision: u64,
}

impl Wishlist {
    pub fn load() -> Self {
        let path = crate::config::config_dir().ok().map(|d| d.join("wishlist.json"));
        let wishes = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { wishes, path, revision: 0 }
    }

    /// Goes up with every change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision += 1;
        let Some(path) = &self.path else { return };
        if let Err(e) = serde_json::to_vec(&self.wishes).map_err(anyhow::Error::from).and_then(|b| Ok(std::fs::write(path, b)?)) {
            log::warn!("couldn't save {}: {e}", path.display());
        }
    }

    /// The wish for the same thing as `wish`, if there's one.
    pub fn find(&self, wish: &Wish) -> Option<&str> {
        self.wishes.iter().find(|(_, w)| w.same(wish)).map(|(id, _)| id.as_str())
    }

    /// Wish for `wish`, wished for now: its ID (the one there was, if it
    /// was wished for already).
    pub fn add(&mut self, mut wish: Wish) -> String {
        if let Some(id) = self.find(&wish).map(str::to_string) {
            // (An album wished for by itself before: now one of the
            // artist's picking it.)
            if let Some(under) = wish.under {
                self.adopt(&under, &[&id]);
            }
            return id;
        }
        wish.added = crate::settings_sync::now();
        let id = crate::playlist::new_id();
        self.wishes.insert(id.clone(), wish);
        self.changed();
        id
    }

    /// Put the wishes `ids` that aren't under an artist under the artist's
    /// wish `artist`: whether any was.
    pub fn adopt(&mut self, artist: &str, ids: &[&str]) -> bool {
        let mut adopted = false;
        for id in ids {
            if let Some(wish) = self.wishes.get_mut(*id).filter(|w| w.under.is_none() && *id != artist) {
                wish.under = Some(artist.to_string());
                adopted = true;
            }
        }
        if adopted {
            self.changed();
        }
        adopted
    }

    /// Take the wishes `ids` off the wishlist, and an artist's albums with
    /// them.
    pub fn remove(&mut self, ids: &[String]) {
        let before = self.wishes.len();
        self.wishes.retain(|id, wish| !ids.contains(id) && !wish.under.as_ref().is_some_and(|artist| ids.contains(artist)));
        if self.wishes.len() != before {
            self.changed();
        }
    }

    /// Wish for the song `recording` of the album wish `id`, or skip it.
    pub fn set_skipped(&mut self, id: &str, recording: &str, skipped: bool) {
        let Some(wish) = self.wishes.get_mut(id) else { return };
        if wish.skipped.iter().any(|r| r == recording) == skipped {
            return;
        }
        wish.skipped.retain(|r| r != recording);
        if skipped {
            wish.skipped.push(recording.to_string());
        }
        self.changed();
    }

    /// The wishes for the albums of the artist's wish `id`.
    pub fn under<'a>(&'a self, id: &'a str) -> impl Iterator<Item = (&'a String, &'a Wish)> {
        self.wishes.iter().filter(move |(_, w)| w.under.as_deref() == Some(id))
    }

    pub fn set_note(&mut self, id: &str, note: &str) {
        if let Some(wish) = self.wishes.get_mut(id)
            && wish.note != note
        {
            wish.note = note.to_string();
            self.changed();
        }
    }
}

/// The wishes as leaves: each by its ID.
pub fn leaves(wishlist: &Wishlist) -> BTreeMap<String, Value> {
    wishlist.wishes.iter().map(|(id, wish)| (id.clone(), serde_json::to_value(wish).unwrap_or_default())).collect()
}

/// Make `wishlist` what `fields` say: whether that changed it.
pub fn apply(wishlist: &mut Wishlist, fields: &Fields) -> bool {
    let wishes: BTreeMap<String, Wish> = fields
        .iter()
        .filter(|(id, _)| crate::playlist::is_id(id))
        .filter_map(|(id, field)| Some((id.clone(), serde_json::from_value(field.value.clone()).ok()?)))
        .collect();
    if wishes == wishlist.wishes {
        return false;
    }
    wishlist.wishes = wishes;
    wishlist.changed();
    true
}

/// How much of a wish is here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Wanted,
    Partly,
    Got,
}

impl State {
    pub const ALL: [State; 3] = [State::Wanted, State::Partly, State::Got];

    pub fn name(self) -> &'static str {
        match self {
            State::Wanted => "Wanted",
            State::Partly => "Partly got",
            State::Got => "Got it",
        }
    }
}

/// What of a wish is here: of how many tracks (an album's best edition,
/// or 1; for an artist, of how many of their albums picked), how many
/// (albums got), and its songs here (by key, in album order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub have: usize,
    pub total: usize,
    pub songs: Vec<String>,
}

impl Progress {
    pub fn state(&self) -> State {
        match self.have {
            0 => State::Wanted,
            have if have >= self.total => State::Got,
            _ => State::Partly,
        }
    }

    /// "7 of 12", for an album partly here (or an artist's albums).
    pub fn text(&self) -> String {
        match self.state() {
            // (An artist none of whose albums is picked yet.)
            _ if self.total == 0 => "None picked".to_string(),
            State::Partly => format!("{} of {}", self.have, self.total),
            state => state.name().to_string(),
        }
    }
}

/// The wishes, by ID.
pub type Wishes = BTreeMap<String, Wish>;

/// The wishes a song fills, looked up by what it's matched to: made once
/// for many songs.
pub struct Index<'a> {
    wishes: &'a Wishes,
    groups: BTreeMap<&'a str, Vec<&'a str>>,
    releases: BTreeMap<&'a str, Vec<&'a str>>,
    recordings: BTreeMap<&'a str, Vec<&'a str>>,
    named: Vec<&'a str>,
}

impl<'a> Index<'a> {
    pub fn new(wishes: &'a Wishes) -> Self {
        let mut index = Index { wishes, groups: BTreeMap::new(), releases: BTreeMap::new(), recordings: BTreeMap::new(), named: Vec::new() };
        for (id, wish) in wishes {
            let id = id.as_str();
            match wish.kind {
                Kind::Album => {
                    if let Some(group) = &wish.musicbrainz {
                        index.groups.entry(group).or_default().push(id);
                    }
                    if let Some(release) = &wish.release {
                        index.releases.entry(release).or_default().push(id);
                    }
                }
                Kind::Song => {
                    if let Some(recording) = &wish.musicbrainz {
                        index.recordings.entry(recording).or_default().push(id);
                    }
                }
                Kind::Artist => {}
                Kind::Text => index.named.push(id),
            }
        }
        index
    }

    /// What wish `id` is called: its title, or for one of just an
    /// artist, theirs.
    pub fn name(&self, id: &str) -> &'a str {
        let wish = &self.wishes[id];
        if wish.title.trim().is_empty() { wish.artist.as_deref().unwrap_or_default() } else { &wish.title }
    }

    /// The wishes (by ID) `song` fills: an album's (its match's group, or
    /// the wished release its file says), unless it's a song skipped; a
    /// song's (its recording); one by name (a song or an album named so,
    /// by the artist if it says one); and the artist's an album is under.
    pub fn fills(&self, song: &Song) -> Vec<&'a str> {
        let facts = Facts::of(song);
        let recording = facts.recording();
        let albums = match song.online.as_ref().and_then(|o| o.group.as_deref()).and_then(|g| self.groups.get(g)) {
            Some(albums) => Some(albums),
            None => facts.release().and_then(|r| self.releases.get(r)),
        };
        let mut ids: Vec<&str> =
            albums.into_iter().flatten().copied().filter(|id| recording.is_none_or(|r| !self.wishes[*id].skipped.iter().any(|s| s == r))).collect();
        ids.extend(recording.and_then(|r| self.recordings.get(r)).into_iter().flatten().copied());
        if !self.named.is_empty() {
            let text = |field| facts.text(field);
            let (title, album) = (text(Field::Title), text(Field::Album));
            let artists = [text(Field::Artist), text(Field::AlbumArtist)];
            ids.extend(self.named.iter().copied().filter(|id| {
                let wish = &self.wishes[*id];
                let by = wish.artist.as_ref().is_none_or(|wanted| artists.iter().flatten().any(|a| crate::matching::close(a, wanted)));
                let named = match (!wish.title.trim().is_empty()).then_some(wish.title.as_str()) {
                    Some(wanted) => [&title, &album].into_iter().flatten().any(|n| crate::matching::close(n, wanted)),
                    // (Only an artist: any song of theirs.)
                    None => true,
                };
                named && by
            }));
        }
        let artists: Vec<&str> = ids.iter().filter_map(|id| self.wishes[*id].under.as_deref()).filter(|a| self.wishes.contains_key(*a)).collect();
        ids.extend(artists.into_iter().filter_map(|a| self.wishes.get_key_value(a).map(|(id, _)| id.as_str())));
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// What of each wish (by ID) is in `store`: the songs that fill it
/// ([`Index::fills`]). An album's got once every track (not skipped) of
/// one of its editions is here (`kept` has their track lists; one it
/// hasn't is taken to have as many as the wished one). An artist's is of
/// the albums under them: got once they all are.
pub fn progress(wishlist: &Wishlist, store: &SongStore, kept: &Kept) -> BTreeMap<String, Progress> {
    let index = Index::new(&wishlist.wishes);
    // The songs each wish has here, with the release each is on.
    let mut filled: BTreeMap<&str, Vec<(String, Option<String>)>> = BTreeMap::new();
    for (key, song) in store.known().filter(|(key, _)| store.has(key)) {
        for id in index.fills(song) {
            filled.entry(id).or_default().push((key.to_string(), Facts::of(song).release().map(str::to_string)));
        }
    }
    let ordered = |mut keys: Vec<String>| {
        let place = |key: &String| {
            let facts = store.get_by_key(key).map(Facts::of).unwrap_or_default();
            let number = |field| facts.value(field).and_then(|v| v.number).map_or(i64::MAX, |n| n as i64);
            (number(Field::Disc), number(Field::Track), facts.file_name.to_string())
        };
        keys.sort_by_cached_key(place);
        keys.dedup();
        keys
    };
    let mut progress: BTreeMap<String, Progress> = wishlist
        .wishes
        .iter()
        .filter(|(_, wish)| wish.kind != Kind::Artist)
        .map(|(id, wish)| {
            let songs = filled.remove(id.as_str()).unwrap_or_default();
            let progress = match wish.kind {
                Kind::Album => {
                    // Each edition's songs here, of its tracks.
                    let skipped = |recording: &Option<String>| recording.as_ref().is_some_and(|r| wish.skipped.contains(r));
                    let mut editions: BTreeMap<Option<&str>, usize> = BTreeMap::new();
                    for (_, release) in &songs {
                        *editions.entry(release.as_deref()).or_default() += 1;
                    }
                    let wished = (wish.tracks as usize).saturating_sub(wish.skipped.len()).max(1);
                    let best = editions
                        .iter()
                        .map(|(release, have)| {
                            let tracks = |r: Arc<Release>| r.tracks.iter().filter(|t| !skipped(&t.recording)).count().max(1);
                            let total = release.and_then(|r| kept.release(r)).map_or(wished, tracks);
                            ((*have).min(total), total)
                        })
                        .max_by_key(|&(have, total)| (have >= total, have))
                        .unwrap_or((0, wished));
                    Progress { have: best.0, total: best.1, songs: ordered(songs.into_iter().map(|(key, _)| key).collect()) }
                }
                _ => Progress { have: usize::from(!songs.is_empty()), total: 1, songs: ordered(songs.into_iter().map(|(key, _)| key).collect()) },
            };
            (id.clone(), progress)
        })
        .collect();
    for id in wishlist.wishes.iter().filter(|(_, w)| w.kind == Kind::Artist).map(|(id, _)| id) {
        let albums: Vec<&Progress> = wishlist.under(id).filter_map(|(album, _)| progress.get(album)).collect();
        let artist = Progress {
            have: albums.iter().filter(|p| p.state() == State::Got).count(),
            total: albums.len(),
            songs: albums.iter().flat_map(|p| p.songs.iter().cloned()).collect(),
        };
        progress.insert(id.clone(), artist);
    }
    progress
}

/// The release song `key` is on at MusicBrainz: its match's, else the one
/// its file says.
pub fn release_of<'s>(store: &'s SongStore, key: &str) -> Option<&'s str> {
    Facts::of(store.get_by_key(key)?).release()
}

/// A wish for the album `release` is (its group), if some of its tracks
/// aren't here.
pub fn rest_of(store: &SongStore, release: &Release) -> Option<Wish> {
    let group = release.group.clone()?;
    let have = store.known().filter(|(key, song)| store.has(key) && Facts::of(song).release() == Some(release.id.as_str())).count();
    (have < release.tracks.len()).then(|| Wish {
        artist: Some(release.artist.clone()).filter(|a| !a.is_empty()),
        year: release.first_released.as_deref().or(release.date.as_deref()).and_then(|d| d.split('-').next()).filter(|y| !y.is_empty()).map(str::to_string),
        musicbrainz: Some(group),
        release: Some(release.id.clone()),
        tracks: release.tracks.len() as u32,
        ..Wish::new(Kind::Album, &release.title)
    })
}

/// The MusicBrainz artist (their id and own name) the name `artist` is
/// of, as the releases of `songs` (keys) kept here credit them.
pub fn artist_of(store: &SongStore, kept: &Kept, songs: &[String], artist: &str) -> Option<(String, String)> {
    let wanted = crate::fields::group_key(artist, "");
    for key in songs {
        let Some(release) = release_of(store, key).and_then(|r| kept.release(r)) else { continue };
        let credits = release.credits.iter().chain(release.tracks.iter().flat_map(|t| &t.credits));
        let mut credits = credits.filter(|c| c.id != crate::lookup::musicbrainz::VARIOUS_ARTISTS);
        if let Some(credit) = credits.find(|c| [&c.credited, &c.name].iter().any(|n| crate::fields::group_key(n, "") == wanted)) {
            return Some((credit.id.clone(), credit.name.clone()));
        }
    }
    None
}

/// A wish for more by `artist`: a watch on the MusicBrainz artist their
/// songs (`songs`) are tied to, else anything by their name.
pub fn more_by(store: &SongStore, kept: &Kept, songs: &[String], artist: &str) -> Wish {
    match artist_of(store, kept, songs, artist) {
        Some((id, name)) => Wish { musicbrainz: Some(id), ..Wish::new(Kind::Artist, &name) },
        None => Wish { artist: Some(artist.to_string()), ..Wish::new(Kind::Text, "") },
    }
}

/// The albums (release groups, by ID) some of which is in `store`.
pub fn albums_here(store: &SongStore) -> BTreeSet<&str> {
    store.known().filter(|(key, _)| store.has(key)).filter_map(|(_, song)| song.online.as_ref()?.group.as_deref()).collect()
}

/// The songs (recordings, by ID) in `store`.
pub fn songs_here(store: &SongStore) -> BTreeSet<&str> {
    store.known().filter(|(key, _)| store.has(key)).filter_map(|(_, song)| crate::fields::Facts::of(song).recording()).collect()
}

/// The albums of the artist's wish `wish` (as `kept` has them) of the
/// kinds `shown` that none of is in `store`.
pub fn missing_albums(wish: &Wish, store: &SongStore, kept: &Kept, shown: &[&str]) -> Vec<ArtistAlbum> {
    let Some(albums) = wish.musicbrainz.as_deref().and_then(|id| kept.discography(id)) else { return Vec::new() };
    let here = albums_here(store);
    albums.iter().filter(|a| shown.contains(&a.kind) && !here.contains(a.id.as_str())).cloned().collect()
}

/// A wish for `album`, one of the albums of the artist's wish `under`
/// (named `artist`), its tracks not known yet.
pub fn album_wish(album: &ArtistAlbum, artist: &str, under: &str) -> Wish {
    Wish {
        under: Some(under.to_string()),
        artist: Some(artist.to_string()),
        year: album.year.clone(),
        musicbrainz: Some(album.id.clone()),
        release_type: Some(album.kind.to_string()),
        ..Wish::new(Kind::Album, &album.title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::{self, Online};

    fn album(group: &str, release: &str, tracks: u32) -> Wish {
        Wish { musicbrainz: Some(group.into()), release: Some(release.into()), tracks, ..Wish::new(Kind::Album, "Modal Soul") }
    }

    #[test]
    fn wishes_come_true_as_songs_get_matched() {
        let root = std::env::temp_dir().join(format!("synththing-wishlist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let music = root.join("music");
        let mut store = SongStore::open(music.clone());
        let mut keys = Vec::new();
        for (i, name) in ["Feather.ogg", "Luv Sic.ogg", "Other.ogg", "Tagged.ogg"].iter().enumerate() {
            let stored = song_store::write_in(format!("song {i}").as_bytes(), name, &music).unwrap();
            store.add(stored.clone());
            keys.push(stored.key);
        }
        let matched = |release: &str, recording: &str| Online { release: Some(release.into()), group: Some("g1".into()), recording: Some(recording.into()), ..Online::default() };
        store.set_online(vec![(keys[0].clone(), Some(matched("r1", "c1"))), (keys[1].clone(), Some(matched("r2", "c2")))]);

        let mut wishlist = Wishlist::default();
        let modal = wishlist.add(album("g1", "r1", 2));
        let feather = wishlist.add(Wish { musicbrainz: Some("c1".into()), ..Wish::new(Kind::Song, "Feather") });
        let named = wishlist.add(Wish { artist: Some("nobody".into()), ..Wish::new(Kind::Text, "Other") });
        assert_eq!(wishlist.add(album("g1", "r9", 5)), modal, "the same album isn't wished for twice");

        let kept = Kept::default();
        let progress = progress(&wishlist, &store, &kept);
        assert_eq!((progress[&modal].have, progress[&modal].total, progress[&modal].state()), (1, 2, State::Partly), "one song on each of two editions");
        assert_eq!(progress[&modal].songs.len(), 2);
        assert_eq!(progress[&feather].state(), State::Got);
        assert_eq!(progress[&named].state(), State::Wanted, "by another artist");

        store.set_online(vec![(keys[1].clone(), Some(matched("r1", "c2")))]);
        let progress = super::progress(&wishlist, &store, &kept);
        assert_eq!(progress[&modal].state(), State::Got, "both of one edition's tracks");

        let track = |recording: &str| crate::lookup::matching::Track { recording: Some(recording.into()), ..Default::default() };
        kept.keep_release(Release { id: "r1".into(), tracks: vec![track("c1"), track("c2"), track("c3")], ..Default::default() });
        assert_eq!(super::progress(&wishlist, &store, &kept)[&modal].text(), "2 of 3");
        wishlist.set_skipped(&modal, "c3", true);
        assert_eq!(super::progress(&wishlist, &store, &kept)[&modal].state(), State::Got, "the song skipped isn't wanted");
        wishlist.set_skipped(&modal, "c2", true);
        let progress = super::progress(&wishlist, &store, &kept);
        assert_eq!((progress[&modal].have, progress[&modal].total, progress[&modal].songs.len()), (1, 1, 1), "nor is one skipped that's here");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_rest_of_an_album_is_wished_only_while_some_is_missing() {
        let root = std::env::temp_dir().join(format!("synththing-wish-rest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let music = root.join("music");
        let mut store = SongStore::open(music.clone());
        let stored = song_store::write_in(b"feather", "Feather.ogg", &music).unwrap();
        store.add(stored.clone());
        assert_eq!(release_of(&store, &stored.key), None, "not matched");
        let online = Online { release: Some("r1".into()), group: Some("g1".into()), ..Online::default() };
        store.set_online(vec![(stored.key.clone(), Some(online))]);
        assert_eq!(release_of(&store, &stored.key), Some("r1"));
        let track = crate::lookup::matching::Track::default();
        let release = Release {
            id: "r1".into(),
            group: Some("g1".into()),
            title: "Modal Soul".into(),
            artist: "Nujabes".into(),
            first_released: Some("2005-11-11".into()),
            tracks: vec![track.clone(), track],
            ..Default::default()
        };
        let wish = rest_of(&store, &release).expect("one of two tracks here");
        assert_eq!((wish.title.as_str(), wish.musicbrainz.as_deref(), wish.year.as_deref(), wish.tracks), ("Modal Soul", Some("g1"), Some("2005"), 2));
        assert_eq!(rest_of(&store, &Release { tracks: vec![Default::default()], ..release }), None, "all of it's here");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_artist_is_watched_over_the_albums_it_counts() {
        let root = std::env::temp_dir().join(format!("synththing-wish-artist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let music = root.join("music");
        let mut store = SongStore::open(music.clone());
        let stored = song_store::write_in(b"feather", "Feather.ogg", &music).unwrap();
        store.add(stored.clone());
        let credit = crate::lookup::matching::Credit { id: "a1".into(), credited: "nujabes".into(), name: "Nujabes".into(), sort: String::new() };
        let release = Release { id: "r1".into(), group: Some("g1".into()), credits: vec![credit], ..Default::default() };
        let kept = Kept::default();
        kept.keep_release(release);
        let online = Online { release: Some("r1".into()), group: Some("g1".into()), ..Online::default() };
        store.set_online(vec![(stored.key.clone(), Some(online))]);

        let songs = [stored.key.clone()];
        let watch = more_by(&store, &kept, &songs, "Nujabes");
        assert_eq!((watch.kind, watch.title.as_str(), watch.musicbrainz.as_deref()), (Kind::Artist, "Nujabes", Some("a1")), "tied by the credit");
        let loose = more_by(&store, &kept, &songs, "Shing02");
        assert_eq!((loose.kind, loose.artist.as_deref()), (Kind::Text, Some("Shing02")), "anything by a name not credited");

        let mut wishlist = Wishlist::default();
        let id = wishlist.add(watch);
        let progress = super::progress(&wishlist, &store, &kept);
        assert_eq!((progress[&id].text(), progress[&id].state()), ("None picked".to_string(), State::Wanted));

        let album = |id: &str, kind| ArtistAlbum { id: id.into(), title: id.into(), year: None, kind };
        kept.keep_discography("a1", vec![album("g1", "Album"), album("g2", "Album"), album("g3", "Single")]);
        let missing: Vec<String> = missing_albums(&wishlist.wishes[&id], &store, &kept, &SHOWN).into_iter().map(|a| a.id).collect();
        assert_eq!(missing, ["g2"], "the single isn't shown");
        let alone = wishlist.add(Wish { tracks: 1, ..Wish::new(Kind::Album, "g1") });
        wishlist.wishes.get_mut(&alone).unwrap().musicbrainz = Some("g1".into());
        let modal = wishlist.add(album_wish(&album("g1", "Album"), "Nujabes", &id));
        assert_eq!((modal.as_str(), wishlist.wishes[&modal].under.as_deref()), (alone.as_str(), Some(id.as_str())), "wished for by itself before: now theirs");
        let other = wishlist.add(album_wish(&album("g2", "Album"), "Nujabes", &id));
        assert_eq!(wishlist.under(&id).count(), 2);
        let progress = super::progress(&wishlist, &store, &kept);
        assert_eq!((progress[&modal].state(), progress[&other].state()), (State::Got, State::Wanted));
        assert_eq!((progress[&id].text(), progress[&id].songs.len()), ("1 of 2".to_string(), 1), "of the albums picked");

        wishlist.remove(&[other]);
        assert_eq!(super::progress(&wishlist, &store, &kept)[&id].state(), State::Got, "all of those picked");
        wishlist.remove(&[id]);
        assert!(wishlist.wishes.is_empty(), "the artist's albums go with them");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn wishes_sync_as_leaves() {
        let mut here = Wishlist::default();
        let id = here.add(album("g1", "r1", 2));
        here.set_note(&id, "the remaster");
        let fields: Fields = leaves(&here).into_iter().map(|(k, value)| (k, crate::settings_sync::Field { value, changed: 1 })).collect();
        let mut there = Wishlist::default();
        assert!(apply(&mut there, &fields));
        assert_eq!(there.wishes, here.wishes);
        assert!(!apply(&mut there, &fields), "nothing new");
        assert!(apply(&mut there, &Fields::new()));
        assert!(there.wishes.is_empty(), "removed elsewhere");
    }
}
