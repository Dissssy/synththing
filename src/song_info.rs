//! Song info for the lists: each file's length, plus details (tracks,
//! notes, tempo, codec, tags, ...) for the info view. Read on a background
//! thread, newest request first (so whatever's on screen comes first), and
//! cached by path.
//!
//! MIDI goes through `midi_notes`; audio files through symphonia's
//! container probe (the same library rodio decodes with), which reads
//! headers and tags without decoding any audio. Files symphonia can't open
//! (WMA, AC3, ...) are described by ffmpeg instead, when there is one.
//!
//! What's been read is kept between launches (`song-info.json` in the
//! config folder), so songs' names (`song_title`) and lengths show at once;
//! each is still read again the first time it's asked for in a session, in
//! case the file changed.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::midi_notes::NoteList;

/// A song's length (when it can be known without decoding it) and its
/// details as label/value pairs, empty when the file has nothing to say.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SongInfo {
    pub length: Option<f64>,
    pub details: Vec<(String, String)>,
    /// A MIDI file's note count.
    pub notes: Option<usize>,
    /// A MIDI file's tempo at its start.
    #[serde(default)]
    pub bpm: Option<f64>,
    /// What its tags say it is, for its name (`song_title`).
    #[serde(default)]
    pub tags: Tags,
}

/// The tags a song's name can be made from: an audio file's (ID3, Vorbis
/// comments, ...), or a MIDI file's first track name as its title.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub track: Option<String>,
    pub year: Option<String>,
}

impl Tags {
    /// The tag `name` (as a title pattern writes it), if the song has it.
    pub fn get(&self, name: &str) -> Option<&str> {
        let tag = match name {
            "title" => &self.title,
            "artist" => &self.artist,
            "album" => &self.album,
            "album_artist" => &self.album_artist,
            "composer" => &self.composer,
            "genre" => &self.genre,
            "track" => &self.track,
            "year" => &self.year,
            _ => return None,
        };
        tag.as_deref()
    }

    /// Fill the tag `key` (symphonia's name for a standard tag, or
    /// ffmpeg's) names, with `value`, unless it's set already.
    fn set(&mut self, key: &str, value: &str) {
        let key = key.to_ascii_lowercase().replace([' ', '_'], "");
        let slot = match key.as_str() {
            "tracktitle" | "title" => &mut self.title,
            "artist" => &mut self.artist,
            "album" => &mut self.album,
            "albumartist" => &mut self.album_artist,
            "composer" => &mut self.composer,
            "genre" => &mut self.genre,
            "tracknumber" | "track" => &mut self.track,
            "date" | "year" | "releasedate" | "originaldate" => &mut self.year,
            _ => return,
        };
        let value = value.trim();
        if slot.is_some() || value.is_empty() {
            return;
        }
        let value = match key.as_str() {
            // "3/12" is track 3.
            "tracknumber" | "track" => value.split('/').next().unwrap_or(value).trim(),
            // "2003-05-01" is 2003.
            "title" | "tracktitle" | "artist" | "album" | "albumartist" | "composer" | "genre" => value,
            _ => value.get(..4).filter(|y| y.bytes().all(|b| b.is_ascii_digit())).unwrap_or(value),
        };
        *slot = Some(value.to_string());
    }
}

/// A MIDI file with more notes than this is "very large": playing it is
/// fine, but a script going through every note can freeze the app.
pub const HEAVY_MIDI_NOTES: usize = 100_000;

impl SongInfo {
    pub fn is_heavy(&self) -> bool {
        self.notes.is_some_and(|n| n > HEAVY_MIDI_NOTES)
    }
}

/// `1234567` as `1,234,567`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

enum Slot {
    /// Being read; meanwhile, what an earlier launch read, if anything.
    Pending(Option<Arc<SongInfo>>),
    /// What an earlier launch read, not read again yet this session.
    Saved(Arc<SongInfo>),
    Ready(Arc<SongInfo>),
    /// The file isn't there (a song not on this device yet): not read
    /// again until it's forgotten (`forget`).
    Missing,
}

type Queue = (Mutex<VecDeque<PathBuf>>, Condvar);

/// A read done: the file's info (`None`: it isn't there), and whether it
/// couldn't be read (its info is then empty).
struct Read {
    path: PathBuf,
    info: Option<SongInfo>,
    unreadable: bool,
}

/// How long after a read the cache is saved (so a folder being read is
/// saved a few times, not once per song).
const SAVE_AFTER: Duration = Duration::from_secs(10);

pub struct SongInfoCache {
    slots: HashMap<PathBuf, Slot>,
    queue: Arc<Queue>,
    results: Receiver<Read>,
    /// A file that couldn't be read, since it was last taken
    /// ([`take_unreadable`](Self::take_unreadable)).
    unreadable: Option<PathBuf>,
    /// Where it's kept between launches, if it is.
    file: Option<PathBuf>,
    /// When the first read not saved yet landed.
    unsaved_since: Option<Instant>,
    /// Counts the reads landed that changed what's known (`revision`).
    revision: u64,
}

impl SongInfoCache {
    pub fn new() -> Self {
        let queue: Arc<Queue> = Arc::default();
        let (tx, results) = mpsc::channel();
        let worker_queue = Arc::clone(&queue);
        let _ = thread::Builder::new().name("song-info".into()).spawn(move || worker(&worker_queue, &tx));
        Self { slots: HashMap::new(), queue, results, unreadable: None, file: None, unsaved_since: None, revision: 0 }
    }

    /// A cache kept in `file`: what it holds is shown until each song is
    /// read again, and what's read is saved there.
    pub fn saved_in(file: PathBuf) -> Self {
        let mut cache = Self::new();
        let saved: Vec<(PathBuf, SongInfo)> = match std::fs::read_to_string(&file) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                log::warn!("couldn't read {} ({e}); songs are read again", file.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        cache.slots = saved.into_iter().map(|(path, info)| (path, Slot::Saved(Arc::new(info)))).collect();
        cache.file = Some(file);
        cache
    }

    /// The info for `path`: what's been read, or else what an earlier
    /// launch read (and then it's read again), or else `None` for now (and
    /// it's asked for, ahead of earlier requests).
    pub fn get(&mut self, path: &Path) -> Option<Arc<SongInfo>> {
        let known = match self.slots.get(path) {
            Some(Slot::Ready(info)) => return Some(Arc::clone(info)),
            Some(Slot::Pending(known)) => return known.clone(),
            Some(Slot::Saved(info)) => Some(Arc::clone(info)),
            Some(Slot::Missing) => return None,
            None => None,
        };
        self.slots.insert(path.to_path_buf(), Slot::Pending(known.clone()));
        let (lock, cvar) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.push_front(path.to_path_buf());
            cvar.notify_one();
        }
        known
    }

    /// Collect finished reads (and save them, now and then). Returns
    /// whether any requests are still outstanding (so the caller can keep
    /// repainting until they land).
    pub fn poll(&mut self) -> bool {
        while let Ok(Read { path, info, unreadable }) = self.results.try_recv() {
            if unreadable {
                self.unreadable.get_or_insert_with(|| path.clone());
            }
            // (A read again that says what an earlier launch did changes nothing.)
            let shown = match self.slots.get(&path) {
                Some(Slot::Pending(known)) => known.as_deref(),
                Some(Slot::Saved(info) | Slot::Ready(info)) => Some(&**info),
                Some(Slot::Missing) | None => None,
            };
            if shown != info.as_ref() {
                self.revision += 1;
            }
            match info {
                Some(info) => {
                    self.slots.insert(path, Slot::Ready(Arc::new(info)));
                    self.unsaved_since.get_or_insert_with(Instant::now);
                }
                None => {
                    self.slots.insert(path, Slot::Missing);
                }
            }
        }
        if self.unsaved_since.is_some_and(|since| since.elapsed() >= SAVE_AFTER) {
            self.unsaved_since = None;
            self.save();
        }
        self.slots.values().any(|s| matches!(s, Slot::Pending(_)))
    }

    /// A file found unreadable since this was last asked, if any.
    pub fn take_unreadable(&mut self) -> Option<PathBuf> {
        self.unreadable.take()
    }

    /// A number that changes whenever a read changes what's known, so
    /// what's made from the info (the Songs list) knows to be made again.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Write what's known to the file, on a thread of its own, leaving out
    /// files that are gone.
    fn save(&self) {
        let Some(file) = self.file.clone() else { return };
        let known: Vec<(PathBuf, Arc<SongInfo>)> = self
            .slots
            .iter()
            .filter_map(|(path, slot)| match slot {
                Slot::Ready(info) | Slot::Saved(info) | Slot::Pending(Some(info)) => Some((path.clone(), Arc::clone(info))),
                Slot::Pending(None) | Slot::Missing => None,
            })
            .collect();
        let _ = thread::Builder::new().name("song-info-save".into()).spawn(move || {
            let kept: Vec<(&Path, &SongInfo)> = known
                .iter()
                .filter(|(path, _)| path.to_str().is_some() && path.exists())
                .map(|(path, info)| (path.as_path(), &**info))
                .collect();
            let result = serde_json::to_string(&kept)
                .map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(&file, json).map_err(|e| e.to_string()));
            if let Err(e) = result {
                log::warn!("couldn't save {}: {e}", file.display());
            }
        });
    }

    /// The file at `path` is there now (a song came into the library):
    /// read it when it's next asked for.
    pub fn forget(&mut self, path: &Path) {
        if matches!(self.slots.get(path), Some(Slot::Missing)) {
            self.slots.remove(path);
        }
    }

    /// The file at `from` is at `to` now (brought into the song library):
    /// what's known of it goes along.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        if let Some(slot) = self.slots.remove(from).filter(|s| !matches!(s, Slot::Pending(_))) {
            self.slots.entry(to.to_path_buf()).or_insert(slot);
        }
    }
}

fn worker(queue: &Queue, results: &Sender<Read>) {
    let (lock, cvar) = queue;
    loop {
        let path = {
            let Ok(mut q) = lock.lock() else { return };
            loop {
                if let Some(path) = q.pop_front() {
                    break path;
                }
                q = match cvar.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        // (Nothing to read, and nothing to say of it, for a file that isn't there.)
        let info = path.exists().then(|| read(&path));
        let unreadable = matches!(&info, Some(Err(_)));
        let info = info.map(|info| {
            info.unwrap_or_else(|e| {
                log::warn!("couldn't read song info for {}: {e}", path.display());
                SongInfo::default()
            })
        });
        if results.send(Read { path, info, unreadable }).is_err() {
            return;
        }
    }
}

/// The song at `path`'s info, or why it can't be read (then it likely
/// can't be played either).
pub fn read(path: &Path) -> Result<SongInfo, String> {
    let is_midi = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
    if is_midi { read_midi(path) } else { read_audio(path) }
}

fn read_midi(path: &Path) -> Result<SongInfo, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let list = NoteList::from_smf(&bytes)?;
    let facts = list.facts();
    let mut details = Vec::new();
    let mut tags = Tags::default();
    if let Some(title) = facts.track_names.first().filter(|n| !n.trim().is_empty()) {
        details.push(("Title".into(), title.clone()));
        tags.title = Some(title.trim().to_string());
    }
    if let Some(copyright) = &facts.copyright {
        details.push(("Copyright".into(), copyright.clone()));
    }
    details.push(("Length".into(), format_length(facts.end_time)));
    let format = match facts.format {
        0 => "0 (single track)",
        1 => "1 (tracks played together)",
        2 => "2 (independent songs)",
        _ => "unknown",
    };
    details.push(("MIDI format".into(), format.into()));
    details.push(("Tracks".into(), facts.tracks.to_string()));
    details.push(("Notes".into(), list.note_count().to_string()));
    let channels = list.channels();
    details.push((
        "Channels".into(),
        format!(
            "{} ({})",
            channels.len(),
            channels.iter().map(|c| (c + 1).to_string()).collect::<Vec<_>>().join(", ")
        ),
    ));
    let bpm = list.timing().map(|timing| timing.tempo_at(0.0));
    if let Some(timing) = list.timing() {
        details.push(("Tempo at start".into(), format!("{:.1} bpm", timing.tempo_at(0.0))));
        let (numerator, denominator) = timing.signature_at(0.0);
        details.push(("Time signature".into(), format!("{numerator}/{denominator}")));
    }
    match facts.ticks_per_quarter {
        Some(ticks) => details.push(("Resolution".into(), format!("{ticks} ticks per quarter note"))),
        None => details.push(("Timing".into(), "SMPTE (frame-based)".into())),
    }
    let named: Vec<&str> = facts.track_names.iter().skip(1).filter(|n| !n.is_empty()).map(String::as_str).collect();
    if !named.is_empty() {
        details.push(("Track names".into(), named.join(", ")));
    }
    Ok(SongInfo { length: Some(facts.end_time), details, notes: Some(list.note_count()), bpm, tags })
}

fn read_audio(path: &Path) -> Result<SongInfo, String> {
    read_audio_with_symphonia(path).or_else(|err| match crate::ffmpeg::find() {
        Some(ffmpeg) => read_audio_with_ffmpeg(&ffmpeg, path).map_err(|ffmpeg_err| format!("{err}; ffmpeg couldn't either: {ffmpeg_err}")),
        None => Err(err),
    })
}

fn read_audio_with_symphonia(path: &Path) -> Result<SongInfo, String> {
    use symphonia::core::codecs::CODEC_TYPE_OPUS;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?;

    // Tags can sit before the audio (ID3) or inside the container.
    let mut details: Vec<(String, String)> = Vec::new();
    let mut song_tags = Tags::default();
    let mut add_tags = |tags: &[symphonia::core::meta::Tag]| {
        for tag in tags {
            let key = match tag.std_key {
                Some(key) => tag_name(&format!("{key:?}")),
                None => tag.key.clone(),
            };
            let value = tag.value.to_string();
            if tag.std_key.is_some() {
                song_tags.set(&key, &value);
            }
            // Matroska's DURATION tag repeats what Length says.
            if key.eq_ignore_ascii_case("duration") {
                continue;
            }
            if !value.trim().is_empty() && !details.iter().any(|(k, _)| *k == key) {
                details.push((key, value.trim().to_string()));
            }
        }
    };
    if let Some(metadata) = probed.metadata.get()
        && let Some(revision) = metadata.current()
    {
        add_tags(revision.tags());
    }
    if let Some(revision) = probed.format.metadata().current() {
        add_tags(revision.tags());
    }

    let mut length = None;
    if let Some(track) = probed.format.default_track() {
        let params = &track.codec_params;
        // Frames count in the track's time base, which is only sometimes
        // one over the sample rate (Matroska counts in milliseconds).
        length = match (params.n_frames, params.time_base, params.sample_rate) {
            (Some(frames), Some(time_base), _) => {
                let time = time_base.calc_time(frames);
                Some(time.seconds as f64 + time.frac)
            }
            (Some(frames), None, Some(rate)) if rate > 0 => Some(frames as f64 / f64::from(rate)),
            _ => None,
        };
        if let Some(length) = length {
            details.push(("Length".into(), format_length(length)));
        }
        if let Some(codec) = symphonia::default::get_codecs().get_codec(params.codec) {
            details.push(("Codec".into(), codec.long_name.to_string()));
        } else if params.codec == CODEC_TYPE_OPUS {
            // Known to the container, but there's no decoder to name it (ffmpeg plays it).
            details.push(("Codec".into(), "Opus".into()));
        }
        if let Some(rate) = params.sample_rate {
            details.push(("Sample rate".into(), format!("{rate} Hz")));
        }
        if let Some(channels) = params.channels {
            details.push(("Channels".into(), channels.count().to_string()));
        }
        if let Some(bits) = params.bits_per_sample {
            details.push(("Bits per sample".into(), bits.to_string()));
        }
    }
    Ok(SongInfo { length, details, notes: None, bpm: None, tags: song_tags })
}

/// What `ffmpeg -i <file>` (with no output) prints about it on stderr: the
/// container's tags, its duration, and the first audio stream.
fn read_audio_with_ffmpeg(ffmpeg: &Path, path: &Path) -> Result<SongInfo, String> {
    let output = crate::ffmpeg::command(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-i"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run ffmpeg ({e})"))?;
    // It always "fails" for want of an output file; what matters is whether it read the input.
    let log = String::from_utf8_lossy(&output.stderr);
    parse_ffmpeg_info(&log).ok_or_else(|| {
        let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it read nothing");
        last.trim().to_string()
    })
}

/// The details from ffmpeg's description of an input, in the same terms
/// and order symphonia's give: tags first, then the stream. None when it
/// found no audio stream.
fn parse_ffmpeg_info(log: &str) -> Option<SongInfo> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut lines = log.lines().skip_while(|l| !l.starts_with("Input #0")).skip(1).peekable();
    let mut details: Vec<(String, String)> = Vec::new();
    let mut tags = Tags::default();
    let mut length = None;
    let mut stream = None;
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if indent(line) == 0 {
            break;
        }
        if indent(line) == 2 && trimmed == "Metadata:" {
            // The container's tags; streams' own come later, indented deeper.
            while let Some(tag) = lines.next_if(|l| indent(l) > 2) {
                if let Some((key, value)) = tag.split_once(':') {
                    tags.set(key.trim(), value);
                    let (key, value) = (ffmpeg_tag_name(key.trim()), value.trim());
                    if !value.is_empty() && !key.eq_ignore_ascii_case("duration") && !details.iter().any(|(k, _)| *k == key) {
                        details.push((key, value.to_string()));
                    }
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("Duration: ") {
            length = parse_ffmpeg_duration(rest.split(',').next().unwrap_or_default());
        } else if stream.is_none()
            && trimmed.starts_with("Stream #")
            && let Some((_, audio)) = trimmed.split_once("Audio: ")
        {
            stream = Some(audio.to_string());
        }
    }
    let stream = stream?;

    if let Some(length) = length {
        details.push(("Length".into(), format_length(length)));
    }
    // "wmav2 (a[1][0][0] / 0x0161), 44100 Hz, stereo, fltp, 128 kb/s"
    let mut parts = stream.split(", ").map(str::trim);
    if let Some(codec) = parts.next().and_then(|c| c.split_whitespace().next()) {
        details.push(("Codec".into(), ffmpeg_codec_name(codec)));
    }
    let rest: Vec<&str> = parts.collect();
    let rate_at = rest.iter().position(|p| p.ends_with(" Hz"));
    if let Some(rate) = rate_at.map(|i| rest[i]) {
        details.push(("Sample rate".into(), rate.to_string()));
    }
    if let Some(layout) = rate_at.and_then(|i| rest.get(i + 1)) {
        let channels = match *layout {
            "mono" => "1",
            "stereo" => "2",
            other => other.strip_suffix(" channels").unwrap_or(other),
        };
        details.push(("Channels".into(), channels.to_string()));
    }
    // An integer sample format: "s16", "s16p", "s32 (24 bit)". Floats say nothing useful here.
    if let Some(format) = rest.iter().find(|p| p.starts_with('s') && p[1..].starts_with(|c: char| c.is_ascii_digit())) {
        let bits = match format.split_once(" (") {
            Some((_, real)) => real.trim_end_matches(" bit)"),
            None => format[1..].trim_end_matches('p'),
        };
        details.push(("Bits per sample".into(), bits.to_string()));
    }
    Some(SongInfo { length, details, notes: None, bpm: None, tags })
}

/// "00:03:21.45" -> 201.45 seconds (None for "N/A").
fn parse_ffmpeg_duration(text: &str) -> Option<f64> {
    let mut fields = text.trim().split(':');
    let (h, m, s) = (fields.next()?, fields.next()?, fields.next()?);
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?)
}

/// ffmpeg's tag keys in the names symphonia's tags get.
fn ffmpeg_tag_name(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "title" => "Track title".into(),
        "track" => "Track number".into(),
        "disc" => "Disc number".into(),
        "album_artist" => "Album artist".into(),
        "date" | "year" => "Date".into(),
        other => {
            let spaced = other.replace('_', " ");
            let mut chars = spaced.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

/// ffmpeg's short codec names, spelled out for the ones it's likely to be
/// describing (symphonia describes the rest itself).
fn ffmpeg_codec_name(codec: &str) -> String {
    match codec {
        "wmav1" | "wmav2" => "Windows Media Audio",
        "wmapro" => "Windows Media Audio Professional",
        "wmalossless" => "Windows Media Audio Lossless",
        "wmavoice" => "Windows Media Audio Voice",
        "ac3" => "Dolby Digital (AC-3)",
        "eac3" => "Dolby Digital Plus (E-AC-3)",
        "truehd" => "Dolby TrueHD",
        "dts" => "DTS",
        "opus" => "Opus",
        "ape" => "Monkey's Audio",
        "wavpack" => "WavPack",
        "tta" => "True Audio",
        "amr_nb" | "amrnb" => "AMR Narrowband",
        "amr_wb" | "amrwb" => "AMR Wideband",
        other => other,
    }
    .to_string()
}

/// "TrackTitle" -> "Track title".
fn tag_name(key: &str) -> String {
    let mut out = String::new();
    for (i, c) in key.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Seconds as m:ss (or h:mm:ss).
pub fn format_length(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_lengths_and_tag_names() {
        assert_eq!(format_length(0.0), "0:00");
        assert_eq!(format_length(61.4), "1:01");
        assert_eq!(format_length(3725.0), "1:02:05");
        assert_eq!(tag_name("TrackTitle"), "Track title");
        assert_eq!(tag_name("Artist"), "Artist");
    }

    #[test]
    fn reads_ffmpegs_description_of_a_file() {
        let log = "[aist#0:0/wmav2 @ 0000] Guessed Channel Layout: stereo
Input #0, asf, from 'song.wma':
  Metadata:
    title           : WMA Song
    album_artist    : Someone: Else
    track           : 9
  Duration: 00:01:30.50, start: 0.000000, bitrate: 158 kb/s
  Stream #0:0: Audio: wmav2 (a[1][0][0] / 0x0161), 44100 Hz, stereo, fltp, 128 kb/s
    Metadata:
      title           : not the song's
At least one output file must be specified
";
        let info = parse_ffmpeg_info(log).unwrap();
        assert_eq!(info.length, Some(90.5));
        let details: Vec<(&str, &str)> = info.details.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(
            details,
            [
                ("Track title", "WMA Song"),
                ("Album artist", "Someone: Else"),
                ("Track number", "9"),
                ("Length", "1:31"),
                ("Codec", "Windows Media Audio"),
                ("Sample rate", "44100 Hz"),
                ("Channels", "2"),
            ]
        );

        let flac = "Input #0, flac, from 'a.flac':\n  Duration: N/A\n  Stream #0:0: Audio: flac, 96000 Hz, 5.1, s32 (24 bit)\n";
        let info = parse_ffmpeg_info(flac).unwrap();
        assert_eq!(info.length, None);
        assert!(info.details.contains(&("Channels".into(), "5.1".into())));
        assert!(info.details.contains(&("Bits per sample".into(), "24".into())));

        assert!(parse_ffmpeg_info("not-there.wma: No such file or directory\n").is_none());
    }

    /// Prints what's read from every song in a real folder, for checking
    /// formats by eye: `SYNTHTHING_INFO_DIR=<folder> cargo test --bin
    /// synththing print_song_info -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads a real folder named by SYNTHTHING_INFO_DIR"]
    fn print_song_info_for_a_folder() {
        let Ok(dir) = std::env::var("SYNTHTHING_INFO_DIR") else { return };
        let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        paths.sort();
        for path in paths.iter().take(40) {
            let info = read(path).unwrap_or_else(|e| {
                println!("{} -> {e}", path.display());
                SongInfo::default()
            });
            println!("{} -> {:?}", path.display(), info.length.map(format_length));
            for (key, value) in &info.details {
                println!("    {key}: {value}");
            }
        }
    }

    #[test]
    fn reads_a_wav_and_a_midi_in_the_background() {
        let dir = std::env::temp_dir().join(format!("synththing-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Half a second of 8 kHz mono 16-bit silence.
        let (rate, frames) = (8000u32, 4000u32);
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + frames * 2).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 2).to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((frames * 2).to_le_bytes());
        wav.extend(vec![0u8; frames as usize * 2]);
        let wav_path = dir.join("tone.wav");
        std::fs::write(&wav_path, wav).unwrap();

        let mut smf = b"MThd\0\0\0\x06\0\0\0\x01\x01\xE0MTrk".to_vec();
        let track = [0x00, 0x90, 60, 100, 0x83, 0x60, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00];
        smf.extend((track.len() as u32).to_be_bytes());
        smf.extend(track);
        let mid_path = dir.join("song.mid");
        std::fs::write(&mid_path, smf).unwrap();

        let mut cache = SongInfoCache::new();
        assert!(cache.get(&wav_path).is_none());
        assert!(cache.get(&mid_path).is_none());
        let mut tries = 0;
        while cache.poll() && tries < 400 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            tries += 1;
        }
        let wav_info = cache.get(&wav_path).unwrap();
        assert!((wav_info.length.unwrap() - 0.5).abs() < 1e-6, "{wav_info:?}");
        assert!(wav_info.details.iter().any(|(k, v)| k == "Sample rate" && v == "8000 Hz"), "{wav_info:?}");
        let mid_info = cache.get(&mid_path).unwrap();
        assert!((mid_info.length.unwrap() - 0.5).abs() < 1e-9);
        assert!(mid_info.details.iter().any(|(k, v)| k == "Notes" && v == "1"), "{mid_info:?}");
        assert_eq!(mid_info.notes, Some(1));
        assert!(!mid_info.is_heavy());
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(999), "999");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
