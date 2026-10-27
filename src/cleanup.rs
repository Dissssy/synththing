//! Clean-up suggestions for the library's songs' details, worked out
//! offline from what they say: stray quotes and spacing tidied, several
//! artists in one name split ("\"Perry Porter\"& Nujabes" is Perry
//! Porter and Nujabes), genres split ("Hip-Hop/Rap"), and one name
//! spelled several ways written one way ("Kan R. Gao" and "Kan (Reives)
//! Gao"), and an album by several artists with no album artist marked
//! a compilation. Each is made as songs' edits (`song_store::Song::edits`), so
//! the files are never written, and one that's wrong can be dismissed
//! (`Config::cleanup_dismissed`). Songs that aren't songs at all (macOS's
//! "._" files, imported before they were skipped) are suggested removed.
//! Tags a song's file name, numbered title or video title say
//! (`file_tags`) are suggested for those that lack them, grouped by who
//! and what album they'd be. What MusicBrainz says otherwise of matched
//! songs (`lookup::details`) is offered field by field for each album,
//! and songs far from their track's length are flagged for a look.
//! Songs that are one recording (`duplicates`) are offered as one: the
//! best copy kept, the rest merged onto it and removed.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use unicode_normalization::UnicodeNormalization;

use crate::duplicates;
use crate::fields::{self, Facts, Field};
use crate::file_tags::{self, Known};
use crate::smart::VARIOUS_ARTISTS;
use crate::song_store::{self, SongStore};

/// The fields that hold people's names.
pub const NAME_FIELDS: [Field; 3] = [Field::Artist, Field::AlbumArtist, Field::Composer];

/// The fields whose text is tidied.
const TIDIED: [Field; 6] = [Field::Title, Field::Artist, Field::AlbumArtist, Field::Album, Field::Composer, Field::Genre];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Not songs at all: remove them from the library.
    Junk,
    /// One recording in several files (`duplicates`): keep the best,
    /// remove the others.
    Duplicate,
    /// Songs of one title and artist and about as long: likely one
    /// recording, to look at before keeping one.
    PossibleDuplicate,
    /// Tags from a song's file name, for one with no title.
    FileName,
    /// A title starting with its track number.
    NumberInTitle,
    /// A video's title and channel ("(Official Video)", "...VEVO").
    VideoTitle,
    /// Several artists (or genres) in one: split them.
    Split,
    /// One name spelled several ways: write it one way.
    Same,
    /// An album by several artists, with no album artist: a compilation.
    Compilation,
    /// Stray quotes, brackets or spacing.
    Tidy,
    /// What MusicBrainz says otherwise of a matched album's songs.
    MusicBrainz,
    /// A matched song far from its track's length: another version, or
    /// a bad rip. Only to look at.
    LengthDiffers,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Junk => "Not songs",
            Kind::Duplicate => "Duplicate",
            Kind::PossibleDuplicate => "Possible duplicate",
            Kind::FileName => "From file name",
            Kind::NumberInTitle => "Number in title",
            Kind::VideoTitle => "Video title",
            Kind::Split => "Split",
            Kind::Same => "Same name",
            Kind::Compilation => "Compilation",
            Kind::Tidy => "Tidy",
            Kind::MusicBrainz => "From MusicBrainz",
            Kind::LengthDiffers => "Length differs",
        }
    }
}

/// What a suggestion changes.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// A field's text, as it is on its songs.
    Text { field: Field, text: String },
    /// One name's spellings, wherever a name field has them; and the
    /// MusicBrainz artist (by id) it is, if it's tied to one
    /// (`lookup::artists`).
    Name { spellings: Vec<String>, artist: Option<String> },
    /// An album (as its songs spell it) to mark a compilation.
    Album { name: String },
    /// macOS's AppleDouble files ("._Song.flac"), to remove.
    AppleDouble,
    /// Fields to set on each of its songs (`Suggestion::keys`, in their
    /// order): what one of them said (`from`), and what the group's
    /// known by (who and what album they'd be).
    Fields { from: String, group: String, set: Vec<Vec<(Field, String)>> },
    /// A field as MusicBrainz has it on a release (`album`), for each of
    /// its songs (`Suggestion::keys`, in their order): what it is, and
    /// what it'd be.
    Online { field: Field, album: String, release: String, changes: Vec<(String, String)> },
    /// A song's length (seconds) here and at MusicBrainz.
    Length { title: String, here: f64, there: f64 },
    /// One recording's copies (`Suggestion::keys`, the best first).
    Duplicates { title: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub kind: Kind,
    pub what: What,
    /// What it'd be written as (for a list, its items written as one;
    /// a compilation's is who it'll be by, and can't be changed).
    pub to: String,
    /// Its songs, by key.
    pub keys: Vec<String>,
}

impl Suggestion {
    /// What it's known by once it's dismissed.
    pub fn id(&self, ignore: &str) -> String {
        match &self.what {
            What::Text { field, text } => format!("{}:{text}", field.def().key),
            What::Name { spellings, artist } => {
                let keys: BTreeSet<String> = spellings.iter().map(|s| fields::group_key(s, ignore)).collect();
                let keys = keys.into_iter().collect::<Vec<_>>().join("|");
                match artist {
                    Some(artist) => format!("artist:{artist}:{keys}"),
                    None => format!("name:{keys}"),
                }
            }
            What::Album { name } => format!("compilation:{}", fields::group_key(name, ignore)),
            What::AppleDouble => "apple-double".to_string(),
            What::Fields { group, .. } => format!("{:?}:{group}", self.kind),
            What::Online { field, release, .. } => format!("musicbrainz:{release}:{}", field.def().key),
            What::Length { .. } => format!("length:{}", self.keys.join("|")),
            What::Duplicates { .. } => {
                let keys: BTreeSet<&str> = self.keys.iter().map(String::as_str).collect();
                format!("duplicate:{}", keys.into_iter().collect::<Vec<_>>().join("|"))
            }
        }
    }
}

/// Every suggestion for `store`'s songs (names grouped as by
/// `fields::group_key`, past `ignore`), junk first, then splits.
pub fn suggest(store: &SongStore, ignore: &str) -> Vec<Suggestion> {
    let junk: Vec<String> =
        store.known().filter(|(key, song)| !store.is_blocked(key) && song_store::is_apple_double(&song.name)).map(|(key, _)| key.to_string()).collect();
    let mut texts: BTreeMap<(Field, String), Suggestion> = BTreeMap::new();
    // Each name's spellings (by group key), and how many songs each is on.
    let mut names: BTreeMap<String, (BTreeMap<String, usize>, BTreeSet<&str>)> = BTreeMap::new();
    let mut albums: BTreeMap<String, Album> = BTreeMap::new();
    let known = Known::of(store, ignore);
    let mut guessed: BTreeMap<(Kind, String), Suggestion> = BTreeMap::new();
    for (key, song) in store.known().filter(|(_, song)| !song_store::is_apple_double(&song.name)) {
        let facts = Facts::of(song);
        if let Some((kind, from, set)) = guess(&facts, &known) {
            let named = |field| set.iter().find(|(f, _)| *f == field).map(|(_, t)| Cow::Borrowed(t.as_str())).or_else(|| facts.text(field));
            let shape: Vec<&str> = set.iter().map(|(f, _)| f.def().key).collect();
            let group = [named(Field::Artist), named(Field::Album)].map(|n| n.map(|n| fields::group_key(&n, ignore)).unwrap_or_default());
            let group = format!("{}|{}|{}", shape.join(","), group[0], group[1]);
            let suggestion = guessed.entry((kind, group.clone())).or_insert_with(|| Suggestion {
                kind,
                what: What::Fields { from, group, set: Vec::new() },
                to: describe_set(&set),
                keys: Vec::new(),
            });
            if let What::Fields { set: sets, .. } = &mut suggestion.what {
                sets.push(set);
            }
            suggestion.keys.push(key.to_string());
        }
        if let Some(name) = facts.text(Field::Album) {
            let album = albums.entry(fields::group_key(&name, ignore)).or_default();
            album.artists.extend(facts.first(Field::Artist).map(|a| fields::group_key(&a, ignore)));
            album.album_artist |= facts.text(Field::AlbumArtist).is_some();
            if !facts.flag(Field::Compilation) {
                album.unmarked.push(key.to_string());
            }
            album.spellings.push(name.into_owned());
        }
        for field in TIDIED {
            let Some(text) = facts.text(field) else { continue };
            let items = facts.items(field);
            let cleaned: Vec<String> = items.iter().flat_map(|item| split(field, item)).map(|i| tidy(&i)).filter(|i| !i.is_empty()).collect();
            if cleaned.iter().map(String::as_str).eq(items.iter().map(Cow::as_ref)) || cleaned.is_empty() {
                continue;
            }
            let kind = if cleaned.len() > items.len() { Kind::Split } else { Kind::Tidy };
            let to = field.def().join(&cleaned);
            let suggestion = texts.entry((field, text.to_string())).or_insert_with(|| Suggestion {
                kind,
                what: What::Text { field, text: text.to_string() },
                to,
                keys: Vec::new(),
            });
            suggestion.keys.push(key.to_string());
        }
        for field in NAME_FIELDS {
            for name in facts.items(field) {
                let (spellings, keys) = names.entry(fields::group_key(&name, ignore)).or_default();
                if keys.insert(key) {
                    *spellings.entry(name.into_owned()).or_default() += 1;
                }
            }
        }
    }
    let mut suggestions: Vec<Suggestion> = texts.into_values().chain(guessed.into_values()).collect();
    if !junk.is_empty() {
        suggestions.push(Suggestion { kind: Kind::Junk, what: What::AppleDouble, to: String::new(), keys: junk });
    }
    suggestions.extend(duplicates::groups(store, ignore).into_iter().map(|group| Suggestion {
        kind: if group.sure { Kind::Duplicate } else { Kind::PossibleDuplicate },
        what: What::Duplicates { title: group.title },
        to: String::new(),
        keys: group.keys,
    }));
    suggestions.extend(same_names(&names));
    suggestions.extend(albums.into_values().filter(|a| a.artists.len() > 1 && !a.album_artist && !a.unmarked.is_empty()).map(|album| Suggestion {
        kind: Kind::Compilation,
        what: What::Album { name: fields::best_spelling(album.spellings.iter().map(String::as_str)).unwrap_or_default().to_string() },
        to: VARIOUS_ARTISTS.to_string(),
        keys: album.unmarked,
    }));
    suggestions.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| b.keys.len().cmp(&a.keys.len())));
    suggestions
}

/// The tags `facts`' song lacks that its file name, title or video title
/// say: of which kind, what said it, and the fields to set.
fn guess(facts: &Facts, known: &Known) -> Option<(Kind, String, Vec<(Field, String)>)> {
    let lacks = |field| match field {
        Field::Track => facts.number(field).is_none(),
        _ => facts.text(field).is_none(),
    };
    let Some(title) = facts.text(Field::Title) else {
        let stem = std::path::Path::new(facts.file_name).file_stem()?.to_str()?;
        let set: Vec<(Field, String)> = file_tags::from_file_name(stem, known).into_iter().filter(|(f, _)| lacks(*f)).collect();
        return set.iter().any(|(f, _)| *f == Field::Title).then(|| (Kind::FileName, facts.file_name.to_string(), set));
    };
    if let Some((track, rest)) = file_tags::number_in_title(&title) {
        let mut set = vec![(Field::Title, rest)];
        if lacks(Field::Track) {
            set.insert(0, (Field::Track, track.to_string()));
        }
        return Some((Kind::NumberInTitle, title.into_owned(), set));
    }
    let (artist, rest) = file_tags::from_video(facts.text(Field::Artist).as_deref(), &title, known)?;
    let mut set: Vec<(Field, String)> = artist.map(|a| (Field::Artist, a)).into_iter().collect();
    if rest != title {
        set.push((Field::Title, rest));
    }
    Some((Kind::VideoTitle, title.into_owned(), set))
}

/// What MusicBrainz says of a matched song (`lookup::details`).
pub struct Offered {
    pub key: String,
    pub title: String,
    pub release: String,
    /// Its release's title.
    pub album: String,
    /// Its fields that MusicBrainz has otherwise: what they are, and what
    /// MusicBrainz says.
    pub differs: Vec<(Field, String, String)>,
    /// Its length, and its track's (seconds).
    pub length: Option<(f64, f64)>,
}

/// Matched songs' fields that MusicBrainz has otherwise, a suggestion for
/// each field of each release; and those more than `leeway` seconds from
/// their track's length, one for each song.
pub fn from_musicbrainz(offered: Vec<Offered>, leeway: f64) -> Vec<Suggestion> {
    let mut by_field: BTreeMap<(String, Field), Suggestion> = BTreeMap::new();
    let mut lengths = Vec::new();
    for song in offered {
        for (field, now, to) in song.differs {
            let suggestion = by_field.entry((song.release.clone(), field)).or_insert_with(|| Suggestion {
                kind: Kind::MusicBrainz,
                what: What::Online { field, album: song.album.clone(), release: song.release.clone(), changes: Vec::new() },
                to: String::new(),
                keys: Vec::new(),
            });
            if let What::Online { changes, .. } = &mut suggestion.what {
                changes.push((now, to));
            }
            suggestion.keys.push(song.key.clone());
        }
        if let Some((here, there)) = song.length
            && (here - there).abs() > leeway
        {
            lengths.push(Suggestion {
                kind: Kind::LengthDiffers,
                what: What::Length { title: song.title, here, there },
                to: format!("{} here, {} at MusicBrainz", crate::song_info::format_length(here), crate::song_info::format_length(there)),
                keys: vec![song.key],
            });
        }
    }
    let mut suggestions: Vec<Suggestion> = by_field.into_values().collect();
    for suggestion in &mut suggestions {
        if let What::Online { changes, .. } = &suggestion.what {
            // (One value for the whole album, or the first song's.)
            let (now, to) = &changes[0];
            suggestion.to =
                if changes.iter().all(|(_, t)| t == to) || changes.len() == 1 { format!("“{to}”") } else { format!("“{to}” (was “{now}”)") };
        }
    }
    suggestions.extend(lengths);
    suggestions
}

/// What a song's fields will be set to, as one line: "Track 34, Title
/// “Days”, Artist “Kan Gao”".
fn describe_set(set: &[(Field, String)]) -> String {
    set.iter()
        .map(|(field, text)| match field.kind() {
            fields::Kind::Number => format!("{} {text}", field.label()),
            _ => format!("{} “{text}”", field.label()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What's known of an album, for whether it's a compilation.
#[derive(Default)]
struct Album {
    spellings: Vec<String>,
    /// Its songs' (first) artists, by group key.
    artists: BTreeSet<String>,
    /// A song of it says its album artist.
    album_artist: bool,
    /// Its songs not marked as on a compilation.
    unmarked: Vec<String>,
}

/// Names that look like one name spelled several ways: as many words,
/// each the same or one the other's initial ("Kan R. Gao", "Kan (Reives)
/// Gao"), with a whole word in common.
fn same_names(names: &BTreeMap<String, (BTreeMap<String, usize>, BTreeSet<&str>)>) -> Vec<Suggestion> {
    // (Only names with the same initials can be alike.)
    let mut by_initials: BTreeMap<String, Vec<(&String, Vec<String>)>> = BTreeMap::new();
    for (group, (spellings, _)) in names {
        let Some(spelling) = spellings.keys().next() else { continue };
        let words = words(spelling);
        let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
        by_initials.entry(initials).or_default().push((group, words));
    }
    let mut suggestions = Vec::new();
    for alike in by_initials.values().filter(|a| a.len() > 1) {
        // Joined into sets: (A, B) and (B, C) are one name.
        let mut set: Vec<usize> = (0..alike.len()).collect();
        for i in 0..alike.len() {
            for j in i + 1..alike.len() {
                if same_words(&alike[i].1, &alike[j].1) {
                    let (a, b) = (root(&mut set, i), root(&mut set, j));
                    set[a] = b;
                }
            }
        }
        let mut sets: BTreeMap<usize, Vec<&String>> = BTreeMap::new();
        for i in 0..alike.len() {
            let r = root(&mut set, i);
            sets.entry(r).or_default().push(alike[i].0);
        }
        for groups in sets.into_values().filter(|g| g.len() > 1) {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            let mut keys = BTreeSet::new();
            for group in &groups {
                let (spellings, songs) = &names[*group];
                for (spelling, n) in spellings {
                    *counts.entry(spelling).or_default() += n;
                }
                keys.extend(songs.iter().map(|k| k.to_string()));
            }
            // The spelling on the most songs, then the fullest.
            let to = counts.iter().max_by_key(|(s, n)| (**n, s.len())).map(|(s, _)| s.to_string()).unwrap_or_default();
            suggestions.push(Suggestion {
                kind: Kind::Same,
                what: What::Name { spellings: counts.keys().map(|s| s.to_string()).collect(), artist: None },
                to,
                keys: keys.into_iter().collect(),
            });
        }
    }
    suggestions
}

/// The set `i` is in, of those `set` joins (each its parent, a set's
/// root its own).
fn root(set: &mut [usize], i: usize) -> usize {
    if set[i] != i {
        set[i] = root(set, set[i]);
    }
    set[i]
}

/// A name's words, in lowercase, without punctuation.
fn words(name: &str) -> Vec<String> {
    let name: String = name.nfkc().flat_map(char::to_lowercase).collect();
    name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn same_words(a: &[String], b: &[String]) -> bool {
    let initial = |short: &str, long: &str| short.chars().count() == 1 && long.starts_with(short);
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| x == y || initial(x, y) || initial(y, x))
        && a.iter().zip(b).any(|(x, y)| x == y && x.chars().count() > 1)
        && a != b
}

/// What a field's item could be split into: a name's (artist, album
/// artist or composer) at "&", ",", " x " and "feat." (and "ft.", "featuring", bracketed or
/// not), a genre's at "/" and " & ".
fn split(field: Field, item: &str) -> Vec<String> {
    const NAMES: &[&str] = &[
        "(featuring ",
        "[featuring ",
        "(feat. ",
        "[feat. ",
        "(feat ",
        "[feat ",
        "(ft. ",
        "[ft. ",
        "(ft ",
        "[ft ",
        " featuring ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " x ",
        "&",
        ",",
    ];
    const GENRES: &[&str] = &["/", " & "];
    let at = match field {
        Field::Artist | Field::AlbumArtist | Field::Composer => NAMES,
        Field::Genre => GENRES,
        _ => return vec![item.to_string()],
    };
    // (The separators are ASCII, so the lowercase copy's indices are
    // the item's.)
    let lower = item.to_ascii_lowercase();
    let mut parts = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < lower.len() {
        match at.iter().filter(|s| lower[i..].starts_with(*s)).map(|s| s.len()).max() {
            Some(len) => {
                parts.push(item[start..i].to_string());
                i += len;
                start = i;
            }
            None => i += lower[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    parts.push(item[start..].to_string());
    parts
}

/// `text` without stray quotes or brackets, or extra spacing.
fn tidy(text: &str) -> String {
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        let before = text.clone();
        for (open, close) in [('"', '"'), ('“', '”'), ('‘', '’'), ('\'', '\''), ('「', '」')] {
            if text.chars().count() > 1 && text.starts_with(open) && text.ends_with(close) {
                text = text[open.len_utf8()..text.len() - close.len_utf8()].to_string();
            }
        }
        // An odd quote at either end is a stray.
        if text.matches('"').count() % 2 == 1 {
            text = text.trim_matches('"').to_string();
        }
        for (open, close) in [('“', '”'), ('(', ')'), ('[', ']')] {
            let (opens, closes) = (text.matches(open).count(), text.matches(close).count());
            if opens > closes {
                text = text.trim_start_matches(open).trim_end_matches(open).to_string();
            } else if closes > opens {
                text = text.trim_start_matches(close).trim_end_matches(close).to_string();
            }
        }
        text = text.trim().to_string();
        if text == before {
            return text;
        }
    }
}

/// The edits that make the `chosen` suggestions (each with what it's to
/// be written as) on `store`'s songs, as `SongStore::edit` takes them:
/// one after the other, so a name split and then written one way is both.
pub fn edits(store: &SongStore, chosen: &[(&Suggestion, &str)], ignore: &str) -> Vec<(String, Field, Option<String>)> {
    let mut written: BTreeMap<(String, Field), String> = BTreeMap::new();
    for (suggestion, to) in chosen {
        let to = to.trim();
        for key in &suggestion.keys {
            let Some(song) = store.get_by_key(key) else { continue };
            let facts = Facts::of(song);
            let now = |written: &BTreeMap<(String, Field), String>, field| {
                written.get(&(key.clone(), field)).cloned().or_else(|| facts.text(field).map(Cow::into_owned))
            };
            match &suggestion.what {
                // (Removed, not edited: `App::remove_songs`; merged and
                // removed: `App::merge_duplicates`; or only to look at.)
                What::AppleDouble | What::Duplicates { .. } | What::Length { .. } => {}
                What::Online { field, changes, .. } => {
                    if let Some((_, to)) = suggestion.keys.iter().position(|k| k == key).map(|i| &changes[i]) {
                        written.insert((key.clone(), *field), to.clone());
                    }
                }
                What::Album { .. } => {
                    written.insert((key.clone(), Field::Compilation), "1".to_string());
                }
                What::Fields { set, .. } => {
                    let at = suggestion.keys.iter().position(|k| k == key);
                    for (field, text) in at.map(|i| set[i].as_slice()).unwrap_or_default() {
                        written.insert((key.clone(), *field), text.clone());
                    }
                }
                What::Text { field, text } => {
                    if now(&written, *field).as_deref() == Some(text) {
                        written.insert((key.clone(), *field), to.to_string());
                    }
                }
                What::Name { spellings, .. } => {
                    let groups: BTreeSet<String> = spellings.iter().map(|s| fields::group_key(s, ignore)).collect();
                    for field in NAME_FIELDS {
                        let Some(text) = now(&written, field) else { continue };
                        let def = field.def();
                        let items: Vec<&str> = def.split(&text).collect();
                        if !items.iter().any(|i| groups.contains(&fields::group_key(i, ignore))) {
                            continue;
                        }
                        let mut renamed: Vec<&str> = Vec::new();
                        for item in items {
                            let item = if groups.contains(&fields::group_key(item, ignore)) { to } else { item };
                            if !renamed.contains(&item) {
                                renamed.push(item);
                            }
                        }
                        written.insert((key.clone(), field), def.join(&renamed));
                    }
                }
            }
        }
    }
    written
        .into_iter()
        .filter_map(|((key, field), text)| {
            let facts = Facts::of(store.get_by_key(&key)?);
            // (Back to what the file says is no edit at all.)
            let edit = (facts.unedited().text(field).as_deref() != Some(text.as_str())).then_some(text);
            Some((key, field, edit))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::{Meta, Song};

    fn store(name: &str, songs: &[Meta]) -> SongStore {
        let dir = std::env::temp_dir().join(format!("synththing-cleanup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir);
        store.learn(
            songs
                .iter()
                .enumerate()
                .map(|(i, meta)| (format!("{:064x}.ogg", i + 1), Song { name: format!("{i}.ogg"), meta: Some(meta.clone()), ..Song::default() })),
        );
        store
    }

    #[test]
    fn names_split_and_tidy() {
        assert_eq!(split(Field::Artist, "\"Perry Porter\"& Nujabes"), ["\"Perry Porter\"", " Nujabes"]);
        assert_eq!(split(Field::Artist, "A feat. B, C"), ["A", "B", " C"]);
        assert_eq!(split(Field::Artist, "A (Feat. B)"), ["A ", "B)"]);
        assert_eq!(split(Field::Artist, "Lil Nas X"), ["Lil Nas X"]);
        assert_eq!(split(Field::Artist, "Tom x Jerry"), ["Tom", "Jerry"]);
        assert_eq!(split(Field::Genre, "Hip-Hop/Rap"), ["Hip-Hop", "Rap"]);
        assert_eq!(split(Field::Genre, "R&B"), ["R&B"]);
        assert_eq!(split(Field::Title, "A & B"), ["A & B"]);
        assert_eq!(split(Field::AlbumArtist, "Uyama Hiroto & Nujabes"), ["Uyama Hiroto ", " Nujabes"]);
        for (text, tidied) in [
            ("\"Perry Porter\"", "Perry Porter"),
            (" Nujabes", "Nujabes"),
            ("B)", "B"),
            ("Perry  Porter\"", "Perry Porter"),
            ("“Song”", "Song"),
            ("Guns N' Roses", "Guns N' Roses"),
            ("Song (Live)", "Song (Live)"),
        ] {
            assert_eq!(tidy(text), tidied, "{text}");
        }
    }

    #[test]
    fn nujams_is_split() {
        let album = Some("Tribute To Nujabes... \"NuJams\"".to_string());
        let song = Meta {
            title: Some("Middle-Horn Shuffle".into()),
            artist: Some("\"Perry Porter\"& Nujabes".into()),
            composer: Some("Nujabes".into()),
            album: album.clone(),
            ..Meta::default()
        };
        let other = Meta { title: Some("Another".into()), artist: Some("Substantial".into()), album, ..Meta::default() };
        let mut store = store("nujams", &[song, other]);
        let suggestions = suggest(&store, "");
        let kinds: Vec<Kind> = suggestions.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Split, Kind::Compilation], "{suggestions:?}");
        let split = &suggestions[0];
        assert_eq!(split.to, "Perry Porter; Nujabes");
        let compilation = &suggestions[1];
        assert_eq!(compilation.what, What::Album { name: "Tribute To Nujabes... \"NuJams\"".into() });
        store.edit(edits(&store, &[(split, &split.to), (compilation, &compilation.to)], ""));
        let facts: Vec<Facts> = store.known().map(|(_, s)| Facts::of(s)).collect();
        assert_eq!(facts[0].items(Field::Artist), ["Perry Porter", "Nujabes"]);
        assert!(facts.iter().all(|f| f.flag(Field::Compilation)));
        assert!(suggest(&store, "").is_empty(), "nothing more");
    }

    #[test]
    fn file_names_numbers_and_videos_fill_in_tags() {
        let mut store = store("guessed", &[]);
        let song = |i: usize, name: &str, meta: Meta| (format!("{i:064x}.mp3"), Song { name: name.into(), meta: Some(meta), ..Song::default() });
        let video =
            Meta { artist: Some("georgemichaelVEVO".into()), title: Some("George Michael - Careless Whisper (Official Video)".into()), ..Meta::default() };
        store.learn([
            song(1, "34. Days - Kan Gao.mp3", Meta::default()),
            song(2, "35. Back Home - Kan Gao.mp3", Meta { track: Some(35), ..Meta::default() }),
            song(3, "x.mp3", Meta { title: Some("27. Luke".into()), track: Some(27), ..Meta::default() }),
            song(4, "y.mp3", video),
            song(5, "Overworld.mp3", Meta::default()),
            song(6, "z.mp3", Meta { title: Some("Lullaby".into()), artist: Some("Kan Gao".into()), ..Meta::default() }),
        ]);
        let suggestions = suggest(&store, "");
        let kinds: Vec<(Kind, usize)> = suggestions.iter().map(|s| (s.kind, s.keys.len())).collect();
        // (Song 2 has its track, so it's another group: what's set differs.)
        assert_eq!(kinds, [(Kind::FileName, 1), (Kind::FileName, 1), (Kind::NumberInTitle, 1), (Kind::VideoTitle, 1)], "{suggestions:?}");
        let chosen: Vec<(&Suggestion, &str)> = suggestions.iter().map(|s| (s, s.to.as_str())).collect();
        store.edit(edits(&store, &chosen, ""));
        let facts = |i: usize| Facts::of(store.get_by_key(&format!("{i:064x}.mp3")).unwrap()).value(Field::Title).map(|v| v.text);
        assert_eq!([facts(1), facts(2), facts(3), facts(4)], ["Days", "Back Home", "Luke", "Careless Whisper"].map(|t| Some(t.to_string())));
        let days = Facts::of(store.get_by_key(&format!("{:064x}.mp3", 1)).unwrap());
        assert_eq!((days.number(Field::Track), days.text(Field::Artist).as_deref()), (Some(34.0), Some("Kan Gao")));
        assert!(suggest(&store, "").is_empty(), "nothing more: {:?}", suggest(&store, ""));
    }

    #[test]
    fn apple_double_files_are_junk() {
        let mut store = store("junk", &[]);
        let junk = format!("{:064x}.flac", 1);
        let meta = Some(Meta { title: Some("\"Song\"".into()), ..Meta::default() });
        store.learn([
            (junk.clone(), Song { name: "._Song.flac".into(), meta: meta.clone(), ..Song::default() }),
            (format!("{:064x}.flac", 2), Song { name: "Song.flac".into(), ..Song::default() }),
        ]);
        let suggestions = suggest(&store, "");
        assert_eq!(suggestions.len(), 1, "only the junk, not its details: {suggestions:?}");
        assert_eq!((suggestions[0].kind, &suggestions[0].keys), (Kind::Junk, &vec![junk.clone()]));
        store.remove(&store.path(&junk)).unwrap();
        assert!(suggest(&store, "").is_empty(), "gone once removed");
    }

    #[test]
    fn a_name_spelled_two_ways_is_one() {
        let by = |artist: &str| Meta { artist: Some(artist.into()), ..Meta::default() };
        let mut store = store("same", &[by("Kan R. Gao"), by("Kan (Reives) Gao"), by("Kan (Reives) Gao"), by("Kevin R. Gao"), by("K. R. G.")]);
        let suggestions = suggest(&store, "");
        let same: Vec<_> = suggestions.iter().filter(|s| s.kind == Kind::Same).collect();
        assert_eq!(same.len(), 1, "{suggestions:?}");
        assert_eq!(same[0].to, "Kan (Reives) Gao", "the most common");
        assert_eq!(same[0].what, What::Name { spellings: vec!["Kan (Reives) Gao".into(), "Kan R. Gao".into()], artist: None });
        store.edit(edits(&store, &[(same[0], &same[0].to)], ""));
        let artists: BTreeSet<String> = store.known().filter_map(|(_, s)| Facts::of(s).text(Field::Artist).map(Cow::into_owned)).collect();
        assert_eq!(artists, ["K. R. G.", "Kan (Reives) Gao", "Kevin R. Gao"].map(String::from).into());
    }

    #[test]
    fn suggestions_made_together_build_on_each_other() {
        let song = Meta { artist: Some("Kan R. Gao & Someone".into()), ..Meta::default() };
        let other = Meta { artist: Some("Kan (Reives) Gao".into()), ..Meta::default() };
        let store = store("together", &[song, other]);
        let split = suggest(&store, "").into_iter().find(|s| s.kind == Kind::Split).unwrap();
        let same = Suggestion {
            kind: Kind::Same,
            what: What::Name { spellings: vec!["Kan R. Gao".into()], artist: None },
            to: "Kan (Reives) Gao".into(),
            keys: split.keys.clone(),
        };
        let edits = edits(&store, &[(&split, &split.to), (&same, &same.to)], "");
        assert_eq!(edits, [(split.keys[0].clone(), Field::Artist, Some("Kan (Reives) Gao; Someone".into()))]);
    }

    #[test]
    fn musicbrainz_offers_by_album_and_field() {
        let meta = |title: &str| Meta { title: Some(title.into()), album: Some("Modal Soul".into()), ..Meta::default() };
        let mut store = store("musicbrainz", &[meta("feather "), meta("Reflection Eternal")]);
        let keys: Vec<String> = store.known().map(|(k, _)| k.to_string()).collect();
        let offered = |i: usize, title: &str, length| Offered {
            key: keys[i].clone(),
            title: title.into(),
            release: "r1".into(),
            album: "Modal Soul".into(),
            differs: vec![(Field::Title, title.into(), format!("{title}!"))],
            length,
        };
        let suggestions = from_musicbrainz(vec![offered(0, "feather", Some((587.0, 660.0))), offered(1, "Reflection Eternal", Some((280.0, 284.0)))], 10.0);
        let kinds: Vec<(Kind, usize)> = suggestions.iter().map(|s| (s.kind, s.keys.len())).collect();
        assert_eq!(kinds, [(Kind::MusicBrainz, 2), (Kind::LengthDiffers, 1)], "{suggestions:?}");
        assert_eq!(suggestions[1].to, "9:47 here, 11:00 at MusicBrainz");
        store.edit(edits(&store, &[(&suggestions[0], &suggestions[0].to), (&suggestions[1], &suggestions[1].to)], ""));
        let titles: Vec<String> = store.known().filter_map(|(_, s)| Facts::of(s).text(Field::Title).map(Cow::into_owned)).collect();
        assert_eq!(titles, ["feather!", "Reflection Eternal!"]);
    }
}
