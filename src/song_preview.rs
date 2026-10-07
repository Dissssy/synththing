//! A song's preview in the Online Library (docs/SERVER.md): the bundled
//! keyboard visualizer (`preview::SONG_SCRIPT`) playing the song's preview
//! stretch over and over, drawn and synthesized together by an
//! `offline::Stage` on a thread of its own, in real time. Its sound goes to
//! the app's output through a gate (`Gated`) that's shut while it's muted,
//! so unmuting is heard at once; each picture is handed over when its
//! sound is heard (the ring's backlog says when), so the two stay together
//! whether it's muted or not.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustysynth::{MidiFile, SoundFont};

use crate::audio::{AudioRing, SynthSource};
use crate::lua_visualizer::SettingValue;
use crate::midi_notes::NoteList;
use crate::offline::{Stage, SAMPLE_RATE};

/// Frames a second the preview is drawn at.
const FPS: f64 = 30.0;
/// How far ahead of what's heard it renders (seconds).
const LEAD: f64 = 0.12;

/// Where previews' sound goes: a ring of its own in the app's output, the
/// gate's gain (0 while muted), and which preview owns the ring now.
#[derive(Clone)]
pub struct PreviewOutput {
    ring: AudioRing,
    flush: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
    owner: Arc<AtomicU64>,
}

impl PreviewOutput {
    /// The output, and the source to add to the app's mixer for it.
    pub fn new() -> (Self, Gated) {
        let ring = AudioRing::new(SAMPLE_RATE as usize * 2);
        let flush = Arc::new(AtomicBool::new(false));
        let gain = Arc::new(AtomicU32::new(0f32.to_bits()));
        let source = Gated { inner: SynthSource::new(ring.clone(), Arc::clone(&flush), SAMPLE_RATE), gain: Arc::clone(&gain) };
        (Self { ring, flush, gain, owner: Arc::new(AtomicU64::new(0)) }, source)
    }

    /// How loud it plays: 0 is muted.
    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }
}

/// The preview's source in the mixer: its ring, times the gain.
pub struct Gated {
    inner: SynthSource,
    gain: Arc<AtomicU32>,
}

impl Iterator for Gated {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        self.inner.next().map(|s| s * gain)
    }
}

impl rodio::Source for Gated {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// The newest picture heard: its number (counting up), and its pixels
/// (`0x00RRGGBB`).
type Picture = Option<(u64, Arc<Vec<u32>>)>;

/// One song's preview, playing until it's dropped.
pub struct SongPreview {
    pub width: usize,
    pub height: usize,
    picture: Arc<Mutex<Picture>>,
    error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    output: PreviewOutput,
    id: u64,
}

impl SongPreview {
    /// Start `midi` (a MIDI file's bytes) from `start` seconds, for
    /// `songs::PREVIEW_SECONDS`, played with `soundfont`, drawn `width` by
    /// `height`.
    pub fn start(output: &PreviewOutput, midi: Vec<u8>, soundfont: Arc<SoundFont>, start: f64, width: usize, height: usize) -> Self {
        let id = output.owner.fetch_add(1, Ordering::Relaxed) + 1;
        output.ring.flush(&output.flush);
        let preview = Self {
            width,
            height,
            picture: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            output: output.clone(),
            id,
        };
        let (picture, error, stop, output) =
            (Arc::clone(&preview.picture), Arc::clone(&preview.error), Arc::clone(&preview.stop), output.clone());
        let spawned = std::thread::Builder::new().name("song-preview".into()).spawn(move || {
            if let Err(e) = run(&output, id, &midi, soundfont, start, width, height, &picture, &stop) {
                *error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
            }
        });
        if let Err(e) = spawned {
            *preview.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("couldn't start it: {e}"));
        }
        preview
    }

    /// The newest picture heard, if it's newer than picture `seen`.
    pub fn picture_after(&self, seen: u64) -> Option<(u64, Arc<Vec<u32>>)> {
        self.picture.lock().unwrap_or_else(|p| p.into_inner()).clone().filter(|(n, _)| *n > seen)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Drop for SongPreview {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // (Silent at once, unless another preview has the ring already.)
        if self.output.owner.load(Ordering::Relaxed) == self.id {
            self.output.ring.flush(&self.output.flush);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    output: &PreviewOutput,
    id: u64,
    midi: &[u8],
    soundfont: Arc<SoundFont>,
    start: f64,
    width: usize,
    height: usize,
    picture: &Mutex<Picture>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let source = crate::lua_visualizer::bundled_default(crate::preview::SONG_SCRIPT).ok_or("the keyboard visualizer isn't bundled")?;
    let mut stage = Stage::new(source.to_string(), None, width, height, FPS);
    stage.visualizer.detach_store();
    stage.visualizer.set_setting("play_along", SettingValue::Bool(false));
    stage.engine.set_soundfont(soundfont, "preview".into()).map_err(|e| format!("couldn't use the soundfont: {e}"))?;
    let file = MidiFile::new(&mut &midi[..]).map_err(|e| format!("it isn't a MIDI file the synth can play ({e})"))?;
    stage.engine.load_midi(Arc::new(file), "preview".into(), false);
    stage.visualizer.set_note_list(Arc::new(NoteList::from_smf(midi).unwrap_or_default()));
    stage.visualizer.set_song(None, Some(crate::loader::song_id(midi)));
    let end = start + crate::library::songs::PREVIEW_SECONDS;
    stage.engine.seek(start);

    let lead = (LEAD * f64::from(SAMPLE_RATE)) as u64;
    let backlog = output.ring.backlog();
    // Pictures waiting to be heard: the stereo frame their sound starts at.
    let mut waiting: VecDeque<(u64, Vec<u32>)> = VecDeque::new();
    let (mut pushed, mut shown) = (0u64, 0u64);
    while !stop.load(Ordering::Relaxed) {
        let unheard = backlog.unheard_frames() as u64;
        let heard = pushed.saturating_sub(unheard);
        let mut newest = None;
        while waiting.front().is_some_and(|(at, _)| *at <= heard) {
            newest = waiting.pop_front();
        }
        if let Some((_, pixels)) = newest {
            shown += 1;
            *picture.lock().unwrap_or_else(|p| p.into_inner()) = Some((shown, Arc::new(pixels)));
        }
        if unheard > lead {
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }
        let (samples, view) = stage.frame();
        for request in stage.requests() {
            stage.transport(request);
        }
        waiting.push_back((pushed, stage.pixels.clone()));
        let block: Vec<f32> = samples.iter().flat_map(|&(l, r)| [l, r]).collect();
        if output.owner.load(Ordering::Relaxed) != id {
            break;
        }
        output.ring.push(&block);
        pushed += samples.len() as u64;
        if view.position >= end || view.finished {
            stage.engine.seek(start);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Pictures come as the sound is taken (here by a stand-in for the
    /// speakers), muted or not, and stop with the preview.
    #[test]
    fn previews_play_and_draw() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let midi = std::fs::read(root.join("assets/starter/songs/Ode to Joy.mid")).unwrap();
        let soundfont = SoundFont::new(&mut std::fs::File::open(root.join("assets/starter/soundfonts/TimGM6mb.sf2")).unwrap()).unwrap();
        let (output, mut source) = PreviewOutput::new();
        let speakers = std::thread::spawn(move || {
            // About a second of sound, taken a little faster than it plays.
            let mut loud = 0usize;
            for _ in 0..(SAMPLE_RATE as usize * 2) {
                if source.next().is_some_and(|s| s != 0.0) {
                    loud += 1;
                }
                if loud % 4096 == 0 {
                    std::thread::yield_now();
                }
            }
            loud
        });
        let preview = SongPreview::start(&output, midi, Arc::new(soundfont), 2.0, 64, 36);
        let mut seen = 0;
        for _ in 0..400 {
            if let Some((n, pixels)) = preview.picture_after(seen) {
                assert_eq!(pixels.len(), 64 * 36);
                seen = n;
            }
            if seen >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(preview.error(), None);
        assert!(seen >= 3, "only {seen} pictures");
        // Muted (the gain starts at 0): nothing's heard.
        assert_eq!(speakers.join().unwrap(), 0);
        drop(preview);
    }
}
