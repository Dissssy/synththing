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

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fields::{self, Facts, Field, Kind};
use crate::matching;
use crate::song_store::{Song, SongStore, Tag};
use crate::wishlist::{Index, Wishes};

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
        Self { field, op: ops(field.kind())[0], value: String::new(), to: String::new() }
    }
}

/// A text field's value for rules and sorting: a song without a title
/// goes by its file name.
fn text<'a>(song: &Facts<'a>, field: Field) -> Option<Cow<'a, str>> {
    song.text(field).or_else(|| if field == Field::Title { song.text(Field::FileName) } else { None })
}

/// Parse a value typed for `field`.
/// A length as `6:00` or `1:02:30`, or in units (`6m`, `90s`, `1h2m`,
/// `6m30s`), a bare number being minutes; a size in MB, or with a unit
/// (`800kb`, `1.2gb`); else a plain number.
pub fn parse(field: Field, text: &str) -> Option<f64> {
    let text = text.trim().to_lowercase();
    match field {
        Field::Length if text.contains(':') => {
            let parts = text.split(':').map(|p| p.trim().parse::<f64>().ok()).collect::<Option<Vec<f64>>>()?;
            (2..=3).contains(&parts.len()).then(|| parts.iter().fold(0.0, |total, p| total * 60.0 + p))
        }
        Field::Length => units(&text, &[("h", 3600.0), ("m", 60.0), ("s", 1.0)], 60.0),
        Field::Size => units(&text, &[("kb", 1e-3), ("mb", 1.0), ("gb", 1e3)], 1.0),
        _ => text.parse().ok(),
    }
}

/// `text` as numbers with units (`1h2m`), in what `units` (each with
/// what one is worth) count; a bare number is worth `bare`.
fn units(text: &str, units: &[(&str, f64)], bare: f64) -> Option<f64> {
    if let Ok(n) = text.parse::<f64>() {
        return Some(n * bare);
    }
    let mut total = 0.0;
    let mut rest = text;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        let n: f64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let (unit, worth) = units.iter().find(|(u, _)| rest.starts_with(u))?;
        total += n * worth;
        rest = rest[unit.len()..].trim_start();
    }
    (!text.is_empty()).then_some(total)
}

/// How long ago `text` says, in seconds: `30d`, `12h`, `2w`, `6mo` or
/// `1y` (a bare number is days).
pub fn parse_age(text: &str) -> Option<f64> {
    const DAY: f64 = 86_400.0;
    units(&text.trim().to_lowercase(), &[("h", 3600.0), ("d", DAY), ("w", 7.0 * DAY), ("mo", 30.0 * DAY), ("y", 365.0 * DAY)], DAY)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// The same past case, spacing and punctuation (`matching::close`).
    Is,
    IsNot,
    Contains,
    NotContains,
    /// Fuzzily (`matching::fuzzy`).
    Like,
    NotLike,
    /// Case and all.
    Exactly,
    NotExactly,
    StartsWith,
    EndsWith,
    IsEmpty,
    IsNotEmpty,
    Greater,
    Less,
    AtLeast,
    AtMost,
    InRange,
    /// How long ago (`parse_age`).
    InLast,
    NotInLast,
    /// In a year, month or day: `2026`, `2026-10`, `2026-10-20`
    /// (`parse_period`).
    On,
    NotOn,
    Before,
    After,
    Has,
    HasNot,
    IsTrue,
    IsFalse,
}

/// The ops for a kind of field (the first is a new rule's).
pub fn ops(kind: Kind) -> &'static [Op] {
    use Op::*;
    match kind {
        Kind::Text => &[Contains, NotContains, Is, IsNot, Like, NotLike, Exactly, NotExactly, StartsWith, EndsWith, IsEmpty, IsNotEmpty],
        Kind::Number => &[Is, IsNot, Greater, Less, AtLeast, AtMost, InRange, IsEmpty, IsNotEmpty],
        Kind::Date => &[InLast, NotInLast, On, NotOn, Before, After, AtLeast, AtMost, InRange],
        Kind::Tag => &[Has, HasNot],
        Kind::Flag => &[IsTrue, IsFalse],
    }
}

impl Op {
    pub fn title(self) -> &'static str {
        match self {
            Self::Is => "is",
            Self::IsNot => "is not",
            Self::Contains => "contains",
            Self::NotContains => "does not contain",
            Self::Like => "is like (fuzzy)",
            Self::NotLike => "is not like (fuzzy)",
            Self::Exactly => "is exactly (case too)",
            Self::NotExactly => "is not exactly (case too)",
            Self::StartsWith => "starts with",
            Self::EndsWith => "ends with",
            Self::IsEmpty => "is empty",
            Self::IsNotEmpty => "is not empty",
            Self::Greater => "is greater than",
            Self::Less => "is less than",
            Self::AtLeast => "is at least",
            Self::AtMost => "is at most",
            Self::InRange => "is in the range",
            Self::InLast => "is in the last",
            Self::NotInLast => "is not in the last",
            Self::On => "is in",
            Self::NotOn => "is not in",
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

/// A playlist a ruleset made: what tells it apart (its value's
/// `fields::group_key`, and its subtitle's), its title, a line under that (an album's artist), and its
/// songs by key.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: String,
    pub title: String,
    /// What it sorts by (`fields::sort_text`).
    pub sort: String,
    pub subtitle: Option<String>,
    /// The artists its subtitle names (an album's), each a link to theirs.
    pub artists: Vec<String>,
    pub songs: Vec<String>,
}

/// The subtitle of an album whose songs are by several artists, and no
/// album artist says who it's by.
pub const VARIOUS_ARTISTS: &str = "Various Artists";

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

/// The artists (by `fields::group_key`) that `store`'s songs matched at
/// MusicBrainz credit: a matched song's artists, and a matched album's
/// album artists.
fn matched_artists(store: &SongStore) -> BTreeSet<String> {
    let mut artists = BTreeSet::new();
    for (_, song) in store.known() {
        let facts = Facts::of(song);
        for (field, matched) in [(Field::Artist, Field::Matched), (Field::AlbumArtist, Field::AlbumMatched)] {
            if facts.flag(matched) {
                artists.extend(facts.items(field).iter().map(|a| fields::group_key(a, "")));
            }
        }
    }
    artists
}

/// A rule ready to test songs with: its values parsed once.
struct Test<'a> {
    rule: &'a Rule,
    text: String,
    numbers: [Option<f64>; 2],
    /// A date rule's value and `to`, as the periods they name (an age,
    /// the moment it reaches back to).
    periods: [Option<(f64, f64)>; 2],
    /// For `Field::ArtistMatched`: the library's matched artists.
    artists: Option<BTreeSet<String>>,
    /// For `Field::Wished` and `Field::Wish`: the wishes, to find those a
    /// song fills.
    wishes: Option<Index<'a>>,
}

impl<'a> Test<'a> {
    fn new(rule: &'a Rule, store: &SongStore, wishes: &'a Wishes, now: i64) -> Self {
        let date = rule.field.kind() == Kind::Date;
        let period = |text: &str| match rule.op {
            _ if !date => None,
            Op::InLast | Op::NotInLast => parse_age(text).map(|age| (now as f64 - age, f64::INFINITY)),
            _ => parse_period(text).map(|(start, end)| (start as f64, end as f64)),
        };
        let number = |text: &str| if date { None } else { parse(rule.field, text) };
        Self {
            rule,
            text: rule.value.trim().to_lowercase(),
            numbers: [number(&rule.value), number(&rule.to)],
            periods: [period(&rule.value), period(&rule.to)],
            artists: (rule.field == Field::ArtistMatched).then(|| matched_artists(store)),
            wishes: matches!(rule.field, Field::Wished | Field::Wish).then(|| Index::new(wishes)),
        }
    }

    fn matches(&self, song: &Song) -> bool {
        let Rule { field, op, value, .. } = self.rule;
        let (whole, song) = (song, Facts::of(song));
        match field.kind() {
            Kind::Text => {
                // (Each of a list field's items: a song's genre "is" each
                // of its genres.)
                let raw: Vec<Cow<str>> = match (field, &self.wishes) {
                    (Field::Wish, Some(wishes)) => wishes.fills(whole).into_iter().map(|id| Cow::Borrowed(wishes.name(id))).collect(),
                    (Field::Title, _) => text(&song, *field).into_iter().collect(),
                    _ => song.items(*field),
                };
                let have: Vec<String> = raw.iter().map(|t| t.to_lowercase()).collect();
                let want = self.text.as_str();
                let any = |test: &dyn Fn(&str) -> bool| have.iter().any(|h| test(h));
                let any_raw = |test: &dyn Fn(&str) -> bool| raw.iter().any(|h| test(h));
                match op {
                    Op::IsEmpty => have.is_empty(),
                    Op::IsNotEmpty => !have.is_empty(),
                    Op::Is => any(&|h| matching::close(h, want)),
                    Op::IsNot => !any(&|h| matching::close(h, want)),
                    Op::Like => any(&|h| matching::fuzzy(h, want).is_some()),
                    Op::NotLike => !any(&|h| matching::fuzzy(h, want).is_some()),
                    Op::Exactly => any_raw(&|h| h == value.trim()),
                    Op::NotExactly => !any_raw(&|h| h == value.trim()),
                    Op::Contains => any(&|h| h.contains(want)),
                    Op::NotContains => !any(&|h| h.contains(want)),
                    Op::StartsWith => any(&|h| h.starts_with(want)),
                    Op::EndsWith => any(&|h| h.ends_with(want)),
                    _ => false,
                }
            }
            Kind::Number => {
                let have = song.number(*field);
                match (op, have, self.numbers) {
                    (Op::IsEmpty, have, _) => have.is_none(),
                    (Op::IsNotEmpty, have, _) => have.is_some(),
                    (Op::IsNot, Some(h), [Some(w), _]) => (h - w).abs() > 1e-9,
                    (Op::IsNot, None, _) => true,
                    (Op::Is, Some(h), [Some(w), _]) => (h - w).abs() <= 1e-9,
                    (Op::Greater, Some(h), [Some(w), _]) => h > w,
                    (Op::Less, Some(h), [Some(w), _]) => h < w,
                    (Op::AtLeast, Some(h), [Some(w), _]) => h >= w - 1e-9,
                    (Op::AtMost, Some(h), [Some(w), _]) => h <= w + 1e-9,
                    (Op::InRange, Some(h), [Some(a), Some(b)]) => (a.min(b) - 1e-9..=a.max(b) + 1e-9).contains(&h),
                    _ => false,
                }
            }
            Kind::Date => {
                let Some(have) = song.number(*field) else { return false };
                match (op, self.periods) {
                    (Op::InLast, [Some((since, _)), _]) => have >= since,
                    (Op::NotInLast, [Some((since, _)), _]) => have < since,
                    (Op::On, [Some((start, end)), _]) => (start..end).contains(&have),
                    (Op::NotOn, [Some((start, end)), _]) => !(start..end).contains(&have),
                    (Op::Before, [Some((start, _)), _]) => have < start,
                    (Op::After, [Some((_, end)), _]) => have >= end,
                    (Op::AtLeast, [Some((start, _)), _]) => have >= start,
                    (Op::AtMost, [Some((_, end)), _]) => have < end,
                    (Op::InRange, [Some(a), Some(b)]) => (a.0.min(b.0)..a.1.max(b.1)).contains(&have),
                    _ => false,
                }
            }
            Kind::Tag => {
                let has = song.has_tag(value);
                if *op == Op::HasNot { !has } else { has }
            }
            Kind::Flag => {
                let set = match (&self.artists, &self.wishes) {
                    (Some(artists), _) => song.items(Field::Artist).iter().any(|a| artists.contains(&fields::group_key(a, ""))),
                    (_, Some(wishes)) => !wishes.fills(whole).is_empty(),
                    _ => song.flag(*field),
                };
                if *op == Op::IsFalse { !set } else { set }
            }
        }
    }
}

/// The year, month or day `text` names (`2026`, `2026-10`, `2026-10-20`,
/// UTC): when it starts and ends, in Unix seconds.
pub fn parse_period(text: &str) -> Option<(i64, i64)> {
    let parts = text.trim().split('-').map(|p| p.parse::<i64>().ok()).collect::<Option<Vec<i64>>>()?;
    match parts[..] {
        [y] if (1000..=9999).contains(&y) => Some((day(y, 1, 1)?, day(y + 1, 1, 1)?)),
        [y, m] => Some((day(y, m, 1)?, if m == 12 { day(y + 1, 1, 1)? } else { day(y, m + 1, 1)? })),
        [y, m, d] => day(y, m, d).map(|start| (start, start + 86_400)),
        _ => None,
    }
}

/// The day `y-m-d` (UTC) starts, in Unix seconds.
fn day(y: i64, m: i64, d: i64) -> Option<i64> {
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

/// What a song sorts by in `order`, names by what they sort as
/// (`Facts::sort_text`, past a leading article unless `articles`).
fn sort_key(order: Order, seed: u64, key: &str, song: &Song, articles: bool) -> SortKey {
    let facts = Facts::of(song);
    let lower = |field: Field| text(&facts, field).map(|t| t.to_lowercase()).unwrap_or_else(|| "\u{10ffff}".into());
    // (Spellings of one name together, as they're grouped.)
    let name = |field: Field| facts.sort_text(field, articles).map(|t| fields::group_key(&t, "")).unwrap_or_else(|| "\u{10ffff}".into());
    let title = facts.sort_text(Field::Title, articles).unwrap_or_else(|| lower(Field::FileName));
    match order {
        Order::Random => SortKey { numbers: [shuffled(seed, key), 0, 0], ..SortKey::default() },
        Order::Title => SortKey { text: [title, String::new(), String::new()], ..SortKey::default() },
        Order::Artist => SortKey { text: [name(Field::Artist), name(Field::Album), title], ..track_numbers(&facts) },
        // (An album's songs in track order whoever each is by: an album is
        // its name's songs, `Rules::groups`.)
        Order::Album => SortKey { text: [name(Field::Album), String::new(), title], ..track_numbers(&facts) },
        Order::Genre => SortKey { text: [lower(Field::Genre), title, String::new()], ..SortKey::default() },
        Order::Year => SortKey { text: [title, String::new(), String::new()], numbers: [facts.number(Field::Year).map_or(u64::MAX, |y| y as u64), 0, 0] },
        Order::NewestAdded => SortKey { numbers: [u64::MAX - song.imported.max(0) as u64, 0, 0], text: [title, String::new(), String::new()] },
        Order::OldestAdded => SortKey { numbers: [song.imported.max(0) as u64, 0, 0], text: [title, String::new(), String::new()] },
    }
}

fn track_numbers(song: &Facts) -> SortKey {
    let n = |field| song.number(field).map_or(u64::MAX, |n| n as u64);
    SortKey { numbers: [0, song.number(Field::Disc).map_or(1, |d| d as u64), n(Field::Track)], ..SortKey::default() }
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

/// Who a song's album is filed under: its album artists, else Various
/// Artists for a compilation's, else its artists.
fn filed_under<'a>(song: &Facts<'a>) -> Vec<Cow<'a, str>> {
    let album_artists = song.items(Field::AlbumArtist);
    match () {
        _ if !album_artists.is_empty() => album_artists,
        _ if song.flag(Field::Compilation) => vec![Cow::Borrowed(VARIOUS_ARTISTS)],
        _ => song.items(Field::Artist),
    }
}

/// What `song` (with these `facts`) is split by in a ruleset split `by`
/// anything but albums (made of its album, artist and album artist): a
/// playlist's worth of each.
fn split_values<'a>(by: SplitBy, facts: &Facts<'a>, song: &'a Song, tags: &'a BTreeMap<String, Tag>) -> Vec<Cow<'a, str>> {
    match by {
        SplitBy::Artist => facts.items(Field::Artist),
        SplitBy::AlbumArtist => filed_under(facts),
        SplitBy::Album => unreachable!("albums are made of more than their names"),
        SplitBy::Composer => facts.items(Field::Composer),
        SplitBy::Genre => facts.items(Field::Genre),
        SplitBy::Year => facts.value(Field::Year).map(|y| Cow::Owned(y.text)).into_iter().collect(),
        SplitBy::Tag => song.tags.iter().filter_map(|id| tags.get(id)).map(|t| Cow::Borrowed(t.name.as_str())).collect(),
    }
}

/// `songs` (by key) in `order`, ties going by key.
fn sorted<'a>(mut songs: Vec<(&'a str, &'a Song)>, order: Order, seed: u64, articles: bool) -> Vec<(&'a str, &'a Song)> {
    let mut keyed: Vec<(SortKey, (&str, &Song))> = songs.drain(..).map(|(k, s)| (sort_key(order, seed, k, s, articles), (k, s))).collect();
    keyed.sort_by(|(a, x), (b, y)| compare(order, a, b).then_with(|| x.0.cmp(y.0)));
    keyed.into_iter().map(|(_, song)| song).collect()
}

impl Rules {
    /// Whether a song matches the rules (their values parsed once, here),
    /// `now` (Unix seconds) saying when "the last N days" are, `store`
    /// the library the songs are in.
    pub fn matcher<'r>(&'r self, store: &SongStore, wishes: &'r Wishes, now: i64) -> impl Fn(&Song) -> bool + use<'r> {
        let tests: Vec<Test> = self.rules.iter().map(|r| Test::new(r, store, wishes, now)).collect();
        move |song| tests.is_empty() || if self.any { tests.iter().any(|t| t.matches(song)) } else { tests.iter().all(|t| t.matches(song)) }
    }

    fn matching<'a>(&self, store: &SongStore, wishes: &Wishes, songs: impl Iterator<Item = (&'a str, &'a Song)>, now: i64) -> Vec<(&'a str, &'a Song)> {
        let matches = self.matcher(store, wishes, now);
        songs.filter(|(_, song)| matches(song)).collect()
    }

    /// `songs` cut to the limit (filling greedily in its order: a song
    /// that would go over is left out, one after it may still fit), in
    /// the playlist's order.
    fn limited<'a>(&self, songs: Vec<(&'a str, &'a Song)>, articles: bool) -> Vec<(&'a str, &'a Song)> {
        let songs = match &self.limit {
            Some(limit) => {
                let cap = limit.unit.scale(limit.amount);
                let mut total = 0.0;
                sorted(songs, limit.by, self.seed, articles)
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
        sorted(songs, self.order, self.seed, articles)
    }

    /// The songs (keys) of a smart playlist with this definition, from
    /// the library in `store` (those removed here left out), `now`
    /// (Unix seconds) saying when "the last N days" are, names sorted
    /// past a leading article unless `articles`.
    pub fn songs(&self, store: &SongStore, wishes: &Wishes, now: i64, articles: bool) -> Vec<String> {
        let candidates = store.known().filter(|(key, _)| !store.is_blocked(key));
        self.limited(self.matching(store, wishes, candidates, now), articles).into_iter().map(|(key, _)| key.to_string()).collect()
    }

    /// The playlists of a ruleset with this definition (none if it isn't
    /// one), in the order their titles sort (as their songs say they
    /// sort, else past a leading article unless `articles`): the songs that match, each in its value's
    /// playlist (a song without one is in none), limited and ordered in
    /// each. Values are told apart by `fields::group_key` (past the
    /// characters in `ignore`), each playlist titled with the best of its
    /// songs' spellings.
    /// The songs (keys) a ruleset with this definition leaves out of its
    /// playlists for want of what it splits by (none for a ruleset of
    /// tags, or not a ruleset): those its rules match that say no album,
    /// artist, year...
    pub fn missing(&self, store: &SongStore, wishes: &Wishes, now: i64) -> Vec<String> {
        let Some(split) = self.split.as_ref().filter(|s| s.by != SplitBy::Tag) else { return Vec::new() };
        let tags = store.tags();
        let candidates = store.known().filter(|(key, _)| !store.is_blocked(key));
        let lacks = |song: &Song| {
            let facts = Facts::of(song);
            if split.by == SplitBy::Album { facts.text(Field::Album).is_none() } else { split_values(split.by, &facts, song, tags).is_empty() }
        };
        self.matching(store, wishes, candidates, now).into_iter().filter(|(_, song)| lacks(song)).map(|(key, _)| key.to_string()).collect()
    }

    pub fn groups(&self, store: &SongStore, wishes: &Wishes, now: i64, ignore: &str, articles: bool) -> Vec<Group> {
        /// A playlist being made: its songs' spellings of its title and
        /// subtitle (an album's: its album artists', else its artists'),
        /// and its songs.
        #[derive(Default)]
        struct Making<'a> {
            titles: Vec<Cow<'a, str>>,
            subtitles: Vec<Cow<'a, str>>,
            artists: Vec<Cow<'a, str>>,
            /// What its songs say its title sorts as.
            sorts: Vec<Cow<'a, str>>,
            /// A song says it's on a compilation.
            compilation: bool,
            songs: Vec<(&'a str, &'a Song)>,
        }
        /// A song on an album: its key, it, and the album's name, album
        /// artist and artist as it says them (by the name's group key).
        type OnAlbum<'a> = (&'a str, &'a Song, Cow<'a, str>, Option<Cow<'a, str>>, Option<Cow<'a, str>>);
        let Some(split) = &self.split else { return Vec::new() };
        let tags = store.tags();
        let candidates = store.known().filter(|(key, _)| !store.is_blocked(key));
        let mut groups: BTreeMap<String, Making> = BTreeMap::new();
        let mut albums: BTreeMap<String, Vec<OnAlbum>> = BTreeMap::new();
        for (key, song) in self.matching(store, wishes, candidates, now) {
            let facts = Facts::of(song);
            if split.by == SplitBy::Album {
                if let Some(album) = facts.text(Field::Album) {
                    let on = (key, song, album, facts.text(Field::AlbumArtist), facts.first(Field::Artist));
                    albums.entry(fields::group_key(&on.2, ignore)).or_default().push(on);
                }
                continue;
            }
            let values = split_values(split.by, &facts, song, tags);
            // (What a name sorts as is for a song with one.)
            let sort_as = match split.by {
                SplitBy::Artist => Some(Field::SortArtist),
                SplitBy::AlbumArtist if facts.text(Field::AlbumArtist).is_some() => Some(Field::SortAlbumArtist),
                SplitBy::AlbumArtist => Some(Field::SortArtist),
                SplitBy::Composer => Some(Field::SortComposer),
                _ => None,
            };
            let sort_as = sort_as.filter(|_| values.len() == 1).and_then(|f| facts.text(f));
            for title in values {
                let making = groups.entry(format!("{}\u{0}", fields::group_key(&title, ignore))).or_default();
                making.titles.push(title);
                making.sorts.extend(sort_as.clone());
                making.songs.push((key, song));
            }
        }
        // An album is its name's songs, whoever each is by: a soundtrack's
        // songs by several artists, or with its album artist spelled
        // several ways, are one album.
        for (name, songs) in albums {
            let making = groups.entry(format!("{name}\u{0}")).or_default();
            for (key, song, album, album_artist, artist) in songs {
                making.titles.push(album);
                making.subtitles.extend(album_artist);
                making.artists.extend(artist);
                making.compilation |= Facts::of(song).flag(Field::Compilation);
                making.sorts.extend(Facts::of(song).text(Field::SortAlbum));
                making.songs.push((key, song));
            }
        }
        let mut groups: Vec<Group> = groups
            .into_iter()
            .filter(|(_, making)| making.songs.len() >= split.min.max(1))
            .map(|(key, making)| {
                let artists: Vec<String> = match fields::best_spelling(making.subtitles.iter().map(|t| t.as_ref())) {
                    Some(album_artists) => Field::AlbumArtist.def().split(album_artists).map(str::to_string).collect(),
                    None if !making.compilation && making.artists.iter().map(|a| fields::group_key(a, ignore)).collect::<BTreeSet<_>>().len() == 1 => {
                        fields::best_spelling(making.artists.iter().map(|a| a.as_ref())).map(str::to_string).into_iter().collect()
                    }
                    None => Vec::new(),
                };
                let subtitle = match (artists.is_empty(), making.artists.is_empty()) {
                    (false, _) => Some(fields::Def::names(&artists)),
                    (true, false) => Some(VARIOUS_ARTISTS.to_string()),
                    (true, true) if making.compilation => Some(VARIOUS_ARTISTS.to_string()),
                    (true, true) => None,
                };
                let title = fields::best_spelling(making.titles.iter().map(|t| t.as_ref())).unwrap_or_default().to_string();
                let sort = match fields::best_spelling(making.sorts.iter().map(|t| t.as_ref())) {
                    Some(sort) => sort.to_lowercase(),
                    None => fields::sort_text(&title, articles),
                };
                Group {
                    key,
                    title,
                    sort,
                    subtitle,
                    artists,
                    songs: self.limited(making.songs, articles).into_iter().map(|(key, _)| key.to_string()).collect(),
                }
            })
            .collect();
        groups.sort_by(|a, b| a.sort.cmp(&b.sort).then_with(|| a.key.cmp(&b.key)));
        groups
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
            let song = Song { name: name.to_string(), imported: *imported, tags: tags.iter().map(|t| t.to_string()).collect(), meta: Some(meta.clone()), ..Song::default() };
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

    #[test]
    fn matched_at_musicbrainz() {
        use crate::song_store::Online;
        let dir = std::env::temp_dir().join(format!("synththing-smart-matched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir);
        let online = |release: Option<&str>, recording: Option<&str>| Some(Online { release: release.map(Into::into), recording: recording.map(Into::into), ..Online::default() });
        let songs = [
            ("matched.ogg", meta("Nujabes", "Modal Soul", 1, 1.0), online(Some("r1"), Some("c1"))),
            ("extra.ogg", Meta { album_artist: Some("Shing02".into()), ..meta("Uyama Hiroto", "Modal Soul", 9, 1.0) }, online(Some("r1"), None)),
            ("tagged.ogg", Meta { musicbrainz_recording: Some("c2".into()), ..meta("Fat Jon", "", 1, 1.0) }, None),
            ("wrong.ogg", Meta { musicbrainz_recording: Some("c3".into()), ..meta("Fat Jon", "", 2, 1.0) }, Some(Online { rejected: vec!["r9".into()], ..Online::default() })),
            ("loose.ogg", meta("nujabes", "Hydeout", 1, 1.0), None),
            ("other.ogg", meta("Shing02; Someone", "", 1, 1.0), None),
            ("unknown.ogg", meta("Koji Kondo", "", 1, 1.0), None),
        ];
        store.learn(songs.into_iter().enumerate().map(|(i, (name, meta, online))| {
            (format!("{:064x}.ogg", i + 1), Song { name: name.into(), meta: Some(meta), online, ..Song::default() })
        }));
        let found = |field, op| {
            let rules = Rules { rules: vec![Rule { field, op, value: String::new(), to: String::new() }], ..Rules::default() };
            let wishes = Wishes::new();
            let matches = rules.matcher(&store, &wishes, 0);
            let mut names: Vec<String> = store.known().filter(|(_, s)| matches(s)).map(|(_, s)| s.name.clone()).collect();
            names.sort();
            names
        };
        assert_eq!(found(Field::Matched, Op::IsTrue), ["matched.ogg", "tagged.ogg", "wrong.ogg"], "its file's recording counts until a match says otherwise");
        assert_eq!(found(Field::AlbumMatched, Op::IsTrue), ["extra.ogg", "matched.ogg"]);
        assert_eq!(found(Field::ArtistMatched, Op::IsTrue), ["loose.ogg", "matched.ogg", "other.ogg", "tagged.ogg", "wrong.ogg"]);
        assert_eq!(found(Field::ArtistMatched, Op::IsFalse), ["extra.ogg", "unknown.ogg"], "an album artist counts on a matched album");

        use crate::wishlist::{Kind, Wish};
        let wishes: Wishes = [
            ("w1", Wish { release: Some("r1".into()), skipped: vec!["c9".into()], under: Some("w3".into()), ..Wish::new(Kind::Album, "Modal Soul") }),
            ("w2", Wish { musicbrainz: Some("c2".into()), ..Wish::new(Kind::Song, "Tagged") }),
            ("w3", Wish { musicbrainz: Some("a1".into()), ..Wish::new(Kind::Artist, "Nujabes") }),
            ("w4", Wish { artist: Some("Koji Kondo".into()), ..Wish::new(Kind::Text, "") }),
        ]
        .into_iter()
        .map(|(id, wish)| (id.to_string(), wish))
        .collect();
        let wished = |field, op, value: &str| {
            let rules = Rules { rules: vec![Rule { field, op, value: value.into(), to: String::new() }], ..Rules::default() };
            let matches = rules.matcher(&store, &wishes, 0);
            let mut names: Vec<String> = store.known().filter(|(_, s)| matches(s)).map(|(_, s)| s.name.clone()).collect();
            names.sort();
            names
        };
        assert_eq!(wished(Field::Wished, Op::IsTrue, ""), ["extra.ogg", "matched.ogg", "tagged.ogg", "unknown.ogg"], "the album's, the song's and the name's");
        assert_eq!(wished(Field::Wish, Op::Is, "nujabes"), ["extra.ogg", "matched.ogg"], "an artist's are their albums'");
        assert_eq!(wished(Field::Wish, Op::Like, "kondo"), ["unknown.ogg"], "one of just an artist goes by their name");
    }

    fn names(store: &SongStore, keys: &[String]) -> Vec<String> {
        keys.iter().map(|k| store.known().find(|(key, _)| key == k).unwrap().1.name.clone()).collect()
    }

    #[test]
    fn rules_match_all_or_any() {
        let lib = library();
        let rule = |field, op, value: &str| Rule { field, op, value: value.into(), to: String::new() };
        let mut rules = Rules { rules: vec![rule(Field::Artist, Op::Is, "NUJABES"), rule(Field::Starred, Op::IsTrue, "")], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 0, false)), ["a.ogg"]);
        rules.any = true;
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 0, false)), ["a.ogg", "b.ogg", "c.ogg", "d.ogg"]);
        let rules = Rules { rules: vec![rule(Field::Album, Op::IsEmpty, "")], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 0, false)), ["d.ogg", "e.ogg"]);
        let rules = Rules { rules: vec![Rule { field: Field::Length, op: Op::InRange, value: "3:00".into(), to: "4:10".into() }], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 0, false)), ["a.ogg", "c.ogg"]);
        let rules = Rules { rules: vec![rule(Field::Imported, Op::InLast, "1")], ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 86_400 + 300, false)), ["c.ogg", "d.ogg", "e.ogg"]);
        assert_eq!(parse_period("1970-01-02"), Some((86_400, 2 * 86_400)));
        assert_eq!(parse_period("2026-10-20").map(|(start, _)| fields::date(start)).as_deref(), Some("2026-10-20"));
        assert_eq!(parse_period("2026-12").map(|(a, b)| (fields::date(a), fields::date(b))), Some(("2026-12-01".into(), "2027-01-01".into())));
        assert_eq!(parse_period("2026").map(|(_, b)| fields::date(b)).as_deref(), Some("2027-01-01"));
    }

    #[test]
    fn values_with_units() {
        assert_eq!(parse(Field::Length, "6:00"), Some(360.0));
        assert_eq!(parse(Field::Length, "1:02:30"), Some(3750.0));
        assert_eq!(parse(Field::Length, "6"), Some(360.0), "minutes");
        assert_eq!(parse(Field::Length, "6m30s"), Some(390.0));
        assert_eq!(parse(Field::Length, "1h 2m"), Some(3720.0));
        assert_eq!(parse(Field::Length, "90s"), Some(90.0));
        assert_eq!(parse(Field::Length, "6x"), None);
        assert_eq!(parse(Field::Size, "1.5GB"), Some(1500.0));
        assert_eq!(parse(Field::Size, "800kb"), Some(0.8));
        assert_eq!(parse(Field::Size, "5"), Some(5.0));
        assert_eq!(parse_age("2w"), Some(14.0 * 86_400.0));
        assert_eq!(parse_age("6mo"), Some(180.0 * 86_400.0));
        assert_eq!(parse_age("30"), Some(30.0 * 86_400.0));
    }

    #[test]
    fn limits_fill_in_their_order() {
        let lib = library();
        let rules = Rules { limit: Some(Limit { amount: 2.0, unit: Unit::Items, by: Order::NewestAdded }), ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 0, false)), ["d.ogg", "e.ogg"], "newest two, by title");
        // 9 minutes, by album: 240 and 300 fit, 200 doesn't, the one of
        // unknown length does, 100 doesn't.
        let rules = Rules { limit: Some(Limit { amount: 9.0, unit: Unit::Minutes, by: Order::Album }), order: Order::Album, ..Rules::default() };
        assert_eq!(names(&lib, &rules.songs(&lib, &Wishes::new(), 0, false)), ["c.ogg", "b.ogg", "e.ogg"]);
    }

    #[test]
    fn random_is_seeded_and_stable() {
        let lib = library();
        let rules = Rules { order: Order::Random, seed: 7, ..Rules::default() };
        let first = rules.songs(&lib, &Wishes::new(), 0, false);
        assert_eq!(first, rules.songs(&lib, &Wishes::new(), 0, false), "the same every time");
        let reseeded = Rules { seed: 8, ..rules.clone() };
        let mut a = first.clone();
        let mut b = reseeded.songs(&lib, &Wishes::new(), 0, false);
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
        let mut more = rules.songs(&all, &Wishes::new(), 0, false);
        let new = format!("{:064x}.ogg", 6);
        more.retain(|k| *k != new);
        assert_eq!(more, first);
    }

    #[test]
    fn rulesets_split_into_playlists() {
        let lib = library();
        let [(_, artists), (_, albums)] = builtin();
        let groups = artists.rules.groups(&lib, &Wishes::new(), 0, "", false);
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Basil Poledouris", "Nujabes"], "case-insensitive, songs without an artist in none");
        assert_eq!(names(&lib, &groups[1].songs), ["c.ogg", "b.ogg", "a.ogg"], "by album, then track");
        let groups = albums.rules.groups(&lib, &Wishes::new(), 0, "", false);
        let titles: Vec<(&str, Option<&str>)> = groups.iter().map(|g| (g.title.as_str(), g.subtitle.as_deref())).collect();
        assert_eq!(titles, [("Metaphorical Music", Some("nujabes")), ("Modal Soul", Some("Nujabes"))]);
        assert_eq!(names(&lib, &groups[1].songs), ["b.ogg", "a.ogg"]);
        let at_least_two = Rules { split: Some(Split { by: SplitBy::AlbumArtist, min: 2 }), ..artists.rules.clone() };
        assert_eq!(at_least_two.groups(&lib, &Wishes::new(), 0, "", false).len(), 1);
        // "<OST>" and "OST" are one album, under the spelling most of its
        // songs have.
        let ost = store(&[
            ("a.ogg", meta("Composer", "<OST>", 1, 1.0), &[], 0),
            ("b.ogg", meta("COMPOSER", "OST", 2, 1.0), &[], 0),
            ("c.ogg", meta("composer", "<OST>", 3, 1.0), &[], 0),
        ]);
        let groups = albums.rules.groups(&ost, &Wishes::new(), 0, "", false);
        let titles: Vec<(&str, Option<&str>)> = groups.iter().map(|g| (g.title.as_str(), g.subtitle.as_deref())).collect();
        assert_eq!(titles, [("<OST>", Some("Composer"))]);
        assert_eq!(names(&ost, &groups[0].songs), ["a.ogg", "b.ogg", "c.ogg"]);
        // A soundtrack by several artists is one album, in track order;
        // so is one with its album artist spelled two ways.
        let various = store(&[
            ("a.ogg", meta("Kan Gao", "Finding Paradise <OST>", 2, 1.0), &[], 0),
            ("b.ogg", meta("Laura Shigihara", "Finding Paradise OST", 1, 1.0), &[], 0),
            ("c.ogg", Meta { album_artist: Some("Kan R. Gao".into()), ..meta("Kan Gao", "A Bird Story", 2, 1.0) }, &[], 0),
            ("d.ogg", Meta { album_artist: Some("Kan (Reives) Gao".into()), ..meta("Kan Gao", "A Bird Story", 1, 1.0) }, &[], 0),
        ]);
        let groups = albums.rules.groups(&various, &Wishes::new(), 0, "", false);
        let titles: Vec<(&str, Option<&str>)> = groups.iter().map(|g| (g.title.as_str(), g.subtitle.as_deref())).collect();
        assert_eq!(titles, [("A Bird Story", Some("Kan (Reives) Gao")), ("Finding Paradise <OST>", Some(VARIOUS_ARTISTS))]);
        assert_eq!(names(&various, &groups[0].songs), ["d.ogg", "c.ogg"]);
        assert_eq!(names(&various, &groups[1].songs), ["b.ogg", "a.ogg"]);
        // An album by two album artists names both, and is in each one's
        // playlist of By Artist.
        let duo = store(&[("a.ogg", Meta { album_artist: Some("Uyama Hiroto; Nujabes".into()), ..meta("Nujabes", "Modal Soul", 1, 1.0) }, &[], 0)]);
        let groups = albums.rules.groups(&duo, &Wishes::new(), 0, "", false);
        assert_eq!((groups[0].subtitle.as_deref(), groups[0].artists.as_slice()), (Some("Uyama Hiroto & Nujabes"), &["Uyama Hiroto".to_string(), "Nujabes".to_string()][..]));
        let titles: Vec<String> = artists.rules.groups(&duo, &Wishes::new(), 0, "", false).into_iter().map(|g| g.title).collect();
        assert_eq!(titles, ["Nujabes", "Uyama Hiroto"]);
        // A compilation (with no album artist) is by Various Artists, and
        // filed whole under them.
        let compilation = store(&[
            ("a.ogg", Meta { compilation: true, ..meta("Nujabes", "Hydeout Productions", 1, 1.0) }, &[], 0),
            ("b.ogg", meta("Nujabes", "Hydeout Productions", 2, 1.0), &[], 0),
        ]);
        assert_eq!(albums.rules.groups(&compilation, &Wishes::new(), 0, "", false)[0].subtitle.as_deref(), Some(VARIOUS_ARTISTS));
        let titles: Vec<String> = artists.rules.groups(&compilation, &Wishes::new(), 0, "", false).into_iter().map(|g| g.title).collect();
        assert_eq!(titles, ["Nujabes", VARIOUS_ARTISTS], "only the song that says so");
        // A song is in each of its genres.
        let genres = store(&[
            ("a.ogg", Meta { genre: Some("Avant-Garde, Rock".into()), ..Meta::default() }, &[], 0),
            ("b.ogg", Meta { genre: Some("rock; Classical".into()), ..Meta::default() }, &[], 0),
        ]);
        let by_genre = Rules { split: Some(Split { by: SplitBy::Genre, min: 1 }), ..Rules::default() };
        let groups = by_genre.groups(&genres, &Wishes::new(), 0, "", false);
        let found: Vec<(&str, usize)> = groups.iter().map(|g| (g.title.as_str(), g.songs.len())).collect();
        assert_eq!(found, [("Avant-Garde", 1), ("Classical", 1), ("Rock", 2)]);
        let rock = Rules { rules: vec![Rule { field: Field::Genre, op: Op::Is, value: "rock".into(), to: String::new() }], ..Rules::default() };
        assert_eq!(names(&genres, &rock.songs(&genres, &Wishes::new(), 0, false)), ["a.ogg", "b.ogg"]);
    }

    #[test]
    fn missing_what_a_ruleset_splits_by() {
        let songs = store(&[
            ("a.ogg", meta("Nujabes", "Modal Soul", 1, 1.0), &[], 0),
            ("b.ogg", meta("Nujabes", "", 2, 1.0), &[], 0),
            ("c.ogg", Meta { album: Some("Hydeout".into()), ..Meta::default() }, &[], 0),
        ]);
        let [(_, artists), (_, albums)] = builtin();
        assert_eq!(names(&songs, &albums.rules.missing(&songs, &Wishes::new(), 0)), ["b.ogg"]);
        assert_eq!(names(&songs, &artists.rules.missing(&songs, &Wishes::new(), 0)), ["c.ogg"]);
        let by_tag = Rules { split: Some(Split { by: SplitBy::Tag, min: 1 }), ..Rules::default() };
        assert!(by_tag.missing(&songs, &Wishes::new(), 0).is_empty(), "no tag isn't missing anything");
        assert!(Rules::default().missing(&songs, &Wishes::new(), 0).is_empty(), "not a ruleset");
    }
}
