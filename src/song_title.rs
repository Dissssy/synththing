//! A song's name: the first of the title patterns (Preferences > Appearance,
//! `Config::title_patterns`) that every field it uses is there for, e.g.
//! "{title} by {artist}" then "{title}". With none of them, the file's
//! name (as it was before it went into the song library).

use std::path::Path;

use crate::config::Config;
use crate::song_info::{format_length, thousands, SongInfo};
use crate::song_store::SongStore;

/// The patterns until changed.
pub const DEFAULT_PATTERNS: [&str; 2] = ["{title} by {artist}", "{title}"];

/// The fields a pattern can use, as it writes them (for the preferences'
/// list of them): the song's tags, then what's known of its file.
pub const FIELDS: [&str; 13] = [
    "title",
    "artist",
    "album",
    "album_artist",
    "composer",
    "genre",
    "track",
    "year",
    "file_name",
    "file_extension",
    "length",
    "bpm",
    "notes",
];

/// How songs are named: the patterns, and the library, which knows its
/// songs' original file names.
pub struct Naming<'a> {
    patterns: Option<&'a [String]>,
    store: &'a SongStore,
}

impl<'a> Naming<'a> {
    pub fn new(config: &'a Config, store: &'a SongStore) -> Self {
        Self { patterns: config.title_patterns.as_deref(), store }
    }

    /// The original name of the file at `path`: the library's name for
    /// it, or (a file played from outside it) its own.
    pub fn file_name<'p>(&'p self, path: &'p Path) -> &'p str {
        match self.store.get(path) {
            Some(song) => &song.name,
            None => path.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        }
    }

    /// The song at `path`, with what's been read of it (`info`), for its
    /// fields.
    pub fn song<'p>(&'p self, path: &'p Path, info: Option<&'p SongInfo>) -> Song<'p> {
        Song { file_name: self.file_name(path), info }
    }

    /// When the song at `path` came into the library (Unix seconds).
    pub fn imported(&self, path: &Path) -> Option<i64> {
        self.store.get(path).map(|song| song.imported)
    }

    /// The song at `path`'s name, with what's been read of it (`info`).
    pub fn name(&self, path: &Path, info: Option<&SongInfo>) -> String {
        let song = self.song(path, info);
        match self.patterns {
            Some(patterns) => first_match(patterns, &song),
            None => first_match(&DEFAULT_PATTERNS, &song),
        }
        .unwrap_or_else(|| stem(song.file_name).to_string())
    }
}

fn stem(file_name: &str) -> &str {
    Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(file_name)
}

fn first_match(patterns: &[impl AsRef<str>], song: &Song) -> Option<String> {
    patterns.iter().find_map(|pattern| render(pattern.as_ref(), song))
}

/// What a pattern's fields are filled in from.
pub struct Song<'a> {
    /// The song's original file name.
    pub file_name: &'a str,
    pub info: Option<&'a SongInfo>,
}

impl Song<'_> {
    /// The field `name` (as a pattern writes it), if the song has it.
    pub fn field(&self, name: &str) -> Option<String> {
        let info = self.info;
        let value = match name {
            "file_name" => stem(self.file_name).to_string(),
            "file_extension" => Path::new(self.file_name).extension()?.to_string_lossy().into_owned(),
            "length" => format_length(info?.length?),
            "bpm" => {
                let bpm = info?.bpm?;
                if (bpm - bpm.round()).abs() < 0.05 { format!("{bpm:.0}") } else { format!("{bpm:.1}") }
            }
            "notes" => thousands(info?.notes?),
            tag => info?.tags.get(tag)?.to_string(),
        };
        (!value.is_empty()).then_some(value)
    }
}

impl Song<'_> {
    /// The field `name` as a number, for the ones that are (to sort by).
    pub fn number(&self, name: &str) -> Option<f64> {
        let info = self.info?;
        match name {
            "length" => info.length,
            "bpm" => info.bpm,
            "notes" => info.notes.map(|n| n as f64),
            "track" | "year" => {
                let tag = info.tags.get(name)?;
                let digits: String = tag.trim().chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            }
            _ => None,
        }
    }
}

/// `pattern` with each `{field}` filled in, or `None` when the song doesn't
/// have one of them (or it names no field). A `{` with no `}` after it is
/// just a `{`.
pub fn render(pattern: &str, song: &Song) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else { break };
        out.push_str(&rest[..open]);
        out.push_str(&song.field(rest[open + 1..close].trim())?);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_info::Tags;

    fn info() -> SongInfo {
        SongInfo {
            length: Some(83.0),
            bpm: Some(120.0),
            notes: Some(12345),
            tags: Tags {
                title: Some("Harbour Lights".into()),
                artist: Some("The Night Owls".into()),
                year: Some("2010".into()),
                ..Tags::default()
            },
            ..SongInfo::default()
        }
    }

    fn song<'a>(file_name: &'a str, info: &'a SongInfo) -> Song<'a> {
        Song { file_name, info: Some(info) }
    }

    #[test]
    fn fills_in_the_fields() {
        let info = info();
        let song = song("harbour.mid", &info);
        assert_eq!(render("{title} by {artist} ({year})", &song).as_deref(), Some("Harbour Lights by The Night Owls (2010)"));
        assert_eq!(render("{ title }", &song).as_deref(), Some("Harbour Lights"));
        assert_eq!(
            render("{file_name}.{file_extension} {length} {bpm} {notes}", &song).as_deref(),
            Some("harbour.mid 1:23 120 12,345")
        );
    }

    #[test]
    fn a_missing_or_unknown_field_skips_the_pattern() {
        let info = info();
        let song = song("x.mp3", &info);
        assert_eq!(render("{title} on {album}", &song), None);
        assert_eq!(render("{tittle}", &song), None);
        assert_eq!(first_match(&["{title} on {album}", "{title} by {artist}"], &song).as_deref(), Some("Harbour Lights by The Night Owls"));
    }

    #[test]
    fn falls_back_to_the_file_name() {
        let config = Config::default();
        let dir = std::env::temp_dir().join(format!("synththing-naming-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SongStore::open(dir.clone());
        let stored = crate::song_store::write_in(b"x", "03 shuffle.mp3", &dir).unwrap();
        let path = dir.join(&stored.key);
        store.add(stored);
        let naming = Naming::new(&config, &store);
        assert_eq!(naming.name(&path, Some(&SongInfo::default())), "03 shuffle");
        assert_eq!(naming.name(Path::new("/elsewhere/song.mid"), None), "song");
    }

    #[test]
    fn an_unclosed_brace_is_text() {
        let info = info();
        assert_eq!(render("{title} {", &song("x.mid", &info)).as_deref(), Some("Harbour Lights {"));
    }
}
