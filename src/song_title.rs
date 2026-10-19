//! A song's name, from its tags: the first of the title patterns
//! (Preferences > Songs, `Config::title_patterns`) that every tag it uses
//! is there for, e.g. "{title} by {artist}" then "{title}". With none of
//! them, the file's name.

use std::path::Path;

use crate::config::nice_name;
use crate::song_info::Tags;

/// The patterns until changed.
pub const DEFAULT_PATTERNS: [&str; 2] = ["{title} by {artist}", "{title}"];

/// The tags a pattern can use, as it writes them (for the preferences'
/// list of them).
pub const FIELDS: [&str; 8] = ["title", "artist", "album", "album_artist", "composer", "genre", "track", "year"];

/// The song at `path`'s name: from `tags` by the first pattern that works
/// for them, or else the file's name (without its extension).
pub fn song_title(patterns: &[impl AsRef<str>], tags: Option<&Tags>, path: &Path) -> String {
    tags.and_then(|tags| patterns.iter().find_map(|pattern| render(pattern.as_ref(), tags)))
        .unwrap_or_else(|| nice_name(path))
}

/// `pattern` with each `{tag}` filled in, or `None` when the song doesn't
/// have one of them (or it names no tag). A `{` with no `}` after it is
/// just a `{`.
pub fn render(pattern: &str, tags: &Tags) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else { break };
        out.push_str(&rest[..open]);
        out.push_str(tags.get(rest[open + 1..close].trim())?);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Tags {
        Tags {
            title: Some("Middle-Horn Shuffle".into()),
            artist: Some("Nujabes".into()),
            year: Some("2010".into()),
            ..Tags::default()
        }
    }

    #[test]
    fn fills_in_the_tags() {
        assert_eq!(render("{title} by {artist} ({year})", &tags()).as_deref(), Some("Middle-Horn Shuffle by Nujabes (2010)"));
        assert_eq!(render("{ title }", &tags()).as_deref(), Some("Middle-Horn Shuffle"));
    }

    #[test]
    fn a_missing_or_unknown_tag_skips_the_pattern() {
        assert_eq!(render("{title} on {album}", &tags()), None);
        assert_eq!(render("{tittle}", &tags()), None);
        let patterns = ["{title} on {album}", "{title} by {artist}"];
        assert_eq!(song_title(&patterns, Some(&tags()), Path::new("x.mp3")), "Middle-Horn Shuffle by Nujabes");
    }

    #[test]
    fn falls_back_to_the_file_name() {
        assert_eq!(song_title(&DEFAULT_PATTERNS, Some(&Tags::default()), Path::new("/music/03 shuffle.mp3")), "03 shuffle");
        assert_eq!(song_title(&DEFAULT_PATTERNS, None, Path::new("/music/song.mid")), "song");
    }

    #[test]
    fn an_unclosed_brace_is_text() {
        assert_eq!(render("{title} {", &tags()).as_deref(), Some("Middle-Horn Shuffle {"));
    }
}
