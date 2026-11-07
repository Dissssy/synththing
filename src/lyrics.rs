//! A song's lyrics, for the Lyrics panel: timed (LRC) where the source has
//! times, else plain text. Read off the UI thread when a song starts
//! (`App::lyrics`), from, in order: an `.lrc` file beside the song (same
//! name), then the library's copy of one (`stored`: the `.lrc` beside a
//! song when it was imported), then a MIDI file's own lyric events, then
//! the lyrics in its tags (which are often LRC too). Where the source
//! times each word (enhanced LRC, MIDI's syllables) the lines carry those
//! times too, so the words fill in as they're sung. A song with none of
//! its own has LRCLIB's, once it's been looked up there.

use std::fs::File;
use std::path::{Path, PathBuf};

/// Where a song's lyrics came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// An `.lrc` file beside the song, or beside it when it was
    /// imported.
    File,
    /// A MIDI file's lyric events (or a .kar file's text events).
    Midi,
    /// The song's own tags.
    Tags,
    /// LRCLIB (`lookup::lrclib`), for a song with none of its own.
    Lrclib,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::File => "From the .lrc file beside the song",
            Source::Midi => "From the MIDI file's own lyrics",
            Source::Lrclib => "From LRCLIB (lrclib.net)",
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
    /// When each word (or syllable) starts, with where in `text` it does,
    /// in order: only where the source times them.
    pub words: Vec<(f64, usize)>,
}

impl Line {
    /// A line of `text`, its outer spaces trimmed (and `words` kept with
    /// their places in it); `at` its first word's time if not given.
    fn new(at: Option<f64>, text: &str, mut words: Vec<(f64, usize)>) -> Line {
        let lead = text.len() - text.trim_start().len();
        let text = text.trim().to_string();
        for (_, place) in &mut words {
            *place = place.saturating_sub(lead);
        }
        words.retain(|&(_, place)| place < text.len());
        Line { at: at.or_else(|| words.first().map(|&(at, _)| at)), text, words }
    }

    /// How much of the text is sung at `position` (a byte length): up to
    /// the first word not started yet. `None` without word times.
    pub fn sung(&self, position: f64) -> Option<usize> {
        let first = self.words.first()?;
        let next = self.words.iter().find(|(at, _)| *at > position);
        Some(match next {
            Some(&(_, place)) if position >= first.0 => place,
            Some(_) => 0,
            None => self.text.len(),
        })
    }
}

impl Lyrics {
    /// Lyrics from LRC (or plain) text, if there's any text.
    pub fn parse(text: &str, source: Source) -> Option<Lyrics> {
        // (A malformed time tag fails the whole parse: then it's text.)
        if let Ok(mut lrc) = text.parse::<lrc::Lyrics>()
            && !lrc.get_timed_lines().is_empty()
        {
            // LRC's own offset: positive shows lines sooner.
            lrc.shift(-lrc_offset(&lrc));
            let lines = lrc
                .get_timed_lines()
                .iter()
                .map(|(at, text)| {
                    let (text, words) = word_times(text, -lrc_offset(&lrc));
                    Line::new(Some(at.get_timestamp().max(0) as f64 / 1000.0), &text, words)
                })
                .collect();
            return Some(Lyrics { source, lines, timed: true });
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let lines = text.lines().map(|l| Line { at: None, text: l.trim_end().to_string(), words: Vec::new() }).collect();
        Some(Lyrics { source, lines, timed: false })
    }

    /// Lyrics from a MIDI file's syllables (`NoteList::lyrics`): a line
    /// ends at a line break in one, or before one starting with `/` or `\`
    /// (a .kar file's new line and new verse).
    pub fn from_syllables(syllables: &[(f64, String)]) -> Option<Lyrics> {
        let mut lines = Vec::new();
        let (mut text, mut words) = (String::new(), Vec::new());
        let mut end_line = |text: &mut String, words: &mut Vec<(f64, usize)>| {
            let line = Line::new(None, text, std::mem::take(words));
            if !line.text.is_empty() {
                lines.push(line);
            }
            text.clear();
        };
        for (at, syllable) in syllables {
            let syllable = match syllable.strip_prefix(['/', '\\']) {
                Some(rest) => {
                    end_line(&mut text, &mut words);
                    rest
                }
                None => syllable,
            };
            for (i, part) in syllable.split(['\r', '\n']).enumerate() {
                if i > 0 {
                    end_line(&mut text, &mut words);
                }
                if !part.trim().is_empty() {
                    words.push((*at, text.len() + (part.len() - part.trim_start().len())));
                }
                text.push_str(part);
            }
        }
        end_line(&mut text, &mut words);
        (!lines.is_empty()).then_some(Lyrics { source: Source::Midi, lines, timed: true })
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

/// An LRC file's own `[offset:]` (milliseconds; positive shows lines
/// sooner).
fn lrc_offset(lrc: &lrc::Lyrics) -> i64 {
    lrc.metadata.get_text("offset").and_then(|o| o.trim().parse().ok()).unwrap_or(0)
}

/// A line of enhanced LRC's text without its words' times (`<01:02.50>`),
/// and those times (seconds, moved by `shift` milliseconds) with where in
/// the text each word starts.
fn word_times(line: &str, shift: i64) -> (String, Vec<(f64, usize)>) {
    static WORD_TIME: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"<(\d+):(\d+)(?:[.:](\d+))?>").expect("a valid regex"));
    let (mut text, mut words, mut from) = (String::new(), Vec::new(), 0);
    for tag in WORD_TIME.captures_iter(line) {
        let whole = tag.get(0).expect("the whole match");
        text.push_str(&line[from..whole.start()]);
        from = whole.end();
        let number = |i| tag.get(i).map_or(0.0, |m| m.as_str().parse::<f64>().unwrap_or(0.0));
        // (Hundredths, as LRC writes them; three digits are thousandths.)
        let fraction = tag.get(3).map_or(0.0, |m| number(3) / 10f64.powi(m.as_str().len() as i32));
        let at = number(1) * 60.0 + number(2) + fraction + shift as f64 / 1000.0;
        // (The word starts after any space following the tag.)
        let rest = &line[from..];
        words.push((at.max(0.0), text.len() + (rest.len() - rest.trim_start().len())));
    }
    text.push_str(&line[from..]);
    (text, words)
}

/// The song at `path`'s lyrics, if it has any (on the lyrics thread).
pub fn read(job: Job) -> Option<Lyrics> {
    local(&job.path).or_else(|| {
        let record = crate::lookup::lrclib::record(job.lrclib?, job.online)?;
        Lyrics::parse(record.text()?, Source::Lrclib)
    })
}

/// What [`read`] reads.
pub struct Job {
    pub path: PathBuf,
    /// Its lyrics at LRCLIB, if it was looked up there
    /// (`song_store::Online::lyrics`).
    pub lrclib: Option<i64>,
    /// LRCLIB can be asked (else only what's kept from it is read).
    pub online: bool,
}

/// The song at `path`'s own lyrics, if it has any.
pub fn local(path: &Path) -> Option<Lyrics> {
    let path = path.to_path_buf();
    let library = crate::config::music_dir().ok();
    let stored = library.as_deref().and_then(|dir| crate::song_store::key_in(dir, &path)).and_then(stored);
    let files = [Some(path.with_extension("lrc")), stored];
    if let Some(lyrics) = files.iter().flatten().find_map(|f| Lyrics::parse(&std::fs::read_to_string(f).ok()?, Source::File)) {
        return Some(lyrics);
    }
    if crate::midi_notes::is_midi(&path) {
        let notes = crate::midi_notes::NoteList::from_smf(&std::fs::read(&path).ok()?).ok()?;
        return Lyrics::from_syllables(notes.lyrics());
    }
    tagged(&path).and_then(|text| Lyrics::parse(&text, Source::Tags))
}

/// Where the library keeps the song `key`'s lyrics: in a folder of their
/// own, as the song store takes every `<hash>.<ext>` in its own for a song.
pub fn stored(key: &str) -> Option<PathBuf> {
    Some(crate::config::config_dir().ok()?.join("lyrics").join(format!("{key}.lrc")))
}

/// Keep the `.lrc` beside `file` (if there is one) as the lyrics of the
/// song it was imported as (`key`), unless it has some already.
pub fn import(file: &Path, key: &str) {
    let lrc = file.with_extension("lrc");
    let Some(to) = stored(key).filter(|to| lrc.is_file() && !to.exists()) else { return };
    let copied = to.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(&lrc, &to));
    if let Err(e) = copied {
        log::warn!("couldn't keep the lyrics in {}: {e}", lrc.display());
    }
}

/// Forget the song `key`'s lyrics, as it leaves the library.
pub fn forget(key: &str) {
    if let Some(path) = stored(key) {
        let _ = std::fs::remove_file(path);
    }
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
    fn enhanced_lrc_times_each_word() {
        let lyrics = Lyrics::parse("[00:12.00]<00:12.00> Your <00:12.40>faith <00:12.90>was<00:13.20>\n", Source::File).unwrap();
        let line = &lyrics.lines[0];
        assert_eq!(line.text, "Your faith was");
        assert_eq!(line.words, [(12.0, 0), (12.4, 5), (12.9, 11)]);
        assert_eq!(line.sung(11.0), Some(0));
        assert_eq!(line.sung(12.5), Some(11));
        assert_eq!(&line.text[..line.sung(12.5).unwrap()], "Your faith ");
        assert_eq!(line.sung(14.0), Some(line.text.len()));
    }

    #[test]
    fn midi_syllables_make_lines() {
        let syllables: Vec<(f64, String)> = [(1.0, "Hel"), (1.5, "lo "), (2.0, "world\r"), (3.0, "/A"), (3.5, "gain"), (4.0, "\\Verse two\n")]
            .iter()
            .map(|&(t, s)| (t, s.to_string()))
            .collect();
        let lyrics = Lyrics::from_syllables(&syllables).unwrap();
        let lines: Vec<_> = lyrics.lines.iter().map(|l| (l.at.unwrap(), l.text.as_str())).collect();
        assert_eq!(lines, [(1.0, "Hello world"), (3.0, "Again"), (4.0, "Verse two")]);
        assert_eq!(lyrics.lines[0].words, [(1.0, 0), (1.5, 3), (2.0, 6)]);
        assert_eq!(lyrics.source, Source::Midi);
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
        assert!(local(&song).is_none());
        std::fs::write(dir.join("song.lrc"), "[00:00.00]Hello").unwrap();
        let lyrics = local(&song).unwrap();
        assert_eq!((lyrics.source, lyrics.lines[0].text.as_str()), (Source::File, "Hello"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
