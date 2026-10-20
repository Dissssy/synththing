//! The rules engine: smart playlists and rulesets, one definition for both
//! ([`Rules`]). A definition says which songs match (rules on what the
//! synced library knows of them: `song_store::Song`, all of them or any),
//! how they're ordered, how many are kept (`Limit`), and, for a ruleset,
//! what splits them into a playlist each (`Split`: one per artist, album,
//! genre...).
//!
//! Only definitions are kept and synced (a smart playlist's in its
//! playlist, `playlist.rs`; rulesets in the settings, `Config::rulesets`):
//! every device works out the songs from its own copy of the library, so
//! they're the same everywhere. Random order is seeded: songs go by
//! `sha256(seed, key)`, the same on every device, and a song coming in
//! doesn't reshuffle the rest. Re-seeding writes a new seed, an ordinary
//! edit.
//!
//! The built-in rulesets, By Artist and By Album ([`builtin`]), are
//! made here, not kept: they can't be removed.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::song_store::{self, Song, SongStore};

/// A smart playlist's (or ruleset's) definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    /// Match any rule (else all of them).
    pub any: bool,
    pub rules: Vec<Rule>,
    pub limit: Option<Limit>,
    /// The order the songs are listed in (a limit picks in its own).
    pub order: Order,
    /// Seeds `Order::Random`.
    pub seed: u64,
    /// Makes it a ruleset: a playlist for each value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<Split>,
}

impl Default for Rules {
    fn default() -> Self {
        Self { any: false, rules: Vec::new(), limit: None, order: Order::Title, seed: new_seed(), split: None }
    }
}

/// A rule: a field, how it's compared, and what with (as typed: parsed
/// for the field when evaluated).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub field: Field,
    pub op: Op,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// The range's other end (`Op::InRange`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
}

impl Rule {
    pub fn new(field: Field) -> Self {
        Self { field, op: field.kind().ops()[0], value: String::new(), to: String::new() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Composer,
    Genre,
    FileName,
    Year,
    Track,
    Disc,
    /// Seconds (typed as `m:ss` or seconds).
    Length,
    /// MB.
    Size,
    Imported,
    Tag,
    Starred,
}

/// What a field's values are, which says how they can be compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    Date,
    Tag,
    Flag,
}

impl Field {
    pub const ALL: [Self; 15] = [
        Self::Title,
        Self::Artist,
        Self::AlbumArtist,
        Self::Album,
        Self::Composer,
        Self::Genre,
        Self::FileName,
        Self::Year,
        Self::Track,
        Self::Disc,
        Self::Length,
        Self::Size,
        Self::Imported,
        Self::Tag,
        Self::Starred,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::AlbumArtist => "Album artist",
            Self::Album => "Album",
            Self::Composer => "Composer",
            Self::Genre => "Genre",
            Self::FileName => "File name",
            Self::Year => "Year",
            Self::Track => "Track",
            Self::Disc => "Disc",
            Self::Length => "Length",
            Self::Size => "Size (MB)",
            Self::Imported => "Date added",
            Self::Tag => "Tag",
            Self::Starred => "Starred",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::Title | Self::Artist | Self::AlbumArtist | Self::Album | Self::Composer | Self::Genre | Self::FileName => Kind::Text,
            Self::Year | Self::Track | Self::Disc | Self::Length | Self::Size => Kind::Number,
            Self::Imported => Kind::Date,
            Self::Tag => Kind::Tag,
            Self::Starred => Kind::Flag,
        }
    }

    fn text<'a>(self, song: &'a Song) -> Option<&'a str> {
        let meta = song.meta.as_ref();
        let text = match self {
            Self::Title => meta.and_then(|m| m.title.as_deref()).or_else(|| Some(stem(&song.name))),
            Self::Artist => meta.and_then(|m| m.artist.as_deref()),
            Self::AlbumArtist => meta.and_then(|m| m.album_artist.as_deref()),
            Self::Album => meta.and_then(|m| m.album.as_deref()),
            Self::Composer => meta.and_then(|m| m.composer.as_deref()),
            Self::Genre => meta.and_then(|m| m.genre.as_deref()),
            Self::FileName => Some(song.name.as_str()),
            _ => None,
        };
        text.map(str::trim).filter(|t| !t.is_empty())
    }

    fn number(self, song: &Song) -> Option<f64> {
        let meta = song.meta.as_ref()?;
        match self {
            Self::Year => meta.year.map(f64::from),
            Self::Track => meta.track.map(f64::from),
            Self::Disc => meta.disc.map(f64::from),
            Self::Length => meta.length,
            Self::Size => Some(meta.size as f64 / 1e6),
            _ => None,
        }
    }

    /// Parse a value typed for this field.
    fn parse(self, text: &str) -> Option<f64> {
        let text = text.trim();
        if self == Self::Length
            && let Some((m, s)) = text.split_once(':')
        {
            return Some(m.trim().parse::<f64>().ok()? * 60.0 + s.trim().parse::<f64>().ok()?);
        }
        text.parse().ok()
    }
}

/// The file name without its extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Is,
    IsNot,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsEmpty,
    IsNotEmpty,
    Greater,
    Less,
    InRange,
    /// Days.
    InLast,
    NotInLast,
    /// A date, `2026-10-20`.
    Before,
    After,
    Has,
    HasNot,
    IsTrue,
    IsFalse,
}

impl Kind {
    /// The ops for this kind of field (the first is a new rule's).
    pub fn ops(self) -> &'static [Op] {
        use Op::*;
        match self {
            Self::Text => &[Contains, NotContains, Is, IsNot, StartsWith, EndsWith, IsEmpty, IsNotEmpty],
            Self::Number => &[Is, IsNot, Greater, Less, InRange, IsEmpty, IsNotEmpty],
            Self::Date => &[InLast, NotInLast, Before, After],
            Self::Tag => &[Has, HasNot],
            Self::Flag => &[IsTrue, IsFalse],
        }
    }
}

impl Op {
    pub fn title(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::IsNot => "is not",
            Self::Contains => "contains",
            Self::NotContains => "does not contain",
            Self::StartsWith => "starts with",
            Self::EndsWith => "ends with",
            Self::IsEmpty => "is empty",
            Self::IsNotEmpty => "is not empty",
            Self::Greater => "is greater than",
            Self::Less => "is less than",
            Self::InRange => "is in the range",
            Self::InLast => "is in the last (days)",
            Self::NotInLast => "is not in the last (days)",
            Self::Before => "is before",
            Self::After => "is after",
            Self::Has => "has",
            Self::HasNot => "doesn't have",
            Self::IsTrue => "is true",
            Self::IsFalse => "is false",
        }
    }

    /// How many values it takes.
    pub fn values(self) -> usize {
        match self {
            Self::IsEmpty | Self::IsNotEmpty | Self::IsTrue | Self::IsFalse => 0,
            Self::InRange => 2,
            _ => 1,
        }
    }
}

/// How many songs are kept, and which.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    pub amount: f64,
    pub unit: Unit,
    /// Which are picked first.
    pub by: Order,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Items,
    Minutes,
    Hours,
    Mb,
    Gb,
}

impl Unit {
    pub const ALL: [Self; 5] = [Self::Items, Self::Minutes, Self::Hours, Self::Mb, Self::Gb];

    pub fn title(self) -> &'static str {
        match self {
            Self::Items => "songs",
            Self::Minutes => "minutes",
            Self::Hours => "hours",
            Self::Mb => "MB",
            Self::Gb => "GB",
        }
    }

    /// What a song counts for: one, seconds or bytes (unknown: nothing).
    fn of(self, song: &Song) -> f64 {
        let meta = song.meta.as_ref();
        match self {
            Self::Items => 1.0,
            Self::Minutes | Self::Hours => meta.and_then(|m| m.length).unwrap_or(0.0),
            Self::Mb | Self::Gb => meta.map_or(0.0, |m| m.size as f64),
        }
    }

    /// `amount` in what `of` counts.
    fn scale(self, amount: f64) -> f64 {
        amount
            * match self {
                Self::Items => 1.0,
                Self::Minutes => 60.0,
                Self::Hours => 3600.0,
                Self::Mb => 1e6,
                Self::Gb => 1e9,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Random,
    Title,
    Artist,
    /// By album, then disc and track.
    Album,
    Genre,
    Year,
    NewestAdded,
    OldestAdded,
}

impl Order {
    pub const ALL: [Self; 8] =
        [Self::Random, Self::Title, Self::Artist, Self::Album, Self::Genre, Self::Year, Self::NewestAdded, Self::OldestAdded];

    pub fn title(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album (disc and track)",
            Self::Genre => "genre",
            Self::Year => "year",
            Self::NewestAdded => "most recently added",
            Self::OldestAdded => "least recently added",
        }
    }
}

/// What a ruleset splits its songs by, and how few songs a playlist of it
/// can have (fewer are left out).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub by: SplitBy,
    #[serde(default)]
    pub min: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitBy {
    Artist,
    /// The album artist, else the artist: who an album's filed under.
    AlbumArtist,
    /// The album, by who it's filed under.
    Album,
    Composer,
    Genre,
    Year,
    /// A song is in each of its tags'.
    Tag,
}

impl SplitBy {
    pub const ALL: [Self; 7] = [Self::Artist, Self::AlbumArtist, Self::Album, Self::Composer, Self::Genre, Self::Year, Self::Tag];

    pub fn title(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::AlbumArtist => "album artist (else artist)",
            Self::Album => "album",
            Self::Composer => "composer",
            Self::Genre => "genre",
            Self::Year => "year",
            Self::Tag => "tag",
        }
    }
}

/// A playlist a ruleset made: what tells it apart (its value in
/// lowercase), its title, a line under that (an album's artist), and its
/// songs by key.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub songs: Vec<String>,
}

/// A ruleset as kept: its name and definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ruleset {
    pub name: String,
    pub rules: Rules,
}

/// The rulesets every library has, by ID: By Artist and By Album.
pub fn builtin() -> [(&'static str, Ruleset); 2] {
    let split = |by| Rules { split: Some(Split { by, min: 1 }), order: Order::Album, seed: 0, ..Rules::default() };
    [
        ("artists", Ruleset { name: "By Artist".into(), rules: split(SplitBy::AlbumArtist) }),
        ("albums", Ruleset { name: "By Album".into(), rules: split(SplitBy::Album) }),
    ]
}

/// A new random seed.
pub fn new_seed() -> u64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        bytes = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64).to_le_bytes();
    }
    u64::from_le_bytes(bytes)
}

/// Where song `key` goes in a random order seeded with `seed`.
fn shuffled(seed: u64, key: &str) -> u64 {
    let digest = Sha256::new().chain_update(seed.to_le_bytes()).chain_update(key.as_bytes()).finalize();
    u64::from_le_bytes(digest[..8].try_into().unwrap_or_default())
}

/// A rule ready to test songs with: its values parsed once.
struct Test<'a> {
    rule: &'a Rule,
    text: String,
    numbers: [Option<f64>; 2],
}

impl<'a> Test<'a> {
    fn new(rule: &'a Rule, now: i64) -> Self {
        let parse = |text: &str| match rule.field.kind() {
            Kind::Date => match rule.op {
                Op::InLast | Op::NotInLast => text.trim().parse::<f64>().ok().map(|days| now as f64 - days * 86_400.0),
                _ => parse_date(text).map(|secs| secs as f64),
            },
            _ => rule.field.parse(text),
        };
        Self { rule, text: rule.value.trim().to_lowercase(), numbers: [parse(&rule.value), parse(&rule.to)] }
    }

    fn matches(&self, song: &Song) -> bool {
        let Rule { field, op, value, .. } = self.rule;
        match field.kind() {
            Kind::Text => {
                let have = field.text(song).map(str::to_lowercase);
                let want = self.text.as_str();
                match (op, have) {
                    (Op::IsEmpty, have) => have.is_none(),
                    (Op::IsNotEmpty, have) => have.is_some(),
                    (Op::IsNot, have) => have.as_deref() != Some(want),
                    (Op::NotContains, have) => !have.is_some_and(|h| h.contains(want)),
                    (_, None) => false,
                    (Op::Is, Some(h)) => h == want,
                    (Op::Contains, Some(h)) => h.contains(want),
                    (Op::StartsWith, Some(h)) => h.starts_with(want),
                    (Op::EndsWith, Some(h)) => h.ends_with(want),
                    _ => false,
                }
            }
            Kind::Number => {
                let have = field.number(song);
                match (op, have, self.numbers) {
                    (Op::IsEmpty, have, _) => have.is_none(),
                    (Op::IsNotEmpty, have, _) => have.is_some(),
                    (Op::IsNot, Some(h), [Some(w), _]) => (h - w).abs() > 1e-9,
                    (Op::IsNot, None, _) => true,
                    (Op::Is, Some(h), [Some(w), _]) => (h - w).abs() <= 1e-9,
                    (Op::Greater, Some(h), [Some(w), _]) => h > w,
                    (Op::Less, Some(h), [Some(w), _]) => h < w,
                    (Op::InRange, Some(h), [Some(a), Some(b)]) => (a.min(b)..=a.max(b)).contains(&h),
                    _ => false,
                }
            }
            Kind::Date => {
                let have = song.imported as f64;
                match (op, self.numbers[0]) {
                    (Op::InLast, Some(since)) => have >= since,
                    (Op::NotInLast, Some(since)) => have < since,
                    (Op::Before, Some(day)) => have < day,
                    (Op::After, Some(day)) => have >= day + 86_400.0,
                    _ => false,
                }
            }
            Kind::Tag => {
                let has = song.tags.iter().any(|t| t == value);
                if *op == Op::HasNot { !has } else { has }
            }
            Kind::Flag => {
                let starred = song.tags.iter().any(|t| t == song_store::STARRED);
                if *op == Op::IsFalse { !starred } else { starred }
            }
        }
    }
}

/// `2026-10-20` (UTC) as Unix seconds.
fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.trim().splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(1));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from 1970-01-01 (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400)
}

/// What songs sort by, made once for each.
#[derive(Default)]
struct SortKey {
    text: [String; 3],
    numbers: [u64; 3],
}

fn sort_key(order: Order, seed: u64, key: &str, song: &Song) -> SortKey {
    let lower = |field: Field| field.text(song).map(str::to_lowercase).unwrap_or_else(|| "\u{10ffff}".into());
    let meta = song.meta.as_ref();
    let title = lower(Field::Title);
    match order {
        Order::Random => SortKey { numbers: [shuffled(seed, key), 0, 0], ..SortKey::default() },
        Order::Title => SortKey { text: [title, String::new(), String::new()], ..SortKey::default() },
        Order::Artist => SortKey { text: [lower(Field::Artist), lower(Field::Album), title], ..track_numbers(meta) },
        Order::Album => SortKey { text: [lower(Field::Album), filed_under(song).map(str::to_lowercase).unwrap_or_default(), title], ..track_numbers(meta) },
        Order::Genre => SortKey { text: [lower(Field::Genre), title, String::new()], ..SortKey::default() },
        Order::Year => SortKey { text: [title, String::new(), String::new()], numbers: [meta.and_then(|m| m.year).map_or(u64::MAX, u64::from), 0, 0] },
        Order::NewestAdded => SortKey { numbers: [u64::MAX - song.imported.max(0) as u64, 0, 0], text: [title, String::new(), String::new()] },
        Order::OldestAdded => SortKey { numbers: [song.imported.max(0) as u64, 0, 0], text: [title, String::new(), String::new()] },
    }
}

fn track_numbers(meta: Option<&song_store::Meta>) -> SortKey {
    let n = |v: Option<u32>| v.map_or(u64::MAX, u64::from);
    SortKey { numbers: [0, n(meta.and_then(|m| m.disc).or(Some(1))), n(meta.and_then(|m| m.track))], ..SortKey::default() }
}

fn compare(order: Order, a: &SortKey, b: &SortKey) -> Ordering {
    match order {
        // Album then artist, then disc and track, then title.
        Order::Artist | Order::Album => a.text[0]
            .cmp(&b.text[0])
            .then_with(|| a.text[1].cmp(&b.text[1]))
            .then_with(|| a.numbers[1..].cmp(&b.numbers[1..]))
            .then_with(|| a.text[2].cmp(&b.text[2])),
        _ => a.numbers.cmp(&b.numbers).then_with(|| a.text.cmp(&b.text)),
    }
}

/// Who an album's filed under: its album artist, else its artist.
fn filed_under(song: &Song) -> Option<&str> {
    Field::AlbumArtist.text(song).or_else(|| Field::Artist.text(song))
}

/// `songs` (by key) in `order`, ties going by key.
fn sorted<'a>(mut songs: Vec<(&'a str, &'a Song)>, order: Order, seed: u64) -> Vec<(&'a str, &'a Song)> {
    let mut keyed: Vec<(SortKey, (&str, &Song))> = songs.drain(..).map(|(k, s)| (sort_key(order, seed, k, s), (k, s))).collect();
    keyed.sort_by(|(a, x), (b, y)| compare(order, a, b).then_with(|| x.0.cmp(y.0)));
    keyed.into_iter().map(|(_, song)| song).collect()
}

impl Rules {
    fn matching<'a>(&self, songs: impl Iterator<Item = (&'a str, &'a Song)>, now: i64) -> Vec<(&'a str, &'a Song)> {
        let tests: Vec<Test> = self.rules.iter().map(|r| Test::new(r, now)).collect();
        songs
            .filter(|(_, song)| {
                tests.is_empty() || if self.any { tests.iter().any(|t| t.matches(song)) } else { tests.iter().all(|t| t.matches(song)) }
            })
            .collect()
    }

    /// `songs` cut to the limit (filling greedily in its order: a song
    /// that would go over is left out, one after it may still fit), in
    /// the playlist's order.
    fn limited<'a>(&self, songs: Vec<(&'a str, &'a Song)>) -> Vec<(&'a str, &'a Song)> {
        let songs = match &self.limit {
            Some(limit) => {
                let cap = limit.unit.scale(limit.amount);
                let mut total = 0.0;
                sorted(songs, limit.by, self.seed)
                    .into_iter()
                    .filter(|(_, song)| {
                        let size = limit.unit.of(song);
                        let fits = total + size <= cap + 1e-9;
                        if fits {
                            total += size;
                        }
                        fits
                    })
                    .collect()
            }
            None => songs,
        };
        sorted(songs, self.order, self.seed)
    }

    /// The songs (keys) of a smart playlist with this definition, from
    /// the library in `store` (those removed here left out), `now`
    /// (Unix seconds) saying when "the last N days" are.
    pub fn songs(&self, store: &SongStore, now: i64) -> Vec<String> {
        let candidates = store.known().filter(|(key, _)| !store.is_blocked(key));
        self.limited(self.matching(candidates, now)).into_iter().map(|(key, _)| key.to_string()).collect()
    }

    /// The playlists of a ruleset with this definition (none if it isn't
    /// one), by title: the songs that match, each in its value's
    /// playlist (a song without one is in none), limited and ordered in
    /// each.
    pub fn groups(&self, store: &SongStore, now: i64) -> Vec<Group> {
        let Some(split) = &self.split else { return Vec::new() };
        let tags = store.tags();
        let candidates = store.known().filter(|(key, _)| !store.is_blocked(key));
        let mut groups: BTreeMap<String, (String, Option<String>, Vec<(&str, &Song)>)> = BTreeMap::new();
        for (key, song) in self.matching(candidates, now) {
            let values: Vec<(String, Option<String>)> = match split.by {
                SplitBy::Artist => Field::Artist.text(song).map(|a| (a.to_string(), None)).into_iter().collect(),
                SplitBy::AlbumArtist => filed_under(song).map(|a| (a.to_string(), None)).into_iter().collect(),
                SplitBy::Album => match (Field::Album.text(song), filed_under(song)) {
                    (Some(album), artist) => vec![(album.to_string(), artist.map(str::to_string))],
                    _ => Vec::new(),
                },
                SplitBy::Composer => Field::Composer.text(song).map(|c| (c.to_string(), None)).into_iter().collect(),
                SplitBy::Genre => Field::Genre.text(song).map(|g| (g.to_string(), None)).into_iter().collect(),
                SplitBy::Year => song.meta.as_ref().and_then(|m| m.year).map(|y| (y.to_string(), None)).into_iter().collect(),
                SplitBy::Tag => song.tags.iter().filter_map(|id| tags.get(id)).map(|t| (t.name.clone(), None)).collect(),
            };
            for (title, subtitle) in values {
                let key_of = format!("{}\u{0}{}", title.to_lowercase(), subtitle.as_deref().unwrap_or_default().to_lowercase());
                groups.entry(key_of).or_insert_with(|| (title, subtitle, Vec::new())).2.push((key, song));
            }
        }
        groups
            .into_iter()
            .filter(|(_, (_, _, songs))| songs.len() >= split.min.max(1))
            .map(|(key, (title, subtitle, songs))| Group {
                key,
                title,
                subtitle,
                songs: self.limited(songs).into_iter().map(|(key, _)| key.to_string()).collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::Meta;

    fn store(songs: &[(&str, Meta, &[&str], i64)]) -> SongStore {
        let dir = std::env::temp_dir().join(format!("synththing-smart-{}-{}", std::process::id(), songs.len()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir);
        store.learn(songs.iter().enumerate().map(|(i, (name, meta, tags, imported))| {
            let key = format!("{:064x}.ogg", i + 1);
            let song = Song { name: name.to_string(), imported: *imported, tags: tags.iter().map(|t| t.to_string()).collect(), meta: Some(meta.clone()) };
            (key, song)
        }));
        store
    }

    fn meta(artist: &str, album: &str, track: u32, length: f64) -> Meta {
        Meta {
            artist: Some(artist.into()),
            album: (!album.is_empty()).then(|| album.into()),
            track: Some(track),
            length: Some(length),
            size: 4_000_000,
            ..Meta::default()
        }
    }

    fn library() -> SongStore {
        store(&[
            ("a.ogg", meta("Nujabes", "Modal Soul", 2, 200.0), &["starred"], 100),
            ("b.ogg", meta("Nujabes", "Modal Soul", 1, 300.0), &[], 200),
            ("c.ogg", meta("nujabes", "Metaphorical Music", 1, 240.0), &[], 300),
            ("d.ogg", meta("Basil Poledouris", "", 1, 100.0), &["starred"], 400),
            ("e.ogg", Meta { size: 1, ..Meta::default() }, &[], 500),
        ])
    }

    fn names(store: &SongStore, keys: &[String]) -> Vec<String> {
        keys.iter().map(|k| store.known().find(|(key, _)| key == k).unwrap().1.name.clone()).collect()
    }

    #[test]
    fn rules_match_all_or_any() {
        let lib = library();
        let rule = |field, op, value: &str| Rule { field, op, value: value.into(), to: String::new() };
        let mut rules = Rules { rules: vec![rule(Field::Artist, Op::Is, "NUJABES"), rule(Field::Starred, Op::IsTrue, "")], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, 0)), ["a.ogg"]);
        rules.any = true;
        assert_eq!(names(&lib, &rules.songs(&lib, 0)), ["a.ogg", "b.ogg", "c.ogg", "d.ogg"]);
        let rules = Rules { rules: vec![rule(Field::Album, Op::IsEmpty, "")], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, 0)), ["d.ogg", "e.ogg"]);
        let rules = Rules { rules: vec![Rule { field: Field::Length, op: Op::InRange, value: "3:00".into(), to: "250".into() }], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, 0)), ["a.ogg", "c.ogg"]);
        let rules = Rules { rules: vec![rule(Field::Imported, Op::InLast, "1")], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, 86_400 + 300)), ["c.ogg", "d.ogg", "e.ogg"]);
        assert_eq!(parse_date("1970-01-02"), Some(86_400));
        assert_eq!(parse_date("2026-10-20").map(crate::song_list::date).as_deref(), Some("2026-10-20"));
    }

    #[test]
    fn limits_fill_in_their_order() {
        let lib = library();
        let rules = Rules { limit: Some(Limit { amount: 2.0, unit: Unit::Items, by: Order::NewestAdded }), ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, 0)), ["d.ogg", "e.ogg"], "newest two, by title");
        // 9 minutes, by album: 240 and 300 fit, 200 doesn't, the one of
        // unknown length does, 100 doesn't.
        let rules = Rules { limit: Some(Limit { amount: 9.0, unit: Unit::Minutes, by: Order::Album }), order: Order::Album, ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, 0)), ["c.ogg", "b.ogg", "e.ogg"]);
    }

    #[test]
    fn random_is_seeded_and_stable() {
        let lib = library();
        let rules = Rules { order: Order::Random, seed: 7, ..Rules::default() };
        let first = rules.songs(&lib, 0);
        assert_eq!(first, rules.songs(&lib, 0), "the same every time");
        let reseeded = Rules { seed: 8, ..rules.clone() };
        let mut a = first.clone();
        let mut b = reseeded.songs(&lib, 0);
        a.sort();
        b.sort();
        assert_eq!(a, b, "the same songs");
        // A song coming in keeps the others in their order.
        let all = store(&[
            ("a.ogg", Meta::default(), &[], 0),
            ("b.ogg", Meta::default(), &[], 0),
            ("c.ogg", Meta::default(), &[], 0),
            ("d.ogg", Meta::default(), &[], 0),
            ("e.ogg", Meta::default(), &[], 0),
            ("f.ogg", Meta::default(), &[], 0),
        ]);
        let mut more = rules.songs(&all, 0);
        let new = format!("{:064x}.ogg", 6);
        more.retain(|k| *k != new);
        assert_eq!(more, first);
    }

    #[test]
    fn rulesets_split_into_playlists() {
        let lib = library();
        let [(_, artists), (_, albums)] = builtin();
        let groups = artists.rules.groups(&lib, 0);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Basil Poledouris", "Nujabes"], "case-insensitive, songs without an artist in none");
        assert_eq!(names(&lib, &groups[1].songs), ["c.ogg", "b.ogg", "a.ogg"], "by album, then track");
        let groups = albums.rules.groups(&lib, 0);
        let titles: Vec<(&str, Option<&str>)> = groups.iter().map(|g| (g.title.as_str(), g.subtitle.as_deref())).collect();
        assert_eq!(titles, [("Metaphorical Music", Some("nujabes")), ("Modal Soul", Some("Nujabes"))]);
        assert_eq!(names(&lib, &groups[1].songs), ["b.ogg", "a.ogg"]);
        let at_least_two = Rules { split: Some(Split { by: SplitBy::AlbumArtist, min: 2 }), ..artists.rules.clone() };
        assert_eq!(at_least_two.groups(&lib, 0).len(), 1);
    }
}
