//! Tags guessed from what a song says of itself outside its tags: its
//! file name ("04. This War of Mine OST - Piotr Musiał - These Cold
//! Days"), a track number left in its title ("27. Luke"), and a video's
//! title ("George Michael - Careless Whisper (Official Video)" by
//! "georgemichaelVEVO"). Offered as clean-up suggestions (`cleanup`),
//! never written by themselves.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::fields::{self, Facts, Field};
use crate::song_store::SongStore;

/// A track number leading a file name: "177_", "6 - ", "34. ".
static FILE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,3})\s*[.\-_)]\s*(\D.*)$").unwrap());
/// A track number leading a title, spaced from it: "27. Luke".
static TITLE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{1,3})\s*[.\-_)]\s+(\S.*)$").unwrap());
/// A track number leading the last part of a file name, as Bandcamp
/// names them ("Artist - Album - 01 Title"): two digits, or three with
/// a leading zero ("200 AM" is a title).
static PART_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{2}|0\d{2})\s+(\D.*)$").unwrap());
/// What a video's title says of it: "(Official Music Video)", "[HD]",
/// "(Lyrics)", or "Official Video" at its end.
static VIDEO_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*[(\[【]\s*(official\s+)?(hd\s+|4k\s+)?(music\s+video|lyric\s+video|video|audio|lyrics?|visuali[sz]er|mv|pv|hd|4k)\s*[)\]】]|\s+official\s+(hd\s+)?(music\s+)?video\s*$",
    )
    .unwrap()
});
/// "Artist「Title」".
static QUOTED_TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.+?)\s*「(.+)」$").unwrap());
/// Words that say a part of a file name is an album.
static ALBUM_WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(soundtrack|ost|original|vol\.?|ep|album)\b").unwrap());

/// The names and albums the library's songs already have (as
/// `fields::group_key`s), to tell which part of a file name is which.
pub struct Known {
    names: HashSet<String>,
    albums: HashSet<String>,
    ignore: String,
}

impl Known {
    pub fn of(store: &SongStore, ignore: &str) -> Self {
        let mut known = Self { names: HashSet::new(), albums: HashSet::new(), ignore: ignore.to_string() };
        for (_, song) in store.known() {
            let facts = Facts::of(song);
            for field in [Field::Artist, Field::AlbumArtist, Field::Composer] {
                known.names.extend(facts.items(field).iter().map(|name| fields::group_key(name, ignore)));
            }
            known.albums.extend(facts.text(Field::Album).map(|album| fields::group_key(&album, ignore)));
        }
        known
    }

    fn key(&self, text: &str) -> String {
        fields::group_key(text, &self.ignore)
    }

    fn is_name(&self, text: &str) -> bool {
        self.names.contains(&self.key(text))
    }

    fn is_album(&self, text: &str) -> bool {
        self.albums.contains(&self.key(text)) || ALBUM_WORDS.is_match(text)
    }
}

/// A file name (without its extension) taken apart: its leading track
/// number, all but that (dashes made one kind), and its parts.
fn split(stem: &str) -> (Option<String>, String, Vec<String>) {
    let mut track = None;
    let mut rest = stem.trim();
    let mut underscored = false;
    if let Some(found) = FILE_NUMBER.captures(rest) {
        track = Some(found[1].to_string());
        underscored = stem[found[1].len()..].trim_start().starts_with('_');
        rest = found.get(2).map_or("", |m| m.as_str());
    }
    let dashed = rest.replace(" – ", " - ").replace(" — ", " - ");
    let mut parts: Vec<String> = dashed.split(" - ").map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect();
    if parts.len() == 1 && underscored {
        // "05_BlueFire_Forest Shrine Depths": only the last is surely the title.
        parts = dashed.rsplit('_').next().map(str::to_string).into_iter().collect();
    }
    // A part that's only a number is the track ("Artist - Album - 14 -
    // Title"), or an index of its own ("003.5 - Title").
    parts.retain(|part| match part.parse::<u32>() {
        Ok(_) if track.is_none() && part.len() <= 3 => {
            track = Some(part.clone());
            false
        }
        Ok(_) => true,
        _ => !part.chars().all(|c| c.is_ascii_digit() || c == '.'),
    });
    (track, dashed, parts)
}

/// What a file name (without its extension) says of its song: its
/// track, artist, album and title, as far as it says. Nothing when it
/// says no more than a title that's the name itself.
pub fn from_file_name(stem: &str, known: &Known) -> Vec<(Field, String)> {
    let (mut track, dashed, parts) = split(stem);
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let (mut artist, mut album) = (None, None);
    let title = match parts.as_slice() {
        [title] => *title,
        [a, b] if known.is_name(b) && !known.is_name(a) => {
            artist = Some(*b);
            *a
        }
        [a, b] if known.is_album(a) && !known.is_name(a) => {
            album = Some(*a);
            *b
        }
        [a, b] => {
            artist = Some(*a);
            *b
        }
        [album_part, title, by] if known.is_name(by) && !known.is_name(title) => {
            (artist, album) = (Some(*by), Some(*album_part));
            *title
        }
        [x, y, title] => {
            let y_is_artist = (known.is_name(y) && !known.is_name(x)) || (known.is_album(x) && !known.is_album(y));
            (artist, album) = if y_is_artist { (Some(*y), Some(*x)) } else { (Some(*x), Some(*y)) };
            *title
        }
        _ => return Vec::new(),
    };
    // "Bird Motif - 2": a title, numbered.
    let title = if title.parse::<u32>().is_ok() {
        (artist, album) = (None, None);
        dashed.as_str()
    } else {
        title
    };
    let mut title = title.to_string();
    if track.is_none()
        && parts.len() > 1
        && let Some((number, rest)) = PART_NUMBER.captures(&title).map(|c| (c[1].to_string(), c[2].trim().to_string()))
    {
        (track, title) = (Some(number), rest);
    }
    if title.is_empty() || (title == stem.trim() && artist.is_none() && album.is_none()) {
        return Vec::new();
    }
    let mut guessed = Vec::new();
    guessed.extend(track.and_then(|t| t.parse::<u32>().ok()).map(|t| (Field::Track, t.to_string())));
    guessed.extend(artist.map(|a| (Field::Artist, a.to_string())));
    guessed.extend(album.map(|a| (Field::Album, a.to_string())));
    guessed.push((Field::Title, title));
    guessed
}

/// A title that starts with its track number ("27. Luke"): the number,
/// and the title without it.
pub fn number_in_title(title: &str) -> Option<(u32, String)> {
    let found = TITLE_NUMBER.captures(title.trim())?;
    Some((found[1].parse().ok()?, found[2].trim().to_string()))
}

/// What a video's title and channel say of a song: its artist, if it
/// says otherwise than `artist` does, and its title without the video's
/// words ("Careless Whisper", by "George Michael", for "George Michael -
/// Careless Whisper (Official Video)" by "georgemichaelVEVO"). Nothing
/// when it's not a video's.
pub fn from_video(artist: Option<&str>, title: &str, known: &Known) -> Option<(Option<String>, String)> {
    let unworded = VIDEO_WORDS.replace_all(title, "");
    let video = unworded.trim() != title.trim();
    let cleaned = unworded.replace(" – ", " - ").replace(" — ", " - ").trim().to_string();
    let channel = artist.and_then(|a| ["VEVO", " - Topic", " Official"].iter().find_map(|end| a.strip_suffix(end)));
    if !video && channel.is_none() {
        return None;
    }
    let artist_key = channel.or(artist).map(|a| known.key(a)).unwrap_or_default();
    // "Artist - Title" and "Artist「Title」", by whoever the channel is.
    let split = cleaned.split_once(" - ").or_else(|| QUOTED_TITLE.captures(&cleaned).map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str())));
    let (new_artist, new_title) = match split {
        Some((by, song)) => {
            let by_key = known.key(by);
            let theirs = !by_key.is_empty() && (artist_key.is_empty() || artist_key == by_key || (by_key.chars().count() >= 3 && artist_key.contains(&by_key)));
            if theirs { (Some(by.trim().to_string()), song.trim().to_string()) } else { (channel.map(|c| c.trim().to_string()), cleaned.clone()) }
        }
        None => (channel.map(|c| c.trim().to_string()), cleaned.clone()),
    };
    let new_artist = new_artist.filter(|a| Some(a.as_str()) != artist && !a.is_empty());
    (new_artist.is_some() || new_title != title.trim()).then_some((new_artist, new_title)).filter(|(_, t)| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str], albums: &[&str]) -> Known {
        Known { names: names.iter().map(|n| fields::group_key(n, "")).collect(), albums: albums.iter().map(|a| fields::group_key(a, "")).collect(), ignore: String::new() }
    }

    fn guess(stem: &str, known: &Known) -> Vec<(Field, String)> {
        from_file_name(stem, known)
    }

    fn want(fields: &[(Field, &str)]) -> Vec<(Field, String)> {
        fields.iter().map(|(f, t)| (*f, t.to_string())).collect()
    }

    #[test]
    fn file_names_say_what_they_can() {
        let k = known(&["Kan Gao"], &[]);
        assert_eq!(guess("177_The Crater", &k), want(&[(Field::Track, "177"), (Field::Title, "The Crater")]));
        assert_eq!(guess("6 - Tale of the Gods", &k), want(&[(Field::Track, "6"), (Field::Title, "Tale of the Gods")]));
        assert_eq!(guess("34. Days - Kan Gao", &k), want(&[(Field::Track, "34"), (Field::Artist, "Kan Gao"), (Field::Title, "Days")]));
        assert_eq!(guess("11. The Guests (Decayed Vers.) - Kan Gao", &k), want(&[(Field::Track, "11"), (Field::Artist, "Kan Gao"), (Field::Title, "The Guests (Decayed Vers.)")]));
        assert_eq!(
            guess("04. This War of Mine Original Soundtrack - Piotr Musiał - These Cold Days", &k),
            want(&[(Field::Track, "4"), (Field::Artist, "Piotr Musiał"), (Field::Album, "This War of Mine Original Soundtrack"), (Field::Title, "These Cold Days")])
        );
        assert_eq!(guess("Artist - Album - 01 Song", &k), want(&[(Field::Track, "1"), (Field::Artist, "Artist"), (Field::Album, "Album"), (Field::Title, "Song")]));
        assert_eq!(guess("05_BlueFire_Forest Shrine Depths", &k), want(&[(Field::Track, "5"), (Field::Title, "Forest Shrine Depths")]));
        assert_eq!(guess("07. Kikuo — 呪術少年", &known(&["Kikuo"], &[])), want(&[(Field::Track, "7"), (Field::Artist, "Kikuo"), (Field::Title, "呪術少年")]));
        // Nothing more than the name itself.
        assert_eq!(guess("Animal Crossing New Leaf - 200 AM", &k), want(&[(Field::Artist, "Animal Crossing New Leaf"), (Field::Title, "200 AM")]));
        assert_eq!(guess("34. Bird Motif - 2", &k), want(&[(Field::Track, "34"), (Field::Title, "Bird Motif - 2")]));
        assert_eq!(guess("003.5 - Your Best Friend", &k), want(&[(Field::Title, "Your Best Friend")]));
        assert_eq!(guess("02. Impostor Factory - Title Theme - Kan Gao", &k), want(&[(Field::Track, "2"), (Field::Artist, "Kan Gao"), (Field::Album, "Impostor Factory"), (Field::Title, "Title Theme")]));
        assert!(guess("Overworld", &k).is_empty());
        assert!(guess("4 PM", &k).is_empty(), "a number with no separator is the title's");
        assert!(guess("13.0.0.0.0", &k).is_empty());
    }

    #[test]
    fn numbers_leave_titles() {
        assert_eq!(number_in_title("27. Luke"), Some((27, "Luke".into())));
        assert_eq!(number_in_title("68. True Ending ~ A New Beginning"), Some((68, "True Ending ~ A New Beginning".into())));
        assert_eq!(number_in_title("4 PM"), None);
        assert_eq!(number_in_title("13.0.0.0.0"), None);
        assert_eq!(number_in_title("Space 5 - Kessler Syndrome"), None);
    }

    #[test]
    fn video_titles_say_who_its_by() {
        let k = known(&[], &[]);
        let video = |artist: &str, title: &str| from_video(Some(artist), title, &k);
        assert_eq!(video("georgemichaelVEVO", "George Michael - Careless Whisper (Official Video)"), Some((Some("George Michael".into()), "Careless Whisper".into())));
        assert_eq!(video("MGMT", "MGMT - Electric Feel (Official HD Video)"), Some((None, "Electric Feel".into())));
        assert_eq!(video("ヨルシカ / n-buna Official", "ヨルシカ - 言って。(Music Video)"), Some((Some("ヨルシカ".into()), "言って。".into())));
        assert_eq!(video("Ayase / YOASOBI", "YOASOBI「アイドル」 Official Music Video"), Some((Some("YOASOBI".into()), "アイドル".into())));
        assert_eq!(video("Michael Wycoff - Topic", "Looking Up to You"), Some((Some("Michael Wycoff".into()), "Looking Up to You".into())));
        assert_eq!(video("Goo Goo Dolls", "Goo Goo Dolls – Iris [Official Music Video]"), Some((None, "Iris".into())));
        assert_eq!(video("Camel", "Never Let Go - Live"), None, "not a video");
        assert_eq!(video("Someone", "Song (Live)"), None);
        assert_eq!(video("David Gilmour", "The Clap (unedited master – Pt. I)"), None, "a dash isn't a video's");
    }
}

