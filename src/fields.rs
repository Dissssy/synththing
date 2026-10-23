//! A song's fields, defined once ([`FIELDS`]): the name each goes by in
//! song name patterns (`{album_artist}`), the Songs list's columns and
//! smart playlist rules, its label, what kind of value it holds, and
//! whether every device knows it.
//!
//! [`Facts`] works out a song's value for any of them from what's known
//! of it: the synced library's `song_store::Meta` (what the file said of
//! itself on the device that read it first), else what's been read of
//! the file here (a song played from outside the library), and what only
//! a read here says (a MIDI file's notes and tempo).
//!
//! Grouping by a name (the Songs list's groups, rulesets' playlists) goes
//! by [`group_key`], which sees past case, spacing and punctuation, so
//! "<OST>" and "OST" are one group, shown as its best spelling
//! ([`best_spelling`]).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::song_info::{format_length, thousands, SongInfo};
use crate::song_store::{self, Meta, STARRED};

/// A song field. Kept (in smart playlist rules) by its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Composer,
    Genre,
    Track,
    Disc,
    Year,
    /// What it's grouped under (a work, a movement).
    Grouping,
    Comment,
    /// Seconds.
    Length,
    Bpm,
    Notes,
    /// Without its extension.
    FileName,
    /// In lowercase.
    FileExtension,
    /// MB.
    Size,
    /// kbps, on average: its size over its length (not MIDI's).
    BitRate,
    /// Hz.
    SampleRate,
    /// When it came into the library (Unix seconds).
    Imported,
    Tag,
    Starred,
    /// Its album's songs are by various artists: it's filed under
    /// Various Artists, unless an album artist says otherwise.
    Compilation,
    /// Its composer's who it's by (classical music): its artist is its
    /// composer.
    ComposerAsArtist,
    /// What its title, album, artist, album artist and composer sort as
    /// ("Beatles" for "The Beatles"), if not themselves.
    SortTitle,
    SortAlbum,
    SortArtist,
    SortAlbumArtist,
    SortComposer,
    /// Where it starts playing, and stops (seconds; edits only).
    StartTime,
    StopTime,
    /// Its volume, up or down (-100 to 100%).
    VolumeAdjust,
    /// Shuffling passes over it.
    SkipShuffle,
    /// It starts again where it was left (`song_store::Song::position`).
    RememberPosition,
    /// The ID of the release (album) it's on at MusicBrainz, as its tags
    /// say.
    #[serde(rename = "musicbrainz_release")]
    MusicBrainzRelease,
    /// The program that added it through the local API (`api.rs`).
    AddedBy,
}

/// Where Song info shows an editable field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Details,
    /// What names sort as.
    Sorting,
    /// How it plays.
    Options,
}

/// What a field's values are, which says how they're shown, sorted and
/// compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    Date,
    /// The song's tags (`song_store::Tag`).
    Tag,
    Flag,
}

/// A field's definition.
pub struct Def {
    pub field: Field,
    /// Its name in patterns, the list's view and rules.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// Every device knows it (it's in the synced library), so rules can
    /// go by it.
    pub synced: bool,
    /// Its column's width in the Songs list (0: it isn't one).
    pub width: f32,
    /// Songs share its values often enough to group the list by them.
    pub groups: bool,
    /// Its text is a list, its items split at these (`Def::split`):
    /// "Electronic, Rock" is two genres, and a song is in each one's
    /// group. Artists split only at semicolons (and the NULs ID3v2.4
    /// separates values with): a comma or "&" can be in a name ("Tyler,
    /// The Creator", "Simon & Garfunkel"), so those are only suggested
    /// as splits (`cleanup`).
    pub list: &'static [char],
    /// A song's edits (`Edits`) can say what it is, over what the file
    /// says (if it says).
    pub editable: bool,
}

#[allow(clippy::too_many_arguments)]
const fn def(field: Field, key: &'static str, label: &'static str, kind: Kind, synced: bool, width: f32, groups: bool, list: &'static [char], editable: bool) -> Def {
    Def { field, key, label, kind, synced, width, groups, list, editable }
}

/// A genre list's separators: "Electronic, Rock; Jazz".
const GENRE_LIST: &[char] = &[',', ';', '\0'];
/// A list of names' separators: "Perry Porter; Nujabes".
const NAME_LIST: &[char] = &[';', '\0'];

/// Every field, in [`Field`]'s order.
pub const FIELDS: &[Def] = &[
    def(Field::Title, "title", "Title", Kind::Text, true, 0.0, false, &[], true),
    def(Field::Artist, "artist", "Artist", Kind::Text, true, 140.0, true, NAME_LIST, true),
    def(Field::AlbumArtist, "album_artist", "Album artist", Kind::Text, true, 140.0, true, NAME_LIST, true),
    def(Field::Album, "album", "Album", Kind::Text, true, 140.0, true, &[], true),
    def(Field::Composer, "composer", "Composer", Kind::Text, true, 140.0, true, NAME_LIST, true),
    def(Field::Genre, "genre", "Genre", Kind::Text, true, 100.0, true, GENRE_LIST, true),
    def(Field::Track, "track", "Track", Kind::Number, true, 48.0, false, &[], true),
    def(Field::Disc, "disc", "Disc", Kind::Number, true, 40.0, true, &[], true),
    def(Field::Year, "year", "Year", Kind::Number, true, 52.0, true, &[], true),
    def(Field::Grouping, "grouping", "Grouping", Kind::Text, true, 120.0, true, &[], true),
    def(Field::Comment, "comment", "Comment", Kind::Text, true, 160.0, false, &[], true),
    def(Field::Length, "length", "Length", Kind::Number, true, 60.0, false, &[], false),
    def(Field::Bpm, "bpm", "BPM", Kind::Number, false, 52.0, false, &[], false),
    def(Field::Notes, "notes", "Notes", Kind::Number, false, 72.0, false, &[], false),
    def(Field::FileName, "file_name", "File name", Kind::Text, true, 160.0, false, &[], false),
    def(Field::FileExtension, "file_extension", "Type", Kind::Text, true, 52.0, true, &[], false),
    def(Field::Size, "size", "Size (MB)", Kind::Number, true, 72.0, false, &[], false),
    def(Field::BitRate, "bit_rate", "Bit rate (kbps)", Kind::Number, true, 64.0, false, &[], false),
    def(Field::SampleRate, "sample_rate", "Sample rate", Kind::Number, true, 80.0, true, &[], false),
    def(Field::Imported, "imported", "Date added", Kind::Date, true, 96.0, true, &[], false),
    def(Field::Tag, "tag", "Tag", Kind::Tag, true, 0.0, false, &[], false),
    def(Field::Starred, "starred", "Starred", Kind::Flag, true, 0.0, false, &[], false),
    def(Field::Compilation, "compilation", "Compilation", Kind::Flag, true, 0.0, false, &[], true),
    def(Field::ComposerAsArtist, "composer_as_artist", "Use composer as artist", Kind::Flag, true, 0.0, false, &[], true),
    def(Field::SortTitle, "sort_title", "Sort title as", Kind::Text, true, 140.0, false, &[], true),
    def(Field::SortAlbum, "sort_album", "Sort album as", Kind::Text, true, 140.0, false, &[], true),
    def(Field::SortArtist, "sort_artist", "Sort artist as", Kind::Text, true, 140.0, false, &[], true),
    def(Field::SortAlbumArtist, "sort_album_artist", "Sort album artist as", Kind::Text, true, 140.0, false, &[], true),
    def(Field::SortComposer, "sort_composer", "Sort composer as", Kind::Text, true, 140.0, false, &[], true),
    def(Field::StartTime, "start_time", "Start at", Kind::Number, true, 0.0, false, &[], true),
    def(Field::StopTime, "stop_time", "Stop at", Kind::Number, true, 0.0, false, &[], true),
    def(Field::VolumeAdjust, "volume_adjust", "Volume adjust (%)", Kind::Number, true, 0.0, false, &[], true),
    def(Field::SkipShuffle, "skip_shuffle", "Skip when shuffling", Kind::Flag, true, 0.0, false, &[], true),
    def(Field::RememberPosition, "remember_position", "Remember position", Kind::Flag, true, 0.0, false, &[], true),
    def(Field::MusicBrainzRelease, "musicbrainz_release", "MusicBrainz release", Kind::Text, true, 0.0, false, &[], false),
    def(Field::AddedBy, "added_by", "Added by", Kind::Text, true, 120.0, true, &[], false),
];

impl Def {
    pub fn is_list(&self) -> bool {
        !self.list.is_empty()
    }

    /// A list field's items: its text split at its separators, trimmed
    /// (a field that isn't a list is its one item).
    pub fn split<'t>(&self, text: &'t str) -> impl Iterator<Item = &'t str> {
        text.split(self.list).map(str::trim).filter(|t| !t.is_empty())
    }

    /// Names written to be read: "Uyama Hiroto & Nujabes".
    pub fn names<S: AsRef<str>>(names: &[S]) -> String {
        names.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(" & ")
    }

    /// Items written as a list field's text, by its first separator.
    pub fn join<S: AsRef<str>>(&self, items: &[S]) -> String {
        let items: Vec<&str> = items.iter().map(AsRef::as_ref).collect();
        match self.list.first() {
            Some(sep) => items.join(&format!("{sep} ")),
            None => items.concat(),
        }
    }
}

impl Field {
    pub fn def(self) -> &'static Def {
        &FIELDS[self as usize]
    }

    pub fn label(self) -> &'static str {
        self.def().label
    }

    pub fn kind(self) -> Kind {
        self.def().kind
    }

    /// The field named `key`.
    pub fn by_key(key: &str) -> Option<Self> {
        FIELDS.iter().find(|d| d.key == key).map(|d| d.field)
    }

    /// The field saying what this one sorts as, for one that has one.
    pub fn sorted_as(self) -> Option<Self> {
        match self {
            Field::Title => Some(Field::SortTitle),
            Field::Album => Some(Field::SortAlbum),
            Field::Artist => Some(Field::SortArtist),
            Field::AlbumArtist => Some(Field::SortAlbumArtist),
            Field::Composer => Some(Field::SortComposer),
            _ => None,
        }
    }

    /// Where Song info shows it.
    pub fn section(self) -> Section {
        match self {
            Field::SortTitle | Field::SortAlbum | Field::SortArtist | Field::SortAlbumArtist | Field::SortComposer => Section::Sorting,
            Field::StartTime | Field::StopTime | Field::VolumeAdjust | Field::SkipShuffle | Field::RememberPosition => Section::Options,
            _ => Section::Details,
        }
    }

    /// Whether a song name pattern can use it: it has a value to show.
    pub fn in_patterns(self) -> bool {
        matches!(self.kind(), Kind::Text | Kind::Number | Kind::Date)
    }
}

/// The fields a song's edits can change, in order.
pub fn editable() -> impl Iterator<Item = &'static Def> {
    FIELDS.iter().filter(|d| d.editable)
}

/// The Songs list's columns.
pub fn columns() -> impl Iterator<Item = &'static Def> {
    FIELDS.iter().filter(|d| d.width > 0.0)
}

/// The fields rules can go by.
pub fn rule_fields() -> Vec<Field> {
    FIELDS.iter().filter(|d| d.synced).map(|d| d.field).collect()
}

/// A field's value as shown, and as a number for those sorted as one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Value {
    pub text: String,
    pub number: Option<f64>,
    /// What it sorts as, when a song says (`Field::sorted_as`).
    pub sort: Option<String>,
}

/// What's known of a song, which its fields' values come from.
#[derive(Clone, Copy, Default)]
pub struct Facts<'a> {
    /// Its original file name, extension included.
    pub file_name: &'a str,
    /// What its file says of itself: the library's, else what's been read
    /// of it here.
    pub meta: Option<&'a Meta>,
    /// What's been read of it here, for what only that knows.
    pub info: Option<&'a SongInfo>,
    pub imported: Option<i64>,
    /// Its tags, by ID.
    pub tags: &'a [String],
    /// What the user says of it over what its file does.
    pub edits: Option<&'a Edits>,
    /// The program that added it through the local API, if one did.
    pub added_by: Option<&'a str>,
}

/// A song's edits (in the synced library, `song_store::Song::edits`): an
/// editable field's text, over what its file says. Empty clears it.
/// The file itself is never written: its hash is the song.
pub type Edits = BTreeMap<Field, String>;

impl<'a> Facts<'a> {
    /// A song in the library, as every device knows it.
    pub fn of(song: &'a song_store::Song) -> Self {
        Self { file_name: &song.name, meta: song.meta.as_ref(), info: None, imported: Some(song.imported), tags: &song.tags, edits: Some(&song.edits), added_by: song.added_by.as_deref() }
    }

    /// What the song's edits say `field` is, if they do.
    pub fn edit(&self, field: Field) -> Option<&'a str> {
        self.edits?.get(&field).map(String::as_str)
    }

    /// What its file says, without its edits.
    pub fn unedited(&self) -> Self {
        Self { edits: None, ..*self }
    }

    /// A text field's value (trimmed; `None` when empty).
    pub fn text(&self, field: Field) -> Option<Cow<'a, str>> {
        let meta = self.meta;
        let text = |t: Option<&'a Option<String>>| t.and_then(|t| t.as_deref()).map(Cow::Borrowed);
        let edit = self.edit(field).map(|e| Some(Cow::Borrowed(e)));
        let value = match field {
            Field::Genre => edit.unwrap_or_else(|| text(meta.map(|m| &m.genre))).map(|g| match genre_names(&g) {
                Cow::Borrowed(_) => g,
                Cow::Owned(named) => Cow::Owned(named),
            }),
            Field::Artist if self.flag(Field::ComposerAsArtist) && self.text(Field::Composer).is_some() => return self.text(Field::Composer),
            _ if edit.is_some() => edit.flatten(),
            Field::Title => text(meta.map(|m| &m.title)),
            Field::Artist => text(meta.map(|m| &m.artist)),
            Field::AlbumArtist => text(meta.map(|m| &m.album_artist)),
            Field::Album => text(meta.map(|m| &m.album)),
            Field::Composer => text(meta.map(|m| &m.composer)),
            Field::SortTitle => text(meta.map(|m| &m.sort_title)),
            Field::SortAlbum => text(meta.map(|m| &m.sort_album)),
            Field::SortArtist => text(meta.map(|m| &m.sort_artist)),
            Field::SortAlbumArtist => text(meta.map(|m| &m.sort_album_artist)),
            Field::SortComposer => text(meta.map(|m| &m.sort_composer)),
            Field::Grouping => text(meta.map(|m| &m.grouping)),
            Field::Comment => text(meta.map(|m| &m.comment)),
            Field::MusicBrainzRelease => text(meta.map(|m| &m.musicbrainz_release)),
            Field::FileName => Some(Cow::Borrowed(Path::new(self.file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(self.file_name))),
            Field::FileExtension => Path::new(self.file_name).extension().map(|e| Cow::Owned(e.to_string_lossy().to_lowercase())),
            Field::AddedBy => self.added_by.map(Cow::Borrowed),
            _ => None,
        }?;
        match value {
            Cow::Borrowed(t) => Some(Cow::Borrowed(t.trim())),
            Cow::Owned(t) => Some(Cow::Owned(t.trim().to_string())),
        }
        .filter(|t| !t.is_empty())
    }

    /// A number or date field's value.
    pub fn number(&self, field: Field) -> Option<f64> {
        let meta = self.meta;
        if let Some(edit) = self.edit(field) {
            return match field {
                Field::StartTime | Field::StopTime => seconds(edit),
                Field::VolumeAdjust => edit.trim().trim_end_matches('%').trim().parse::<f64>().ok().map(|n| n.clamp(-100.0, 100.0)),
                _ => crate::song_info::leading_number(edit).map(f64::from),
            };
        }
        match field {
            Field::Track => meta?.track.map(f64::from),
            Field::Disc => meta?.disc.map(f64::from),
            Field::Year => meta?.year.map(f64::from),
            Field::Length => meta?.length,
            Field::Bpm => self.info?.bpm,
            Field::Notes => self.info?.notes.map(|n| n as f64),
            Field::Size => meta.filter(|m| m.size > 0).map(|m| m.size as f64 / 1e6),
            Field::BitRate if !self.is_midi() => meta.filter(|m| m.size > 0).and_then(|m| Some(m.size as f64 * 8.0 / m.length.filter(|&l| l > 0.0)? / 1000.0)),
            Field::SampleRate => meta?.sample_rate.map(f64::from),
            Field::Imported => self.imported.map(|secs| secs as f64),
            _ => None,
        }
    }

    /// A text field's values: its items for a list field (`Def::list`),
    /// else its text.
    pub fn items(&self, field: Field) -> Vec<Cow<'a, str>> {
        let def = field.def();
        match self.text(field) {
            Some(Cow::Borrowed(text)) if def.is_list() => def.split(text).map(Cow::Borrowed).collect(),
            Some(Cow::Owned(text)) if def.is_list() => def.split(&text).map(|t| Cow::Owned(t.to_string())).collect(),
            text => text.into_iter().collect(),
        }
    }

    /// A list field's first item (the artist a song's mostly by), else
    /// its text.
    pub fn first(&self, field: Field) -> Option<Cow<'a, str>> {
        self.items(field).into_iter().next()
    }

    /// Whether it's a MIDI file.
    fn is_midi(&self) -> bool {
        Path::new(self.file_name).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"))
    }

    pub fn has_tag(&self, id: &str) -> bool {
        self.tags.iter().any(|t| t == id)
    }

    pub fn starred(&self) -> bool {
        self.has_tag(STARRED)
    }

    /// A flag field's value. An edit says yes with any text but "" or "0".
    pub fn flag(&self, field: Field) -> bool {
        if let Some(edit) = self.edit(field) {
            return !matches!(edit.trim(), "" | "0");
        }
        match field {
            Field::Starred => self.starred(),
            Field::Compilation => self.meta.is_some_and(|m| m.compilation),
            _ => false,
        }
    }

    /// A field's value as shown (tags and flags have none).
    pub fn value(&self, field: Field) -> Option<Value> {
        let text = match field.kind() {
            Kind::Text => {
                let sort = field.sorted_as().and_then(|f| self.text(f)).map(Cow::into_owned);
                return self.text(field).map(|t| Value { text: t.into_owned(), number: None, sort });
            }
            Kind::Number | Kind::Date => {
                let n = self.number(field)?;
                match field {
                    Field::Length | Field::StartTime | Field::StopTime => format_length(n),
                    Field::VolumeAdjust => format!("{n:+.0}"),
                    Field::Bpm if (n - n.round()).abs() < 0.05 => format!("{n:.0}"),
                    Field::Bpm => format!("{n:.1}"),
                    Field::Notes => thousands(n as usize),
                    Field::Size => format!("{n:.1}"),
                    Field::BitRate => format!("{n:.0}"),
                    Field::SampleRate => format!("{} kHz", n / 1000.0),
                    Field::Imported => date(n as i64),
                    _ => format!("{n}"),
                }
            }
            Kind::Tag | Kind::Flag => return None,
        };
        Some(Value { number: self.number(field), text, sort: None })
    }

    /// What a text field sorts by: what the song says it sorts as, else
    /// its text, past a leading article unless `articles` (`sort_text`).
    pub fn sort_text(&self, field: Field, articles: bool) -> Option<String> {
        match field.sorted_as().and_then(|f| self.text(f)) {
            Some(sort) => Some(sort.to_lowercase()),
            None => self.text(field).map(|t| sort_text(&t, articles)),
        }
    }
}

/// Genres tagged by their ID3v1 numbers named: ID3v2 writes them as
/// "17", "(17)", "(17)(31)" or "(17)Rock" (the text, when there is one,
/// is the name), and "(RX)" and "(CR)" for remix and cover.
pub fn genre_names(text: &str) -> Cow<'_, str> {
    let named = |code: &str| -> Option<&'static str> {
        match code {
            "RX" => Some("Remix"),
            "CR" => Some("Cover"),
            _ => symphonia_metadata::id3v1::util::genre_name(code.parse().ok()?).copied(),
        }
    };
    let item = |item: &str| -> Vec<String> {
        if let Some(name) = named(item) {
            return vec![name.to_string()];
        }
        let mut names = Vec::new();
        let mut rest = item;
        while let Some(inner) = rest.strip_prefix('(')
            && let Some((code, after)) = inner.split_once(')')
            && let Some(name) = named(code)
        {
            names.push(name);
            rest = after.trim_start();
        }
        match (names.is_empty(), rest.is_empty()) {
            (true, _) => vec![item.to_string()],
            (false, true) => names.into_iter().map(str::to_string).collect(),
            // "(17)Rock": the text says it.
            (false, false) => vec![rest.to_string()],
        }
    };
    let items: Vec<&str> = Field::Genre.def().split(text).collect();
    let named: Vec<String> = items.iter().flat_map(|i| item(i)).collect();
    if named.iter().map(String::as_str).eq(items.iter().copied()) { Cow::Borrowed(text) } else { Cow::Owned(named.join(", ")) }
}

/// A time as seconds: "83", "1:23", "1:02:03" or "83.5".
pub fn seconds(text: &str) -> Option<f64> {
    let mut total = 0.0;
    for part in text.trim().split(':') {
        let n: f64 = part.trim().parse().ok().filter(|n: &f64| *n >= 0.0)?;
        total = total * 60.0 + n;
    }
    Some(total)
}

/// What `text` sorts by: in lowercase and, unless `articles`, past a
/// leading "The", "A" or "An" ("The Beatles" with the Bs).
pub fn sort_text(text: &str, articles: bool) -> String {
    let lower = text.trim().to_lowercase();
    if articles {
        return lower;
    }
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = lower.strip_prefix(article).map(str::trim_start).filter(|r| !r.is_empty()) {
            return rest.to_string();
        }
    }
    lower
}

/// `secs` (Unix seconds) as a date, `2026-10-20` (UTC).
pub fn date(secs: i64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// What tells a name's group apart: `name` without case, spacing,
/// punctuation or symbols (and width: "ＯＳＴ" is "OST"), nor any of the
/// characters in `ignore` (`Config::group_ignore`). A name that's nothing
/// but those keeps them, in lowercase.
pub fn group_key(name: &str, ignore: &str) -> String {
    let key: String =
        name.nfkc().filter(|c| c.is_alphanumeric() && !ignore.contains(*c)).flat_map(char::to_lowercase).collect();
    if key.is_empty() { name.trim().to_lowercase() } else { key }
}

/// The spelling of a group's name to show, of those its songs have (each
/// once per song): the one with the most words in mixed case ("Justcore
/// Extravaganza" over "JUSTCORE EXTRAVAGANZA" and "justcore
/// extravaganza"), then the most common, then the first in order.
pub fn best_spelling<'s>(spellings: impl IntoIterator<Item = &'s str>) -> Option<&'s str> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for spelling in spellings {
        *counts.entry(spelling).or_default() += 1;
    }
    let mixed = |s: &str| s.split_whitespace().filter(|w| w.chars().any(char::is_uppercase) && w.chars().any(char::is_lowercase)).count();
    // (`max_by_key` keeps the last of equals: go in reverse for the first.)
    counts.into_iter().rev().max_by_key(|(s, n)| (mixed(s), *n)).map(|(s, _)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_in_field_order() {
        for (i, def) in FIELDS.iter().enumerate() {
            assert_eq!(def.field as usize, i, "{}", def.key);
            assert_eq!(Field::by_key(def.key), Some(def.field));
            let kept = serde_json::to_string(&def.field).unwrap();
            assert_eq!(kept, format!("\"{}\"", def.key), "rules keep a field by its key");
        }
    }

    #[test]
    fn facts_give_each_field() {
        let meta = Meta { title: Some(" Song ".into()), artist: Some(String::new()), track: Some(3), length: Some(83.0), size: 4_200_000, ..Meta::default() };
        let tags = vec![STARRED.to_string()];
        let facts = Facts { file_name: "03 Song.MP3", meta: Some(&meta), info: None, imported: Some(86_400), tags: &tags, edits: None, added_by: None };
        assert_eq!(facts.text(Field::Title).as_deref(), Some("Song"));
        assert_eq!(facts.text(Field::Artist), None, "empty is none");
        assert_eq!(facts.text(Field::FileName).as_deref(), Some("03 Song"));
        assert_eq!(facts.text(Field::FileExtension).as_deref(), Some("mp3"));
        assert_eq!(facts.value(Field::Track), Some(Value { text: "3".into(), number: Some(3.0), sort: None }));
        assert_eq!(facts.value(Field::Length).map(|v| v.text).as_deref(), Some("1:23"));
        assert_eq!(facts.value(Field::Size).map(|v| v.text).as_deref(), Some("4.2"));
        assert_eq!(facts.value(Field::BitRate).map(|v| v.text).as_deref(), Some("405"), "its size over its length");
        let midi = Facts { file_name: "Song.mid", ..facts };
        assert_eq!(midi.value(Field::BitRate), None, "not MIDI's");
        assert_eq!(facts.value(Field::Imported).map(|v| v.text).as_deref(), Some("1970-01-02"));
        assert_eq!(facts.value(Field::Bpm), None, "only a read here knows");
        assert!(facts.starred());
        let meta = Meta { genre: Some("Avant-Garde, Electronic;Rock,".into()), artist: Some("A, B".into()), composer: Some("C; D\0E".into()), ..Meta::default() };
        let facts = Facts { meta: Some(&meta), ..Facts::default() };
        assert_eq!(facts.items(Field::Genre), ["Avant-Garde", "Electronic", "Rock"]);
        assert_eq!(facts.items(Field::Artist), ["A, B"], "names split only at semicolons");
        assert_eq!(facts.items(Field::Composer), ["C", "D", "E"]);
        assert_eq!(facts.first(Field::Composer).as_deref(), Some("C"));
        assert_eq!(Field::Composer.def().join(&["C", "D"]), "C; D");
        assert_eq!(Field::Genre.def().join(&["Rock", "Jazz"]), "Rock, Jazz");
        // Edits over the file's tags: text, a number, and one cleared.
        let edits: Edits = [(Field::Artist, "C".to_string()), (Field::Year, "1999".to_string()), (Field::Genre, String::new())].into();
        let edited = Facts { edits: Some(&edits), ..facts };
        assert_eq!(edited.text(Field::Artist).as_deref(), Some("C"));
        assert_eq!(edited.number(Field::Year), Some(1999.0));
        assert_eq!(edited.items(Field::Genre), Vec::<Cow<str>>::new());
        assert_eq!(edited.unedited().text(Field::Artist).as_deref(), Some("A, B"));
        // Flags: from the file, said otherwise, and the composer as artist.
        let meta = Meta { compilation: true, artist: Some("Orchestra".into()), composer: Some("Bach".into()), ..Meta::default() };
        let facts = Facts { meta: Some(&meta), ..Facts::default() };
        assert!(facts.flag(Field::Compilation));
        let edits: Edits = [(Field::Compilation, String::new()), (Field::ComposerAsArtist, "1".to_string())].into();
        let edited = Facts { edits: Some(&edits), ..facts };
        assert!(!edited.flag(Field::Compilation));
        assert_eq!(edited.text(Field::Artist).as_deref(), Some("Bach"));
        for (tagged, named) in [("17", "Rock"), ("(17)", "Rock"), ("(17)(0)", "Rock, Blues"), ("(17)Hard Rock", "Hard Rock"), ("(RX), 31", "Remix, Trance"), ("Jazz", "Jazz"), ("(Live)", "(Live)"), ("255", "255")] {
            assert_eq!(genre_names(tagged), named, "{tagged}");
        }
    }

    #[test]
    fn playback_options_read_as_times_and_percents() {
        assert_eq!(seconds("83"), Some(83.0));
        assert_eq!(seconds("1:23"), Some(83.0));
        assert_eq!(seconds("1:02:03"), Some(3723.0));
        assert_eq!(seconds("0:01.5"), Some(1.5));
        assert_eq!(seconds("soon"), None);
        let edits: Edits = [(Field::StartTime, "0:30".to_string()), (Field::VolumeAdjust, "-150%".to_string())].into();
        let facts = Facts { edits: Some(&edits), ..Facts::default() };
        assert_eq!(facts.value(Field::StartTime).map(|v| v.text).as_deref(), Some("0:30"));
        assert_eq!(facts.number(Field::VolumeAdjust), Some(-100.0));
        assert_eq!(facts.value(Field::VolumeAdjust).map(|v| v.text).as_deref(), Some("-100"));
    }

    #[test]
    fn sorting_goes_past_articles_and_by_sort_as() {
        assert_eq!(sort_text("The Beatles", false), "beatles");
        assert_eq!(sort_text("A Bird Story", false), "bird story");
        assert_eq!(sort_text("An Ocean", false), "ocean");
        assert_eq!(sort_text("The Beatles", true), "the beatles");
        assert_eq!(sort_text("The", false), "the", "nothing after it");
        assert_eq!(sort_text("Theory", false), "theory");
        let meta = Meta { artist: Some("The Beatles".into()), sort_artist: Some("Beatles, The".into()), album: Some("The White Album".into()), ..Meta::default() };
        let facts = Facts { meta: Some(&meta), ..Facts::default() };
        assert_eq!(facts.sort_text(Field::Artist, false).as_deref(), Some("beatles, the"));
        assert_eq!(facts.sort_text(Field::Album, false).as_deref(), Some("white album"));
        assert_eq!(facts.value(Field::Artist).and_then(|v| v.sort).as_deref(), Some("Beatles, The"));
    }

    #[test]
    fn names_group_past_case_spacing_and_punctuation() {
        assert_eq!(group_key("<OST>", ""), group_key("OST", ""));
        assert_eq!(group_key("ＯＳＴ", ""), "ost");
        assert_eq!(group_key("Justcore  Extravaganza", ""), group_key("JUSTCORE EXTRAVAGANZA!", ""));
        assert_ne!(group_key("Modal Soul", ""), group_key("Modal Soul 2", ""));
        assert_eq!(group_key("ラブ・ソング", ""), group_key("ラブソング", ""));
        assert_eq!(group_key("ラブーソング", "ー"), group_key("ラブソング", ""));
        assert_eq!(group_key(" ★★ ", ""), "★★", "nothing but symbols keeps them");
    }

    #[test]
    fn the_best_spelling_has_mixed_case() {
        let pick = |names: &[&'static str]| best_spelling(names.iter().copied());
        assert_eq!(pick(&["JUSTCORE EXTRAVAGANZA", "Justcore Extravaganza", "justcore extravaganza"]), Some("Justcore Extravaganza"));
        assert_eq!(pick(&["OST", "<OST>", "<OST>"]), Some("<OST>"), "then the most common");
        assert_eq!(pick(&["OST", "<OST>"]), Some("<OST>"), "then the first");
        assert_eq!(pick(&[]), None);
    }
}
