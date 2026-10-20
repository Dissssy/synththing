//! The library's Artists and Albums, made from the songs' tags: each album
//! by its album artist (its artist when that's empty) and name, its tracks
//! in disc then track order, and each artist with their albums and the
//! songs of theirs on none. Songs with no artist are on no artist, and
//! songs with no album on no album: they're in Songs only.
//!
//! (These are the By Artist and By Album rulesets the design has, before
//! there's a rules engine to put them on.)

use std::collections::BTreeMap;
use std::path::PathBuf;

/// What a song's tags say, as the views need it.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub path: PathBuf,
    /// Its name, as songs are named everywhere (`song_title`).
    pub name: String,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    /// Seconds.
    pub length: Option<f64>,
    pub starred: bool,
    /// When it came into the library (Unix seconds).
    pub imported: i64,
    pub midi: bool,
}

impl Track {
    /// Who it's filed under: its album artist, else its artist.
    pub fn filed_under(&self) -> Option<&str> {
        self.album_artist.as_deref().or(self.artist.as_deref()).map(str::trim).filter(|a| !a.is_empty())
    }

    fn album_name(&self) -> Option<&str> {
        self.album.as_deref().map(str::trim).filter(|a| !a.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct Album {
    /// What tells albums apart: artist and name, in lowercase.
    pub key: String,
    pub title: String,
    pub artist: String,
    /// The latest year its tracks give.
    pub year: Option<u32>,
    /// Disc, then track, then name.
    pub tracks: Vec<Track>,
    /// When its newest track came in.
    pub added: i64,
}

impl Album {
    /// The track to take its art from: the first that isn't a MIDI file.
    pub fn art_from(&self) -> Option<&PathBuf> {
        self.tracks.iter().find(|t| !t.midi).map(|t| &t.path)
    }

    pub fn length(&self) -> f64 {
        self.tracks.iter().filter_map(|t| t.length).sum()
    }

    /// Whether it has more than one disc (to show their numbers).
    pub fn discs(&self) -> bool {
        let mut discs = self.tracks.iter().map(|t| t.disc.unwrap_or(1));
        let first = discs.next();
        discs.any(|d| Some(d) != first)
    }
}

#[derive(Clone, Debug)]
pub struct Artist {
    /// Their name in lowercase.
    pub key: String,
    pub name: String,
    /// Into `Index::albums`, by year then name.
    pub albums: Vec<usize>,
    /// Their songs on no album, by name.
    pub singles: Vec<Track>,
}

impl Artist {
    pub fn song_count(&self, index: &Index) -> usize {
        self.albums.iter().map(|&a| index.albums[a].tracks.len()).sum::<usize>() + self.singles.len()
    }

    /// Every song of theirs, in the order the pane lists them.
    pub fn tracks<'a>(&'a self, index: &'a Index) -> impl Iterator<Item = &'a Track> {
        self.albums.iter().flat_map(|&a| index.albums[a].tracks.iter()).chain(self.singles.iter())
    }
}

/// The library's albums (by name) and artists (by name).
#[derive(Clone, Debug, Default)]
pub struct Index {
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
}

impl Index {
    pub fn build(tracks: impl IntoIterator<Item = Track>) -> Self {
        let mut albums: BTreeMap<String, Album> = BTreeMap::new();
        let mut artists: BTreeMap<String, Artist> = BTreeMap::new();
        let mut singles: Vec<Track> = Vec::new();
        for track in tracks {
            let Some(artist) = track.filed_under().map(str::to_string) else { continue };
            let artist_key = artist.to_lowercase();
            artists.entry(artist_key.clone()).or_insert_with(|| Artist { key: artist_key.clone(), name: artist.clone(), albums: Vec::new(), singles: Vec::new() });
            let Some(title) = track.album_name().map(str::to_string) else {
                singles.push(track);
                continue;
            };
            let key = format!("{artist_key}\u{0}{}", title.to_lowercase());
            let album = albums.entry(key.clone()).or_insert_with(|| Album { key, title, artist, year: None, tracks: Vec::new(), added: 0 });
            album.year = album.year.max(track.year);
            album.added = album.added.max(track.imported);
            album.tracks.push(track);
        }
        let mut albums: Vec<Album> = albums.into_values().collect();
        for album in &mut albums {
            album.tracks.sort_by(|a, b| {
                (a.disc.unwrap_or(1), a.track.unwrap_or(u32::MAX)).cmp(&(b.disc.unwrap_or(1), b.track.unwrap_or(u32::MAX))).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
        albums.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then_with(|| a.artist.to_lowercase().cmp(&b.artist.to_lowercase())));
        for (i, album) in albums.iter().enumerate() {
            let key = album.artist.to_lowercase();
            if let Some(artist) = artists.get_mut(&key) {
                artist.albums.push(i);
            }
        }
        for track in singles {
            if let Some(artist) = track.filed_under().map(str::to_lowercase).and_then(|k| artists.get_mut(&k)) {
                artist.singles.push(track);
            }
        }
        let mut artists: Vec<Artist> = artists.into_values().collect();
        for artist in &mut artists {
            artist.albums.sort_by(|&a, &b| {
                let (a, b) = (&albums[a], &albums[b]);
                a.year.unwrap_or(u32::MAX).cmp(&b.year.unwrap_or(u32::MAX)).then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            });
            artist.singles.sort_by_key(|t| t.name.to_lowercase());
        }
        Self { albums, artists }
    }

    pub fn album(&self, key: &str) -> Option<&Album> {
        self.albums.iter().find(|a| a.key == key)
    }

    pub fn artist(&self, key: &str) -> Option<&Artist> {
        self.artists.iter().find(|a| a.key == key)
    }
}

/// Whether `text` has every word of `query` in it, ignoring case: the
/// views' searches.
pub fn matches(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query.to_lowercase().split_whitespace().all(|word| text.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str, artist: Option<&str>, album_artist: Option<&str>, album: Option<&str>, disc: Option<u32>, number: Option<u32>) -> Track {
        Track {
            path: PathBuf::from(name),
            name: name.into(),
            artist: artist.map(Into::into),
            album_artist: album_artist.map(Into::into),
            album: album.map(Into::into),
            disc,
            track: number,
            ..Track::default()
        }
    }

    #[test]
    fn albums_go_by_album_artist_and_keep_together() {
        let index = Index::build([
            track("b", Some("Guest"), Some("Composer"), Some("OST"), None, Some(2)),
            track("a", Some("Composer"), None, Some("ost"), None, Some(1)),
            track("c", Some("Composer"), Some("Composer"), Some("OST"), Some(2), Some(1)),
        ]);
        assert_eq!(index.albums.len(), 1, "the guest track doesn't split the album, nor does case");
        let names: Vec<&str> = index.albums[0].tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"], "disc, then track");
        assert!(index.albums[0].discs());
        let artists: Vec<&str> = index.artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(artists, ["Composer"], "a guest on someone's album isn't an artist of their own here");
    }

    #[test]
    fn songs_without_an_album_are_singles_and_without_an_artist_nowhere() {
        let index = Index::build([track("x", Some("Solo"), None, None, None, None), track("y", None, None, Some("Lost"), None, None)]);
        assert!(index.albums.is_empty());
        assert_eq!(index.artists.len(), 1);
        assert_eq!(index.artists[0].singles.len(), 1);
        assert_eq!(index.artists[0].song_count(&index), 1);
    }

    #[test]
    fn searches_need_every_word() {
        assert!(matches("Tribute To Nujabes", "nujabes tribute"));
        assert!(!matches("Tribute To Nujabes", "nujabes modal"));
    }
}
