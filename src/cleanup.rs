//! Clean-up suggestions for the library's songs' details, worked out
//! offline from what they say: stray quotes and spacing tidied, several
//! artists in one name split ("\"Perry Porter\"& Nujabes" is Perry
//! Porter and Nujabes), genres split ("Hip-Hop/Rap"), and one name
//! spelled several ways written one way ("Kan R. Gao" and "Kan (Reives)
//! Gao"). Each is made as songs' edits (`song_store::Song::edits`), so
//! the files are never written, and one that's wrong can be dismissed
//! (`Config::cleanup_dismissed`).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use unicode_normalization::UnicodeNormalization;

use crate::fields::{self, Facts, Field};
use crate::song_store::SongStore;

/// The fields that hold people's names.
const NAME_FIELDS: [Field; 3] = [Field::Artist, Field::AlbumArtist, Field::Composer];

/// The fields whose text is tidied.
const TIDIED: [Field; 6] = [Field::Title, Field::Artist, Field::AlbumArtist, Field::Album, Field::Composer, Field::Genre];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Several artists (or genres) in one: split them.
    Split,
    /// One name spelled several ways: write it one way.
    Same,
    /// Stray quotes, brackets or spacing.
    Tidy,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Split => "Split",
            Kind::Same => "Same name",
            Kind::Tidy => "Tidy",
        }
    }
}

/// What a suggestion changes.
#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// A field's text, as it is on its songs.
    Text { field: Field, text: String },
    /// One name's spellings, wherever a name field has them.
    Name { spellings: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub kind: Kind,
    pub what: What,
    /// What it'd be written as (for a list, its items written as one).
    pub to: String,
    /// Its songs, by key.
    pub keys: Vec<String>,
}

impl Suggestion {
    /// What it's known by once it's dismissed.
    pub fn id(&self, ignore: &str) -> String {
        match &self.what {
            What::Text { field, text } => format!("{}:{text}", field.def().key),
            What::Name { spellings } => {
                let keys: BTreeSet<String> = spellings.iter().map(|s| fields::group_key(s, ignore)).collect();
                format!("name:{}", keys.into_iter().collect::<Vec<_>>().join("|"))
            }
        }
    }
}

/// Every suggestion for `store`'s songs (names grouped as by
/// `fields::group_key`, past `ignore`), splits first.
pub fn suggest(store: &SongStore, ignore: &str) -> Vec<Suggestion> {
    let mut texts: BTreeMap<(Field, String), Suggestion> = BTreeMap::new();
    // Each name's spellings (by group key), and how many songs each is on.
    let mut names: BTreeMap<String, (BTreeMap<String, usize>, BTreeSet<&str>)> = BTreeMap::new();
    for (key, song) in store.known() {
        let facts = Facts::of(song);
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
    let mut suggestions: Vec<Suggestion> = texts.into_values().collect();
    suggestions.extend(same_names(&names));
    suggestions.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| b.keys.len().cmp(&a.keys.len())));
    suggestions
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
                what: What::Name { spellings: counts.keys().map(|s| s.to_string()).collect() },
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

/// What a field's item could be split into: an artist's or composer's
/// at "&", ",", " x " and "feat." (and "ft.", "featuring", bracketed or
/// not), a genre's at "/" and " & ".
fn split(field: Field, item: &str) -> Vec<String> {
    const NAMES: &[&str] = &[
        "(featuring ", "[featuring ", "(feat. ", "[feat. ", "(feat ", "[feat ", "(ft. ", "[ft. ", "(ft ", "[ft ",
        " featuring ", " feat. ", " feat ", " ft. ", " ft ", " x ", "&", ",",
    ];
    const GENRES: &[&str] = &["/", " & "];
    let at = match field {
        Field::Artist | Field::Composer => NAMES,
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
                What::Text { field, text } => {
                    if now(&written, *field).as_deref() == Some(text) {
                        written.insert((key.clone(), *field), to.to_string());
                    }
                }
                What::Name { spellings } => {
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
        store.learn(songs.iter().enumerate().map(|(i, meta)| {
            (format!("{:064x}.ogg", i + 1), Song { name: format!("{i}.ogg"), meta: Some(meta.clone()), ..Song::default() })
        }));
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
        for (text, tidied) in [("\"Perry Porter\"", "Perry Porter"), (" Nujabes", "Nujabes"), ("B)", "B"), ("Perry  Porter\"", "Perry Porter"), ("“Song”", "Song"), ("Guns N' Roses", "Guns N' Roses"), ("Song (Live)", "Song (Live)")] {
            assert_eq!(tidy(text), tidied, "{text}");
        }
    }

    #[test]
    fn nujams_is_split() {
        let song = Meta { title: Some("Middle-Horn Shuffle".into()), artist: Some("\"Perry Porter\"& Nujabes".into()), composer: Some("Nujabes".into()), album: Some("Tribute To Nujabes... \"NuJams\"".into()), ..Meta::default() };
        let mut store = store("nujams", &[song]);
        let suggestions = suggest(&store, "");
        assert_eq!(suggestions.len(), 1, "{suggestions:?}");
        let split = &suggestions[0];
        assert_eq!((split.kind, split.to.as_str()), (Kind::Split, "Perry Porter; Nujabes"));
        let edits = edits(&store, &[(split, &split.to)], "");
        store.edit(edits);
        let facts = Facts::of(store.known().next().unwrap().1);
        assert_eq!(facts.items(Field::Artist), ["Perry Porter", "Nujabes"]);
        assert!(suggest(&store, "").is_empty(), "nothing more");
    }

    #[test]
    fn a_name_spelled_two_ways_is_one() {
        let by = |artist: &str| Meta { artist: Some(artist.into()), ..Meta::default() };
        let mut store = store("same", &[by("Kan R. Gao"), by("Kan (Reives) Gao"), by("Kan (Reives) Gao"), by("Kevin R. Gao"), by("K. R. G.")]);
        let suggestions = suggest(&store, "");
        let same: Vec<_> = suggestions.iter().filter(|s| s.kind == Kind::Same).collect();
        assert_eq!(same.len(), 1, "{suggestions:?}");
        assert_eq!(same[0].to, "Kan (Reives) Gao", "the most common");
        assert_eq!(same[0].what, What::Name { spellings: vec!["Kan (Reives) Gao".into(), "Kan R. Gao".into()] });
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
        let same = Suggestion { kind: Kind::Same, what: What::Name { spellings: vec!["Kan R. Gao".into()] }, to: "Kan (Reives) Gao".into(), keys: split.keys.clone() };
        let edits = edits(&store, &[(&split, &split.to), (&same, &same.to)], "");
        assert_eq!(edits, [(split.keys[0].clone(), Field::Artist, Some("Kan (Reives) Gao; Someone".into()))]);
    }
}
