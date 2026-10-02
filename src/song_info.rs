//! Song info for the lists: each file's length, plus details (tracks,
//! notes, tempo, codec, tags, ...) for the info view. Read on a background
//! thread, newest request first (so whatever's on screen comes first), and
//! cached by path.
//!
//! MIDI goes through `midi_notes`; audio files through symphonia's
//! container probe (the same library rodio decodes with), which reads
//! headers and tags without decoding any audio.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::midi_notes::NoteList;

/// A song's length (when it can be known without decoding it) and its
/// details as label/value pairs, empty when the file has nothing to say.
#[derive(Clone, Debug, Default)]
pub struct SongInfo {
    pub length: Option<f64>,
    pub details: Vec<(String, String)>,
}

enum Slot {
    Pending,
    Ready(Arc<SongInfo>),
}

type Queue = (Mutex<VecDeque<PathBuf>>, Condvar);

pub struct SongInfoCache {
    slots: HashMap<PathBuf, Slot>,
    queue: Arc<Queue>,
    results: Receiver<(PathBuf, SongInfo)>,
}

impl SongInfoCache {
    pub fn new() -> Self {
        let queue: Arc<Queue> = Arc::default();
        let (tx, results) = mpsc::channel();
        let worker_queue = Arc::clone(&queue);
        let _ = thread::Builder::new().name("song-info".into()).spawn(move || worker(&worker_queue, &tx));
        Self { slots: HashMap::new(), queue, results }
    }

    /// The info for `path` if it's been read; otherwise asks for it (ahead
    /// of earlier requests) and returns `None` for now.
    pub fn get(&mut self, path: &Path) -> Option<Arc<SongInfo>> {
        match self.slots.get(path) {
            Some(Slot::Ready(info)) => return Some(Arc::clone(info)),
            Some(Slot::Pending) => return None,
            None => {}
        }
        self.slots.insert(path.to_path_buf(), Slot::Pending);
        let (lock, cvar) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.push_front(path.to_path_buf());
            cvar.notify_one();
        }
        None
    }

    /// Collect finished reads. Returns whether any requests are still
    /// outstanding (so the caller can keep repainting until they land).
    pub fn poll(&mut self) -> bool {
        while let Ok((path, info)) = self.results.try_recv() {
            self.slots.insert(path, Slot::Ready(Arc::new(info)));
        }
        self.slots.values().any(|s| matches!(s, Slot::Pending))
    }

    /// Forget everything cached for files directly in `dir` (they changed
    /// on disk), so they're read again when next shown.
    pub fn forget_folder(&mut self, dir: &Path) {
        self.slots.retain(|path, slot| matches!(slot, Slot::Pending) || path.parent() != Some(dir));
    }
}

fn worker(queue: &Queue, results: &Sender<(PathBuf, SongInfo)>) {
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
        let info = read(&path);
        if results.send((path, info)).is_err() {
            return;
        }
    }
}

fn read(path: &Path) -> SongInfo {
    let is_midi = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
    let result = if is_midi { read_midi(path) } else { read_audio(path) };
    result.unwrap_or_else(|e| {
        log::warn!("couldn't read song info for {}: {e}", path.display());
        SongInfo::default()
    })
}

fn read_midi(path: &Path) -> Result<SongInfo, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let list = NoteList::from_smf(&bytes)?;
    let facts = list.facts();
    let mut details = Vec::new();
    if let Some(title) = facts.track_names.first().filter(|n| !n.is_empty()) {
        details.push(("Title".into(), title.clone()));
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
    Ok(SongInfo { length: Some(facts.end_time), details })
}

fn read_audio(path: &Path) -> Result<SongInfo, String> {
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
    let mut add_tags = |tags: &[symphonia::core::meta::Tag]| {
        for tag in tags {
            let key = match tag.std_key {
                Some(key) => tag_name(&format!("{key:?}")),
                None => tag.key.clone(),
            };
            let value = tag.value.to_string();
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
        if let (Some(frames), Some(rate)) = (params.n_frames, params.sample_rate)
            && rate > 0
        {
            length = Some(frames as f64 / f64::from(rate));
        }
        if let Some(length) = length {
            details.push(("Length".into(), format_length(length)));
        }
        if let Some(codec) = symphonia::default::get_codecs().get_codec(params.codec) {
            details.push(("Codec".into(), codec.long_name.to_string()));
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
    Ok(SongInfo { length, details })
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
            let info = read(path);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
