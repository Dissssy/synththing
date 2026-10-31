//! A song's name: the first of the title patterns (Preferences > Browsing,
//! `Config::title_patterns`) that every field it uses is there for, e.g.
//! "{title} by {artist}" then "{title}". With none of them, the file's
//! name (as it was before it went into the song library).

use std::path::Path;

use crate::config::Config;
use crate::fields::{Facts, Field};
use crate::song_info::SongInfo;
use crate::song_store::SongStore;

/// The patterns until changed.
pub const DEFAULT_PATTERNS: [&str; 2] = ["{title} by {artist}", "{title}"];

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

    /// What's known of the song at `path`, with what's been read of it
    /// here (`info`): the library's, else (a file played from outside
    /// it) its own.
    pub fn song<'p>(&'p self, path: &'p Path, info: Option<&'p SongInfo>) -> Facts<'p> {
        match self.store.get(path) {
            Some(song) => Facts {
                meta: song.meta.as_ref().or(info.map(|i| &i.meta)),
                info,
                stats: self.store.key(path).map(|key| self.store.totals(&key)),
                ..Facts::of(song)
            },
            None => Facts {
                file_name: path.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
                meta: info.map(|i| &i.meta),
                info,
                ..Facts::default()
            },
        }
    }

    /// The song at `path`'s name, with what's been read of it (`info`).
    pub fn name(&self, path: &Path, info: Option<&SongInfo>) -> String {
        let song = self.song(path, info);
        match self.patterns {
            Some(patterns) => first_match(patterns, &song),
            None => first_match(&DEFAULT_PATTERNS, &song),
        }
        .or_else(|| song.text(Field::FileName).map(|t| t.into_owned()))
        .unwrap_or_default()
    }
}

fn first_match(patterns: &[impl AsRef<str>], song: &Facts) -> Option<String> {
    patterns.iter().find_map(|pattern| render(pattern.as_ref(), song))
}

/// `pattern` with each `{field}` (a field's key, `fields.rs`) filled in,
/// or `None` when the song doesn't have one of them (or it names no
/// field). A `{` with no `}` after it is just a `{`.
pub fn render(pattern: &str, song: &Facts) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else { break };
        out.push_str(&rest[..open]);
        let field = Field::by_key(rest[open + 1..close].trim()).filter(|f| f.in_patterns())?;
        out.push_str(&song.value(field)?.text);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song_store::Meta;

    fn info() -> SongInfo {
        SongInfo {
            bpm: Some(120.0),
            notes: Some(12345),
            meta: Meta {
                title: Some("Harbour Lights".into()),
                artist: Some("The Night Owls".into()),
                year: Some(2010),
                length: Some(83.0),
                ..Meta::default()
            },
            ..SongInfo::default()
        }
    }

    fn song<'a>(file_name: &'a str, info: &'a SongInfo) -> Facts<'a> {
        Facts { file_name, meta: Some(&info.meta), info: Some(info), ..Facts::default() }
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
        let path = crate::song_store::path_of(&dir, &stored.key);
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
