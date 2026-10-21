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
use crate::song_store::{Song, SongStore};

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
fn parse(field: Field, text: &str) -> Option<f64> {
    let text = text.trim();
    if field == Field::Length
        && let Some((m, s)) = text.split_once(':')
    {
        return Some(m.trim().parse::<f64>().ok()? * 60.0 + s.trim().parse::<f64>().ok()?);
    }
    text.parse().ok()
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

/// The ops for a kind of field (the first is a new rule's).
pub fn ops(kind: Kind) -> &'static [Op] {
    use Op::*;
    match kind {
        Kind::Text => &[Contains, NotContains, Is, IsNot, StartsWith, EndsWith, IsEmpty, IsNotEmpty],
        Kind::Number => &[Is, IsNot, Greater, Less, InRange, IsEmpty, IsNotEmpty],
        Kind::Date => &[InLast, NotInLast, Before, After],
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

/// A playlist a ruleset made: what tells it apart (its value's
/// `fields::group_key`, and its subtitle's), its title, a line under that (an album's artist), and its
/// songs by key.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: String,
    pub title: String,
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
            _ => parse(rule.field, text),
        };
        Self { rule, text: rule.value.trim().to_lowercase(), numbers: [parse(&rule.value), parse(&rule.to)] }
    }

    fn matches(&self, song: &Song) -> bool {
        let Rule { field, op, value, .. } = self.rule;
        let song = Facts::of(song);
        match field.kind() {
            Kind::Text => {
                // (Each of a list field's items: a song's genre "is" each
                // of its genres.)
                let have: Vec<String> = match field {
                    Field::Title => text(&song, *field).into_iter().map(|t| t.to_lowercase()).collect(),
                    _ => song.items(*field).iter().map(|t| t.to_lowercase()).collect(),
                };
                let want = self.text.as_str();
                let any = |test: &dyn Fn(&str) -> bool| have.iter().any(|h| test(h));
                match op {
                    Op::IsEmpty => have.is_empty(),
                    Op::IsNotEmpty => !have.is_empty(),
                    Op::Is => any(&|h| h == want),
                    Op::IsNot => !any(&|h| h == want),
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
                    (Op::InRange, Some(h), [Some(a), Some(b)]) => (a.min(b)..=a.max(b)).contains(&h),
                    _ => false,
                }
            }
            Kind::Date => {
                let Some(have) = song.number(*field) else { return false };
                match (op, self.numbers[0]) {
                    (Op::InLast, Some(since)) => have >= since,
                    (Op::NotInLast, Some(since)) => have < since,
                    (Op::Before, Some(day)) => have < day,
                    (Op::After, Some(day)) => have >= day + 86_400.0,
                    _ => false,
                }
            }
            Kind::Tag => {
                let has = song.has_tag(value);
                if *op == Op::HasNot { !has } else { has }
            }
            Kind::Flag => {
                let set = song.flag(*field);
                if *op == Op::IsFalse { !set } else { set }
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
    let facts = Facts::of(song);
    let lower = |field: Field| text(&facts, field).map(|t| t.to_lowercase()).unwrap_or_else(|| "\u{10ffff}".into());
    // (Spellings of one name together, as they're grouped.)
    let name = |field: Field| text(&facts, field).map(|t| fields::group_key(&t, "")).unwrap_or_else(|| "\u{10ffff}".into());
    let title = lower(Field::Title);
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
    /// each. Values are told apart by `fields::group_key` (past the
    /// characters in `ignore`), each playlist titled with the best of its
    /// songs' spellings.
    pub fn groups(&self, store: &SongStore, now: i64, ignore: &str) -> Vec<Group> {
        /// A playlist being made: its songs' spellings of its title and
        /// subtitle (an album's: its album artists', else its artists'),
        /// and its songs.
        #[derive(Default)]
        struct Making<'a> {
            titles: Vec<Cow<'a, str>>,
            subtitles: Vec<Cow<'a, str>>,
            artists: Vec<Cow<'a, str>>,
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
        for (key, song) in self.matching(candidates, now) {
            let facts = Facts::of(song);
            if split.by == SplitBy::Album {
                if let Some(album) = facts.text(Field::Album) {
                    let on = (key, song, album, facts.text(Field::AlbumArtist), facts.first(Field::Artist));
                    albums.entry(fields::group_key(&on.2, ignore)).or_default().push(on);
                }
                continue;
            }
            let each = |field: Field| facts.items(field).into_iter().map(|t| (t, None)).collect();
            let values: Vec<(Cow<str>, Option<Cow<str>>)> = match split.by {
                SplitBy::Artist => each(Field::Artist),
                SplitBy::AlbumArtist => filed_under(&facts).into_iter().map(|a| (a, None)).collect(),
                SplitBy::Album => unreachable!("albums are made below"),
                SplitBy::Composer => each(Field::Composer),
                SplitBy::Genre => each(Field::Genre),
                SplitBy::Year => facts.value(Field::Year).map(|y| (Cow::Owned(y.text), None)).into_iter().collect(),
                SplitBy::Tag => song.tags.iter().filter_map(|id| tags.get(id)).map(|t| (Cow::Borrowed(t.name.as_str()), None)).collect(),
            };
            for (title, subtitle) in values {
                let group_of = format!("{}\u{0}{}", fields::group_key(&title, ignore), subtitle.as_deref().map(|s| fields::group_key(s, ignore)).unwrap_or_default());
                let making = groups.entry(group_of).or_default();
                making.titles.push(title);
                making.subtitles.extend(subtitle);
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
                making.songs.push((key, song));
            }
        }
        groups
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
                Group {
                    key,
                    title: fields::best_spelling(making.titles.iter().map(|t| t.as_ref())).unwrap_or_default().to_string(),
                    subtitle,
                    artists,
                    songs: self.limited(making.songs).into_iter().map(|(key, _)| key.to_string()).collect(),
                }
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
        assert_eq!(parse_date("2026-10-20").map(fields::date).as_deref(), Some("2026-10-20"));
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
        let groups = artists.rules.groups(&lib, 0, "");
        let titles: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Basil Poledouris", "Nujabes"], "case-insensitive, songs without an artist in none");
        assert_eq!(names(&lib, &groups[1].songs), ["c.ogg", "b.ogg", "a.ogg"], "by album, then track");
        let groups = albums.rules.groups(&lib, 0, "");
        let titles: Vec<(&str, Option<&str>)> = groups.iter().map(|g| (g.title.as_str(), g.subtitle.as_deref())).collect();
        assert_eq!(titles, [("Metaphorical Music", Some("nujabes")), ("Modal Soul", Some("Nujabes"))]);
        assert_eq!(names(&lib, &groups[1].songs), ["b.ogg", "a.ogg"]);
        let at_least_two = Rules { split: Some(Split { by: SplitBy::AlbumArtist, min: 2 }), ..artists.rules.clone() };
        assert_eq!(at_least_two.groups(&lib, 0, "").len(), 1);
        // "<OST>" and "OST" are one album, under the spelling most of its
        // songs have.
        let ost = store(&[
            ("a.ogg", meta("Composer", "<OST>", 1, 1.0), &[], 0),
            ("b.ogg", meta("COMPOSER", "OST", 2, 1.0), &[], 0),
            ("c.ogg", meta("composer", "<OST>", 3, 1.0), &[], 0),
        ]);
        let groups = albums.rules.groups(&ost, 0, "");
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
        let groups = albums.rules.groups(&various, 0, "");
        let titles: Vec<(&str, Option<&str>)> = groups.iter().map(|g| (g.title.as_str(), g.subtitle.as_deref())).collect();
        assert_eq!(titles, [("A Bird Story", Some("Kan (Reives) Gao")), ("Finding Paradise <OST>", Some(VARIOUS_ARTISTS))]);
        assert_eq!(names(&various, &groups[0].songs), ["d.ogg", "c.ogg"]);
        assert_eq!(names(&various, &groups[1].songs), ["b.ogg", "a.ogg"]);
        // An album by two album artists names both, and is in each one's
        // playlist of By Artist.
        let duo = store(&[("a.ogg", Meta { album_artist: Some("Uyama Hiroto; Nujabes".into()), ..meta("Nujabes", "Modal Soul", 1, 1.0) }, &[], 0)]);
        let groups = albums.rules.groups(&duo, 0, "");
        assert_eq!((groups[0].subtitle.as_deref(), groups[0].artists.as_slice()), (Some("Uyama Hiroto & Nujabes"), &["Uyama Hiroto".to_string(), "Nujabes".to_string()][..]));
        let titles: Vec<String> = artists.rules.groups(&duo, 0, "").into_iter().map(|g| g.title).collect();
        assert_eq!(titles, ["Nujabes", "Uyama Hiroto"]);
        // A compilation (with no album artist) is by Various Artists, and
        // filed whole under them.
        let compilation = store(&[
            ("a.ogg", Meta { compilation: true, ..meta("Nujabes", "Hydeout Productions", 1, 1.0) }, &[], 0),
            ("b.ogg", meta("Nujabes", "Hydeout Productions", 2, 1.0), &[], 0),
        ]);
        assert_eq!(albums.rules.groups(&compilation, 0, "")[0].subtitle.as_deref(), Some(VARIOUS_ARTISTS));
        let titles: Vec<String> = artists.rules.groups(&compilation, 0, "").into_iter().map(|g| g.title).collect();
        assert_eq!(titles, ["Nujabes", VARIOUS_ARTISTS], "only the song that says so");
        // A song is in each of its genres.
        let genres = store(&[
            ("a.ogg", Meta { genre: Some("Avant-Garde, Rock".into()), ..Meta::default() }, &[], 0),
            ("b.ogg", Meta { genre: Some("rock; Classical".into()), ..Meta::default() }, &[], 0),
        ]);
        let by_genre = Rules { split: Some(Split { by: SplitBy::Genre, min: 1 }), ..Rules::default() };
        let groups = by_genre.groups(&genres, 0, "");
        let found: Vec<(&str, usize)> = groups.iter().map(|g| (g.title.as_str(), g.songs.len())).collect();
        assert_eq!(found, [("Avant-Garde", 1), ("Classical", 1), ("Rock", 2)]);
        let rock = Rules { rules: vec![Rule { field: Field::Genre, op: Op::Is, value: "rock".into(), to: String::new() }], ..Rules::default() };
        assert_eq!(names(&genres, &rock.songs(&genres, 0)), ["a.ogg", "b.ogg"]);
    }
}
