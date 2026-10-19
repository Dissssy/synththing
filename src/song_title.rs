//! A song's name: the first of the title patterns (Preferences > Songs,
//! `Config::title_patterns`) that every field it uses is there for, e.g.
//! "{title} by {artist}" then "{title}". With none of them, the file's
//! name.

use std::path::Path;
use std::path::PathBuf;

use crate::config::{nice_name, Config};
use crate::song_info::{format_length, thousands, SongInfo};

/// The patterns until changed.
pub const DEFAULT_PATTERNS: [&str; 2] = ["{title} by {artist}", "{title}"];

/// The fields a pattern can use, as it writes them (for the preferences'
/// list of them): the song's tags, then what's known of its file.
pub const FIELDS: [&str; 14] = [
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
    "file_path",
    "length",
    "bpm",
    "notes",
];

/// How songs are named: the patterns, and the folder `{file_path}` is
/// relative to (the Songs list's default folder).
pub struct Naming<'a> {
    patterns: Option<&'a [String]>,
    folder: PathBuf,
}

impl<'a> Naming<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { patterns: config.title_patterns.as_deref(), folder: config.browse_start_dir() }
    }

    /// The song at `path`'s name, with what's been read of it (`info`).
    pub fn name(&self, path: &Path, info: Option<&SongInfo>) -> String {
        let song = Song { path, info, folder: &self.folder };
        match self.patterns {
            Some(patterns) => first_match(patterns, &song),
            None => first_match(&DEFAULT_PATTERNS, &song),
        }
        .unwrap_or_else(|| nice_name(path))
    }
}

fn first_match(patterns: &[impl AsRef<str>], song: &Song) -> Option<String> {
    patterns.iter().find_map(|pattern| render(pattern.as_ref(), song))
}

/// What a pattern's fields are filled in from.
pub struct Song<'a> {
    pub path: &'a Path,
    pub info: Option<&'a SongInfo>,
    /// The folder `{file_path}` is relative to.
    pub folder: &'a Path,
}

impl Song<'_> {
    /// The field `name` (as a pattern writes it), if the song has it.
    fn field(&self, name: &str) -> Option<String> {
        let info = self.info;
        let value = match name {
            "file_name" => self.path.file_stem()?.to_string_lossy().into_owned(),
            "file_extension" => self.path.extension()?.to_string_lossy().into_owned(),
            "file_path" => {
                let dir = self.path.parent()?;
                match dir.strip_prefix(self.folder) {
                    Ok(relative) => {
                        relative.iter().map(|part| part.to_string_lossy()).collect::<Vec<_>>().join("/")
                    }
                    Err(_) => dir.display().to_string(),
                }
            }
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

    fn song<'a>(path: &'a str, info: &'a SongInfo) -> Song<'a> {
        Song { path: Path::new(path), info: Some(info), folder: Path::new("/music") }
    }

    #[test]
    fn fills_in_the_fields() {
        let info = info();
        let song = song("/music/games/ds3/harbour.mid", &info);
        assert_eq!(render("{title} by {artist} ({year})", &song).as_deref(), Some("Harbour Lights by The Night Owls (2010)"));
        assert_eq!(render("{ title }", &song).as_deref(), Some("Harbour Lights"));
        assert_eq!(
            render("{file_path}/{file_name}.{file_extension} {length} {bpm} {notes}", &song).as_deref(),
            Some("games/ds3/harbour.mid 1:23 120 12,345")
        );
    }

    #[test]
    fn file_path_outside_the_folder_is_the_full_path() {
        let info = info();
        assert_eq!(render("{file_path}", &song("/other/place/a.mid", &info)).as_deref(), Some("/other/place"));
        assert_eq!(render("{file_path}", &song("/music/a.mid", &info)), None);
    }

    #[test]
    fn a_missing_or_unknown_field_skips_the_pattern() {
        let info = info();
        let song = song("/music/x.mp3", &info);
        assert_eq!(render("{title} on {album}", &song), None);
        assert_eq!(render("{tittle}", &song), None);
        assert_eq!(first_match(&["{title} on {album}", "{title} by {artist}"], &song).as_deref(), Some("Harbour Lights by The Night Owls"));
    }

    #[test]
    fn falls_back_to_the_file_name() {
        let config = Config::default();
        let naming = Naming::new(&config);
        assert_eq!(naming.name(Path::new("/music/03 shuffle.mp3"), Some(&SongInfo::default())), "03 shuffle");
        assert_eq!(naming.name(Path::new("/music/song.mid"), None), "song");
    }

    #[test]
    fn an_unclosed_brace_is_text() {
        let info = info();
        assert_eq!(render("{title} {", &song("/music/x.mid", &info)).as_deref(), Some("Harbour Lights {"));
    }
}
