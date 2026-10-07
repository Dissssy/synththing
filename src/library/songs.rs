//! Songs on library servers (docs/SERVER.md): MIDI files, checked with the
//! app's own MIDI reader before they're taken, with the uploader's
//! declaration of their rights, and a fingerprint of their notes for copy
//! checks (so re-saving a file, or changing its instruments, doesn't make
//! it a different song).

use serde::{Deserialize, Serialize};

use super::fingerprint::Fingerprint;
use crate::midi_notes::NoteList;

/// The largest MIDI file taken.
pub const MAX_SONG_BYTES: usize = 4 * 1024 * 1024;
/// How long a song's preview stretch is, in seconds.
pub const PREVIEW_SECONDS: f64 = 12.0;

/// What an uploader declares about their right to share a song: (as the
/// API names it, as shown).
pub const RIGHTS: [(&str, &str); 3] = [
    ("own", "My own composition"),
    ("public_domain", "An arrangement of a public-domain work"),
    ("licensed", "Released under a license"),
];

pub fn rights_title(rights: &str) -> &str {
    RIGHTS.iter().find(|(r, _)| *r == rights).map_or(rights, |(_, title)| title)
}

/// Where a song is from: (as the API names it, as shown). Kept as its
/// category, so listings can be narrowed to one.
pub const SOURCES: [(&str, &str); 8] = [
    ("original", "Original composition"),
    ("game", "Video game"),
    ("film", "Film or TV"),
    ("anime", "Anime"),
    ("popular", "Popular music"),
    ("classical", "Classical"),
    ("traditional", "Folk or traditional"),
    ("other", "Other"),
];

pub fn source_title(source: &str) -> &str {
    SOURCES.iter().find(|(s, _)| *s == source).map_or(source, |(_, title)| title)
}

/// The longest composer, arranger or "from" name.
pub const MAX_CREDIT_CHARS: usize = 120;

/// `POST /api/v1/songs`: a song (a MIDI file, base64), signed or not.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SongUpload {
    /// The song's title.
    pub name: String,
    /// Who wrote it (or the artist).
    pub composer: String,
    /// Who made the MIDI file; none for the poster (`author_name`).
    #[serde(default)]
    pub arranger: String,
    /// One of `SOURCES`.
    pub from: String,
    /// What it's from: the game, film or album.
    #[serde(default)]
    pub from_title: String,
    pub description: String,
    pub tags: Vec<String>,
    /// The name it's posted under.
    pub author_name: String,
    /// One of `RIGHTS`; with `"licensed"`, `license` names it.
    pub rights: String,
    #[serde(default)]
    pub license: String,
    /// A script it's made for (its ID, on the same server), if any.
    #[serde(default)]
    pub made_for: Option<String>,
    /// The MIDI file, base64.
    pub data: String,
    pub app_version: String,
    /// For a signed upload: its name among the uploader's songs and scripts.
    #[serde(default)]
    pub slug: Option<String>,
}

/// A song's own details, in its summary.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SongInfo {
    pub composer: String,
    /// Who made the MIDI file (the poster's name if they didn't say).
    pub arranger: String,
    /// What it's from (its kind is the summary's `category`).
    #[serde(default)]
    pub from_title: String,
    /// The file's own copyright notice, as it says.
    #[serde(default)]
    pub copyright: Option<String>,
    pub rights: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub made_for: Option<String>,
    /// Seconds.
    pub length: f64,
    pub notes: u64,
    pub tracks: u16,
    pub channels: u16,
    /// Where its preview's stretch starts, in seconds.
    pub preview_start: f64,
}

/// What reading a MIDI file found.
#[derive(Clone, Debug, PartialEq)]
pub struct SongFacts {
    pub length: f64,
    pub notes: u64,
    pub tracks: u16,
    pub channels: u16,
    /// The busiest `PREVIEW_SECONDS` start here.
    pub preview_start: f64,
    pub fingerprint: Fingerprint,
    /// Its copyright notice (meta event 02).
    pub copyright: Option<String>,
}

/// Read a MIDI file as the app would play it: its facts, or why it isn't
/// one that can be taken.
pub fn analyze(bytes: &[u8]) -> Result<SongFacts, String> {
    if bytes.len() > MAX_SONG_BYTES {
        return Err(format!("songs can be up to {} MB", MAX_SONG_BYTES / 1024 / 1024));
    }
    rustysynth::MidiFile::new(&mut &bytes[..]).map_err(|e| format!("it isn't a MIDI file the synth can play ({e})"))?;
    let list = NoteList::from_smf(bytes).map_err(|e| format!("its notes can't be read ({e})"))?;
    if list.note_count() == 0 {
        return Err("it has no notes".into());
    }
    let facts = list.facts();
    let notes: Vec<(f64, u8, f64)> = list.between(0.0, f64::MAX).map(|(_, n)| (n.start, n.key, n.stop - n.start)).collect();
    Ok(SongFacts {
        length: facts.end_time,
        notes: list.note_count() as u64,
        tracks: facts.tracks,
        channels: list.channels().len() as u16,
        preview_start: busiest(&notes, facts.end_time),
        fingerprint: fingerprint(&notes),
        copyright: facts.copyright.clone(),
    })
}

/// What a MIDI file suggests for its own details (Publish song... fills
/// them in, to be changed): its title (the first track's name, else the
/// file's), and the composer and arranger its text events or copyright
/// notice name ("Composed by", "Sequenced by" and the like).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Suggested {
    pub title: String,
    pub composer: String,
    pub arranger: String,
}

const COMPOSER_MARKS: [&str; 5] = ["composed by", "music by", "written by", "composer:", "composer -"];
const ARRANGER_MARKS: [&str; 8] = [
    "sequenced by",
    "arranged by",
    "transcribed by",
    "arrangement by",
    "midi by",
    "sequencer:",
    "arranger:",
    "sequenced:",
];

pub fn suggest(bytes: &[u8], file_name: &str) -> Suggested {
    let Ok(list) = NoteList::from_smf(bytes) else { return Suggested { title: file_name.into(), ..Default::default() } };
    let facts = list.facts();
    let title = facts.track_names.first().filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| file_name.to_string());
    let texts: Vec<&str> = facts.copyright.iter().chain(&facts.texts).map(String::as_str).collect();
    let find = |marks: &[&str]| texts.iter().find_map(|text| after_mark(text, marks)).unwrap_or_default();
    Suggested { title: title.chars().take(super::MAX_NAME_CHARS).collect(), composer: find(&COMPOSER_MARKS), arranger: find(&ARRANGER_MARKS) }
}

/// The name after one of `marks` in `text` (up to the end of its line or
/// clause), if one's there.
fn after_mark(text: &str, marks: &[&str]) -> Option<String> {
    let lower = text.to_lowercase();
    marks.iter().find_map(|mark| {
        // (Lowercasing keeps byte offsets for the ASCII marks, unless
        // something before them changed length: then it's skipped.)
        let at = lower.find(mark)? + mark.len();
        let rest = text.get(at..).filter(|_| lower.len() == text.len())?;
        let name = rest.split(['\n', '\r', ',', ';', '(', '[']).next()?.split(" - ").next()?;
        let name = name.trim_matches(|c: char| c.is_whitespace() || c == ':' || c == '.');
        (!name.is_empty() && name.chars().count() <= MAX_CREDIT_CHARS).then(|| name.to_string())
    })
}

/// Where the busiest `PREVIEW_SECONDS` of a song start (the most notes
/// starting in it), on whole seconds.
fn busiest(notes: &[(f64, u8, f64)], length: f64) -> f64 {
    let last = (length - PREVIEW_SECONDS).max(0.0).floor() as usize;
    let mut best = (0usize, 0.0);
    for second in 0..=last {
        let start = second as f64;
        let count = notes.iter().filter(|(t, ..)| *t >= start && *t < start + PREVIEW_SECONDS).count();
        if count > best.0 {
            best = (count, start);
        }
    }
    best.1
}

/// A song's notes as a fingerprint: each note's pitch with the time since
/// the one before and its length, both in steps of 1/16 s, so the same
/// music matches whatever file it's in or whatever plays it.
fn fingerprint(notes: &[(f64, u8, f64)]) -> Fingerprint {
    let step = |seconds: f64| (seconds * 16.0).round() as i64;
    let mut previous = 0.0;
    let tokens = notes
        .iter()
        .map(|&(start, key, length)| {
            let token = format!("{key}:{}:{}", step(start - previous), step(length).min(64));
            previous = start;
            token
        })
        .collect();
    Fingerprint::of_tokens(tokens)
}

pub fn encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text.trim()).map_err(|_| "the song's data isn't base64".to_string())
}

/// An upload's fields, checked (not the file: `analyze` does that).
pub fn check_upload(upload: &SongUpload) -> Result<(), String> {
    let name = upload.name.trim();
    if name.is_empty() || name.chars().count() > super::MAX_NAME_CHARS {
        return Err(format!("the title has to be 1 to {} characters", super::MAX_NAME_CHARS));
    }
    if upload.composer.trim().is_empty() || upload.composer.chars().count() > MAX_CREDIT_CHARS {
        return Err(format!("say who wrote it (up to {MAX_CREDIT_CHARS} characters)"));
    }
    if upload.arranger.chars().count() > MAX_CREDIT_CHARS || upload.from_title.chars().count() > MAX_CREDIT_CHARS {
        return Err(format!("names can be up to {MAX_CREDIT_CHARS} characters"));
    }
    if !SOURCES.iter().any(|(s, _)| *s == upload.from) {
        return Err("say where it's from".into());
    }
    if upload.description.chars().count() > super::MAX_DESCRIPTION_CHARS {
        return Err(format!("the description can be up to {} characters", super::MAX_DESCRIPTION_CHARS));
    }
    if upload.author_name.trim().is_empty() || upload.author_name.chars().count() > super::MAX_NAME_CHARS {
        return Err("say who's posting it".into());
    }
    if !RIGHTS.iter().any(|(r, _)| *r == upload.rights) {
        return Err("say what right you have to share it".into());
    }
    if upload.rights == "licensed" && upload.license.trim().is_empty() {
        return Err("name the license it's released under".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn starter(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/starter/songs").join(name)).unwrap()
    }

    #[test]
    fn songs_are_read_and_fingerprinted() {
        let canon = analyze(&starter("Canon in D.mid")).unwrap();
        assert!(canon.notes > 100 && canon.length > 20.0 && canon.tracks >= 1, "{canon:?}");
        assert!(canon.preview_start >= 0.0 && canon.preview_start + PREVIEW_SECONDS <= canon.length.max(PREVIEW_SECONDS));
        let ode = analyze(&starter("Ode to Joy.mid")).unwrap();
        assert!(canon.fingerprint.similarity(&ode.fingerprint) < 0.3);
        assert!(canon.fingerprint.similarity(&analyze(&starter("Canon in D.mid")).unwrap().fingerprint) >= 1.0);
        assert!(analyze(b"not a midi").is_err());
        assert!(analyze(&vec![0u8; MAX_SONG_BYTES + 1]).unwrap_err().contains("MB"));
        assert_eq!(decode(&encode(b"abc")).unwrap(), b"abc");
        let mut upload = SongUpload {
            name: "Canon".into(),
            composer: "Pachelbel".into(),
            author_name: "me".into(),
            rights: "public_domain".into(),
            from: "classical".into(),
            ..Default::default()
        };
        assert!(check_upload(&upload).is_ok());
        upload.from = "somewhere".into();
        assert!(check_upload(&upload).is_err());
        upload.from = "classical".into();
        upload.rights = "licensed".into();
        assert!(check_upload(&upload).unwrap_err().contains("license"));
        upload.rights = "trust me".into();
        assert!(check_upload(&upload).is_err());
    }

    #[test]
    fn details_are_suggested() {
        let marks = |text: &str| (after_mark(text, &COMPOSER_MARKS), after_mark(text, &ARRANGER_MARKS));
        assert_eq!(marks("Composed by J. Pachelbel\nSequenced by Some One (2001)"), (
            Some("J. Pachelbel".into()),
            Some("Some One".into())
        ));
        assert_eq!(marks("Copyright 1998 - Arranged by: Jane"), (None, Some("Jane".into())));
        assert_eq!(marks("just a song"), (None, None));
        let canon = suggest(&starter("Canon in D.mid"), "Canon in D");
        assert!(!canon.title.is_empty());
        assert_eq!(suggest(b"not a midi", "x").title, "x");
    }
}
