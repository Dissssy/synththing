//! A song's lyrics, for the Lyrics panel: timed (LRC) where the source has
//! times, else plain text. Read off the UI thread when a song starts
//! (`App::lyrics`), from, in order: an `.lrc` file beside the song (same
//! name), then the lyrics in its tags (which are often LRC too).

use std::fs::File;
use std::path::{Path, PathBuf};

/// Where a song's lyrics came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// An `.lrc` file beside the song.
    File,
    /// The song's own tags.
    Tags,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::File => "From the .lrc file beside the song",
            Source::Tags => "From the song's tags",
        }
    }
}

/// A song's lyrics.
#[derive(Clone, Debug)]
pub struct Lyrics {
    pub source: Source,
    /// The lines, in order. Timed lines carry when they start (song
    /// seconds, before the song's own offset); plain lyrics have no times.
    pub lines: Vec<Line>,
    pub timed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub at: Option<f64>,
    pub text: String,
}

impl Lyrics {
    /// Lyrics from LRC (or plain) text, if there's any text.
    pub fn parse(text: &str, source: Source) -> Option<Lyrics> {
        // (A malformed time tag fails the whole parse: then it's text.)
        if let Ok(mut lrc) = text.parse::<lrc::Lyrics>()
            && !lrc.get_timed_lines().is_empty()
        {
            // LRC's own offset: positive shows lines sooner.
            if let Some(offset) = lrc.metadata.get_text("offset").and_then(|o| o.trim().parse::<i64>().ok()) {
                lrc.shift(-offset);
            }
            let lines = lrc
                .get_timed_lines()
                .iter()
                .map(|(at, text)| Line { at: Some(at.get_timestamp().max(0) as f64 / 1000.0), text: text.trim().to_string() })
                .collect();
            return Some(Lyrics { source, lines, timed: true });
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let lines = text.lines().map(|l| Line { at: None, text: l.trim_end().to_string() }).collect();
        Some(Lyrics { source, lines, timed: false })
    }

    /// The timed line playing at `position` (song seconds, the offset
    /// already taken away), if one's started.
    pub fn current(&self, position: f64) -> Option<usize> {
        if !self.timed {
            return None;
        }
        self.lines.partition_point(|l| l.at.is_some_and(|at| at <= position)).checked_sub(1)
    }

    /// How far through line `index` `position` is (0 to 1): up to the
    /// next line's start, or the song's `length` after the last.
    pub fn progress(&self, index: usize, position: f64, length: f64) -> f32 {
        let Some(start) = self.lines.get(index).and_then(|l| l.at) else { return 0.0 };
        let end = self.lines.get(index + 1).and_then(|l| l.at).unwrap_or(length.max(start));
        if end <= start { 1.0 } else { ((position - start) / (end - start)).clamp(0.0, 1.0) as f32 }
    }
}

/// The song at `path`'s lyrics, if it has any (on the lyrics thread).
pub fn read(path: PathBuf) -> Option<Lyrics> {
    if let Ok(text) = std::fs::read_to_string(path.with_extension("lrc"))
        && let Some(lyrics) = Lyrics::parse(&text, Source::File)
    {
        return Some(lyrics);
    }
    tagged(&path).and_then(|text| Lyrics::parse(&text, Source::Tags))
}

/// The lyrics in an audio file's tags.
fn tagged(path: &Path) -> Option<String> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey};
    use symphonia::core::probe::Hint;

    let stream = MediaSourceStream::new(Box::new(File::open(path).ok()?), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe().format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default()).ok()?;
    let find = |tags: &[symphonia::core::meta::Tag]| {
        tags.iter().find(|t| t.std_key == Some(StandardTagKey::Lyrics) || t.key.eq_ignore_ascii_case("unsyncedlyrics")).map(|t| t.value.to_string())
    };
    // Tags can sit before the audio (ID3) or inside the container.
    let before = probed.metadata.get().and_then(|m| m.current().and_then(|r| find(r.tags())));
    before.or_else(|| probed.format.metadata().current().and_then(|r| find(r.tags())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_timed_lyrics_and_their_offset() {
        let lyrics = Lyrics::parse("[ar:Someone]\n[offset:500]\n[00:01.50]One\n[00:03.00][00:10.00]Two\n", Source::File).unwrap();
        assert!(lyrics.timed);
        let lines: Vec<_> = lyrics.lines.iter().map(|l| (l.at.unwrap(), l.text.as_str())).collect();
        assert_eq!(lines, [(1.0, "One"), (2.5, "Two"), (9.5, "Two")]);
        assert_eq!(lyrics.current(0.5), None);
        assert_eq!(lyrics.current(2.5), Some(1));
        assert_eq!(lyrics.current(100.0), Some(2));
        assert_eq!(lyrics.progress(0, 1.75, 20.0), 0.5);
        assert_eq!(lyrics.progress(2, 20.0, 20.0), 1.0);
    }

    #[test]
    fn plain_text_is_plain_lyrics() {
        let lyrics = Lyrics::parse("First line\r\n\r\nSecond [not a tag]\n", Source::Tags).unwrap();
        assert!(!lyrics.timed);
        let lines: Vec<_> = lyrics.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines, ["First line", "", "Second [not a tag]"]);
        assert_eq!(lyrics.current(5.0), None);
        assert!(Lyrics::parse("  \n ", Source::Tags).is_none());
    }

    #[test]
    fn an_lrc_file_beside_the_song_wins() {
        let dir = std::env::temp_dir().join(format!("synththing-lyrics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("song.mp3");
        std::fs::write(&song, b"not audio").unwrap();
        assert!(read(song.clone()).is_none());
        std::fs::write(dir.join("song.lrc"), "[00:00.00]Hello").unwrap();
        let lyrics = read(song).unwrap();
        assert_eq!((lyrics.source, lyrics.lines[0].text.as_str()), (Source::File, "Hello"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
