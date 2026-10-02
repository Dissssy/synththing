//! Background loading and preloading of songs and soundfonts.
//!
//! `AssetCache` is a store keyed by file path: each entry is a file that's
//! loading, loaded, or failed, plus when it was last needed. Loading
//! (MIDI parsing, decoding a whole audio file, parsing a soundfont) happens
//! on a couple of worker threads, never the UI thread; results are picked
//! up by `poll` once a frame.
//!
//! The app says what it currently needs each frame (`touch`/`request`): the
//! playing track, the next one up, the soundfonts in use, whatever's being
//! hovered when hover preloading is on. Anything not needed for longer than
//! the expiry gets unloaded by `evict`. Paths are the keys on the
//! assumption that a file doesn't change under the same path while the app
//! has it loaded; if one does, the stale copy only lives until it expires.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use rodio::Source as _;
use rustysynth::{MidiFile, SoundFont};
use sha2::{Digest, Sha256};

use crate::engine::DecodedAudio;
use crate::midi_notes::NoteList;
use crate::visualizer::StereoFrame;

/// Loads run on this many threads: enough that a big soundfont parse
/// doesn't hold up the next song, few enough not to fight the synth thread.
const WORKER_THREADS: usize = 2;

/// At most this many loaded files that nothing currently needs are kept
/// (most recently needed first) even before they expire. Decoded audio is
/// large (a 5 minute song is ~100 MB as samples), so hovering down a long
/// list shouldn't be able to pile up gigabytes.
const MAX_IDLE_ENTRIES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Midi,
    Audio,
    SoundFont,
}

/// A parsed MIDI file plus every note in it (for `notes_between`).
#[derive(Clone)]
pub struct LoadedMidi {
    pub file: Arc<MidiFile>,
    pub notes: Arc<NoteList>,
    /// See [`song_id`].
    pub song_id: String,
}

/// A song's identity for scripts (`playback().song_id`): the start of the
/// SHA-256 of the file's bytes, so it follows the song itself, not where
/// it's kept or what it's called.
pub fn song_id(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone)]
pub enum Asset {
    Midi(LoadedMidi),
    Audio(Arc<DecodedAudio>),
    SoundFont(Arc<SoundFont>),
}

#[derive(Clone)]
pub enum LoadState {
    /// Queued or being loaded right now.
    Loading,
    Ready(Asset),
    Failed(String),
}

/// How soon a load should start. Something the user just clicked jumps
/// the queue; preloads wait their turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Now,
    Background,
}

struct Entry {
    kind: AssetKind,
    state: LoadState,
    /// Last time anything said it needed this file.
    last_wanted: Instant,
}

struct Job {
    path: PathBuf,
    kind: AssetKind,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    shutdown: bool,
}

type LoadResult = (PathBuf, std::result::Result<Asset, String>);

pub struct AssetCache {
    entries: HashMap<PathBuf, Entry>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    results: Receiver<LoadResult>,
}

impl AssetCache {
    pub fn new() -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (tx, results) = mpsc::channel();
        for i in 0..WORKER_THREADS {
            let queue = Arc::clone(&queue);
            let tx: Sender<LoadResult> = tx.clone();
            let _ = thread::Builder::new()
                .name(format!("loader-{i}"))
                .spawn(move || worker(&queue, &tx));
        }
        Self { entries: HashMap::new(), queue, results }
    }

    /// Ask for `path` to be loaded (if it isn't already) and mark it needed.
    /// A `Priority::Now` request also retries a file that failed before, and
    /// moves a still-queued load to the front.
    pub fn request(&mut self, path: &Path, kind: AssetKind, priority: Priority, now: Instant) {
        let retry_failed = priority == Priority::Now;
        match self.entries.get_mut(path) {
            Some(entry) => {
                entry.last_wanted = now;
                match &entry.state {
                    LoadState::Ready(_) => return,
                    LoadState::Failed(_) if !retry_failed => return,
                    LoadState::Loading => {
                        if priority == Priority::Now {
                            self.bump(path);
                        }
                        return;
                    }
                    LoadState::Failed(_) => entry.state = LoadState::Loading,
                }
            }
            None => {
                self.entries.insert(
                    path.to_path_buf(),
                    Entry { kind, state: LoadState::Loading, last_wanted: now },
                );
            }
        }
        self.enqueue(Job { path: path.to_path_buf(), kind }, priority);
    }

    /// Mark `path` needed right now, without loading it if it isn't loaded.
    pub fn touch(&mut self, path: &Path, now: Instant) {
        if let Some(entry) = self.entries.get_mut(path) {
            entry.last_wanted = now;
        }
    }

    pub fn get(&self, path: &Path) -> Option<&LoadState> {
        self.entries.get(path).map(|e| &e.state)
    }

    /// Put a file loaded some other way into the store (e.g. a soundfont
    /// already parsed while validating it).
    pub fn insert_ready(&mut self, path: &Path, asset: Asset, now: Instant) {
        let kind = match asset {
            Asset::Midi(_) => AssetKind::Midi,
            Asset::Audio(_) => AssetKind::Audio,
            Asset::SoundFont(_) => AssetKind::SoundFont,
        };
        self.entries.insert(path.to_path_buf(), Entry { kind, state: LoadState::Ready(asset), last_wanted: now });
    }

    /// Collect finished loads. Returns whether anything finished (so the UI
    /// can react this frame). A result for a file that's since been evicted
    /// is dropped.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((path, result)) = self.results.try_recv() {
            any = true;
            if let Some(entry) = self.entries.get_mut(&path)
                && matches!(entry.state, LoadState::Loading)
            {
                entry.state = match result {
                    Ok(asset) => LoadState::Ready(asset),
                    Err(e) => {
                        log::warn!("couldn't load {}: {e}", path.display());
                        LoadState::Failed(e)
                    }
                };
            }
        }
        any
    }

    /// Unload everything that hasn't been needed for longer than `expiry`,
    /// then trim what's left that isn't needed right now down to
    /// `MAX_IDLE_ENTRIES`. Call after this frame's `touch`/`request`es:
    /// "needed right now" means touched at exactly `now`.
    pub fn evict(&mut self, now: Instant, expiry: Duration) {
        let mut evicted: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|(_, e)| now.saturating_duration_since(e.last_wanted) > expiry)
            .map(|(p, _)| p.clone())
            .collect();

        let mut idle: Vec<(&PathBuf, Instant)> = self
            .entries
            .iter()
            .filter(|(p, e)| e.last_wanted < now && !evicted.contains(p))
            .map(|(p, e)| (p, e.last_wanted))
            .collect();
        if idle.len() > MAX_IDLE_ENTRIES {
            idle.sort_by_key(|&(_, t)| std::cmp::Reverse(t));
            evicted.extend(idle.drain(MAX_IDLE_ENTRIES..).map(|(p, _)| p.clone()));
        }

        if evicted.is_empty() {
            return;
        }
        // A load that hasn't started yet doesn't need to happen at all.
        if let Ok(mut queue) = self.queue.0.lock() {
            queue.jobs.retain(|job| !evicted.contains(&job.path));
        }
        for path in evicted {
            self.entries.remove(&path);
        }
    }

    pub fn any_loading(&self) -> bool {
        self.entries.values().any(|e| matches!(e.state, LoadState::Loading))
    }

    /// (path, kind, state) for every entry, for showing what's loaded.
    pub fn summary(&self) -> Vec<(PathBuf, AssetKind, &'static str)> {
        let mut list: Vec<_> = self
            .entries
            .iter()
            .map(|(p, e)| {
                let state = match e.state {
                    LoadState::Loading => "loading",
                    LoadState::Ready(_) => "ready",
                    LoadState::Failed(_) => "failed",
                };
                (p.clone(), e.kind, state)
            })
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    fn enqueue(&self, job: Job, priority: Priority) {
        let (lock, cvar) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.jobs.retain(|j| j.path != job.path);
            match priority {
                Priority::Now => queue.jobs.push_front(job),
                Priority::Background => queue.jobs.push_back(job),
            }
            cvar.notify_one();
        }
    }

    /// Move a still-queued load to the front.
    fn bump(&self, path: &Path) {
        if let Ok(mut queue) = self.queue.0.lock()
            && let Some(pos) = queue.jobs.iter().position(|j| j.path == path)
            && let Some(job) = queue.jobs.remove(pos)
        {
            queue.jobs.push_front(job);
        }
    }
}

impl Drop for AssetCache {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.shutdown = true;
            queue.jobs.clear();
        }
        cvar.notify_all();
    }
}

fn worker(queue: &(Mutex<Queue>, Condvar), results: &Sender<LoadResult>) {
    let (lock, cvar) = queue;
    loop {
        let job = {
            let Ok(mut q) = lock.lock() else { return };
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(job) = q.jobs.pop_front() {
                    break job;
                }
                q = match cvar.wait(q) {
                    Ok(q) => q,
                    Err(_) => return,
                };
            }
        };
        let result = load(&job.path, job.kind).map_err(|e| e.to_string());
        if results.send((job.path, result)).is_err() {
            return;
        }
    }
}

fn load(path: &Path, kind: AssetKind) -> Result<Asset> {
    match kind {
        AssetKind::Midi => load_midi(path).map(Asset::Midi),
        AssetKind::Audio => load_audio_file(path).map(Asset::Audio),
        AssetKind::SoundFont => match probe_soundfont(path)? {
            SoundFontProbe::Playable(sf) => Ok(Asset::SoundFont(sf)),
            SoundFontProbe::Unplayable(reason) => Err(anyhow!("{reason}")),
            SoundFontProbe::NotSoundFont => Err(anyhow!("not a SoundFont file (no RIFF/sfbk header)")),
        },
    }
}

pub fn load_midi(path: &Path) -> Result<LoadedMidi> {
    let bytes = std::fs::read(path)?;
    let file = MidiFile::new(&mut bytes.as_slice()).map_err(|e| anyhow!("{e}"))?;
    // A file rustysynth plays but this can't list notes from still plays;
    // scripts just see no notes from notes_between().
    let notes = NoteList::from_smf(&bytes).unwrap_or_else(|e| {
        log::warn!("couldn't list the notes in {}: {e}", path.display());
        NoteList::default()
    });
    Ok(LoadedMidi { file: Arc::new(file), notes: Arc::new(notes), song_id: song_id(&bytes) })
}

/// Decode a plain audio file (wav/mp3/ogg/flac/...) fully into memory up
/// front, seeking and speed then just become arithmetic on an index, no
/// re-decoding needed. Mono is duplicated to stereo; anything beyond stereo
/// keeps only its first two channels.
pub fn load_audio_file(path: &Path) -> Result<Arc<DecodedAudio>> {
    let bytes = std::fs::read(path)?;
    let id = song_id(&bytes);
    let decoder = rodio::Decoder::new(Cursor::new(bytes)).map_err(|e| anyhow!("{e}"))?;
    let channels = decoder.channels().get() as usize;
    let sample_rate = decoder.sample_rate().get();

    let raw: Vec<f32> = decoder.collect();
    let samples: Vec<StereoFrame> = if channels <= 1 {
        raw.into_iter().map(|s| (s, s)).collect()
    } else {
        raw.chunks_exact(channels).map(|f| (f[0], f[1])).collect()
    };
    if samples.is_empty() {
        return Err(anyhow!("no audio data decoded"));
    }

    Ok(Arc::new(DecodedAudio { samples, sample_rate, song_id: id }))
}

pub enum SoundFontProbe {
    /// rustysynth parsed it and can render with it.
    Playable(Arc<SoundFont>),
    /// A real SoundFont container, but this build of rustysynth can't use it.
    Unplayable(String),
    /// Not a RIFF/sfbk file at all.
    NotSoundFont,
}

pub fn probe_soundfont(path: &Path) -> Result<SoundFontProbe> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"sfbk" {
        return Ok(SoundFontProbe::NotSoundFont);
    }
    match SoundFont::new(&mut bytes.as_slice()) {
        Ok(soundfont) => Ok(SoundFontProbe::Playable(Arc::new(soundfont))),
        Err(err) => Ok(SoundFontProbe::Unplayable(describe_soundfont_error(&err.to_string(), &bytes))),
    }
}

/// Turn a rustysynth parse failure into something actionable. The common case
/// for "valid" soundfonts that fail here is a compressed SF3 (Ogg/FLAC sample
/// data), which rustysynth does not support; FluidSynth does, which is why it
/// plays elsewhere.
fn describe_soundfont_error(base: &str, bytes: &[u8]) -> String {
    if contains_bytes(bytes, b"OggS") || contains_bytes(bytes, b"fLaC") {
        "it's a compressed SF3 soundfont. rustysynth only plays uncompressed SF2, \
         convert it with Polyphone (File > Save as... > .sf2) and add that file."
            .to_string()
    } else {
        format!("rustysynth rejected it ({base}).")
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8; 4]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(cache: &mut AssetCache, path: &Path) -> LoadState {
        for _ in 0..500 {
            cache.poll();
            match cache.get(path) {
                Some(LoadState::Loading) | None => thread::sleep(Duration::from_millis(5)),
                Some(state) => return state.clone(),
            }
        }
        panic!("load never finished");
    }

    #[test]
    fn failed_loads_report_and_only_clicks_retry() {
        let mut cache = AssetCache::new();
        let path = Path::new("definitely/not/here.mid");
        let t0 = Instant::now();
        cache.request(path, AssetKind::Midi, Priority::Now, t0);
        assert!(matches!(wait_for(&mut cache, path), LoadState::Failed(_)));
        // A background request (hover/preload) doesn't hammer a failed file...
        cache.request(path, AssetKind::Midi, Priority::Background, t0);
        assert!(matches!(cache.get(path), Some(LoadState::Failed(_))));
        // ...a click does.
        cache.request(path, AssetKind::Midi, Priority::Now, t0);
        assert!(matches!(cache.get(path), Some(LoadState::Loading)));
    }

    #[test]
    fn loads_a_real_midi_file_in_the_background() {
        let dir = std::env::temp_dir().join(format!("synththing-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tiny.mid");
        // Format 0, one track, 96 ticks/quarter: a single note on/off.
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\0\x60".to_vec();
        let track = [0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00];
        bytes.extend_from_slice(b"MTrk");
        bytes.extend_from_slice(&(track.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&track);
        std::fs::write(&path, bytes).unwrap();

        let mut cache = AssetCache::new();
        cache.request(&path, AssetKind::Midi, Priority::Background, Instant::now());
        assert!(matches!(wait_for(&mut cache, &path), LoadState::Ready(Asset::Midi(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eviction_keeps_whats_needed_and_drops_the_stale() {
        let mut cache = AssetCache::new();
        let t0 = Instant::now();
        let expiry = Duration::from_secs(10);
        let needed = Path::new("needed.mid");
        let stale = Path::new("stale.mid");
        cache.request(needed, AssetKind::Midi, Priority::Background, t0);
        cache.request(stale, AssetKind::Midi, Priority::Background, t0);

        let later = t0 + Duration::from_secs(11);
        cache.touch(needed, later);
        cache.evict(later, expiry);
        assert!(cache.get(needed).is_some());
        assert!(cache.get(stale).is_none());
    }

    #[test]
    fn idle_entries_are_capped_newest_first() {
        let mut cache = AssetCache::new();
        let t0 = Instant::now();
        let paths: Vec<PathBuf> = (0..MAX_IDLE_ENTRIES + 3).map(|i| PathBuf::from(format!("{i}.mid"))).collect();
        for (i, p) in paths.iter().enumerate() {
            cache.request(p, AssetKind::Midi, Priority::Background, t0 + Duration::from_millis(i as u64));
        }
        let now = t0 + Duration::from_secs(1);
        cache.evict(now, Duration::from_secs(60));
        let kept: Vec<_> = paths.iter().filter(|p| cache.get(p).is_some()).collect();
        assert_eq!(kept.len(), MAX_IDLE_ENTRIES);
        // The most recently wanted ones survive.
        assert!(cache.get(paths.last().unwrap()).is_some());
        assert!(cache.get(&paths[0]).is_none());
    }
}
