//! Audio engine: owns the rustysynth sequencer and exposes a rodio `Source`.
//!
//! The rodio mixer pulls samples from [`SynthSource`] on the audio thread. All
//! mutable playback state lives in [`Engine`] behind an `Arc<Mutex<_>>`; the UI
//! thread locks it to apply commands (play/pause/seek/speed/swap soundfont) and
//! the audio thread `try_lock`s it to render — if a command is mid-flight the
//! audio thread just emits a few milliseconds of silence instead of blocking.

use std::num::{NonZeroU16, NonZeroU32};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use rodio::Source;
use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};

use crate::visualizer::SampleTap;

pub const MIN_SPEED: f64 = 0.10;
pub const MAX_SPEED: f64 = 4.00;

/// How many stereo frames the [`SynthSource`] renders per lock of the engine.
/// 1024 frames ≈ 23 ms at 44.1 kHz, which bounds how stale pause/seek feels.
const CHUNK_FRAMES: usize = 1024;

/// A immutable snapshot of the engine, taken under lock for rendering the UI.
#[derive(Clone, Default)]
pub struct EngineView {
    pub midi_name: Option<String>,
    pub soundfont_name: Option<String>,
    pub has_midi: bool,
    pub has_soundfont: bool,
    pub position: f64,
    pub length: f64,
    pub speed: f64,
    pub paused: bool,
    pub finished: bool,
}

pub struct Engine {
    sample_rate: u32,
    seq: Option<MidiFileSequencer>,
    midi: Option<Arc<MidiFile>>,
    midi_name: Option<String>,
    soundfont_name: Option<String>,
    length: f64,
    speed: f64,
    paused: bool,
    finished: bool,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl Engine {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            seq: None,
            midi: None,
            midi_name: None,
            soundfont_name: None,
            length: 0.0,
            speed: 1.0,
            paused: true,
            finished: false,
            scratch_l: Vec::new(),
            scratch_r: Vec::new(),
        }
    }

    pub fn view(&self) -> EngineView {
        EngineView {
            midi_name: self.midi_name.clone(),
            soundfont_name: self.soundfont_name.clone(),
            has_midi: self.midi.is_some(),
            has_soundfont: self.seq.is_some(),
            position: self.position().clamp(0.0, self.length.max(0.0)),
            length: self.length,
            speed: self.speed,
            paused: self.paused,
            finished: self.finished,
        }
    }

    fn position(&self) -> f64 {
        self.seq.as_ref().map(|s| s.get_position()).unwrap_or(0.0)
    }

    /// Swap in a new soundfont, rebuilding the synthesizer while keeping the
    /// current MIDI file, playback position, speed and pause state.
    pub fn set_soundfont(&mut self, soundfont: Arc<SoundFont>, name: String) -> Result<()> {
        let settings = SynthesizerSettings::new(self.sample_rate as i32);
        let synth = Synthesizer::new(&soundfont, &settings).map_err(|e| anyhow!("{e}"))?;
        let mut seq = MidiFileSequencer::new(synth);
        seq.set_speed(self.speed);

        let target = self.position();
        if let Some(midi) = &self.midi {
            seq.play(midi, false);
            fast_forward(&mut seq, target, self.sample_rate);
        }

        self.seq = Some(seq);
        self.soundfont_name = Some(name);
        Ok(())
    }

    /// Load a MIDI file and start playing it from the top (if a soundfont is loaded).
    pub fn load_midi(&mut self, midi: Arc<MidiFile>, name: String) {
        self.length = midi.get_length();
        self.midi_name = Some(name);
        self.finished = false;
        self.paused = false;

        if let Some(seq) = &mut self.seq {
            seq.play(&midi, false);
            seq.set_speed(self.speed);
        }
        self.midi = Some(midi);
    }

    pub fn toggle_pause(&mut self) {
        if self.midi.is_none() {
            return;
        }
        if self.finished {
            // Restart from the top on "play" after the track has ended.
            self.seek(0.0);
            self.paused = false;
            return;
        }
        self.paused = !self.paused;
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        if let Some(seq) = &mut self.seq {
            seq.set_speed(self.speed);
        }
    }

    pub fn nudge_speed(&mut self, delta: f64) {
        self.set_speed(self.speed + delta);
    }

    /// Seek to an absolute position in seconds. rustysynth has no seek, so we
    /// restart the sequence and fast-forward through it silently.
    pub fn seek(&mut self, target: f64) {
        let Some(midi) = self.midi.clone() else {
            return;
        };
        let Some(seq) = self.seq.as_mut() else {
            return;
        };
        let target = target.clamp(0.0, self.length.max(0.0));
        seq.play(&midi, false);
        seq.set_speed(self.speed);
        fast_forward(seq, target, self.sample_rate);
        self.finished = seq.end_of_sequence();
    }

    pub fn seek_relative(&mut self, delta: f64) {
        self.seek(self.position() + delta);
    }

    /// Fill an interleaved stereo buffer. Called on the audio thread.
    fn render_into(&mut self, out: &mut [f32]) {
        let silent = self.paused || self.finished || self.midi.is_none() || self.seq.is_none();
        if silent {
            out.fill(0.0);
            return;
        }

        let frames = out.len() / 2;
        self.scratch_l.resize(frames, 0.0);
        self.scratch_r.resize(frames, 0.0);

        let seq = self.seq.as_mut().unwrap();
        seq.render(&mut self.scratch_l, &mut self.scratch_r);
        for i in 0..frames {
            out[2 * i] = self.scratch_l[i];
            out[2 * i + 1] = self.scratch_r[i];
        }

        if seq.end_of_sequence() {
            self.finished = true;
            self.paused = true;
        }
    }
}

/// Restart-and-scan seek helper. Renders the sequence into a throwaway buffer at
/// an inflated speed until the target position is reached, then restores speed.
fn fast_forward(seq: &mut MidiFileSequencer, target: f64, _sample_rate: u32) {
    if target <= 0.0 {
        return;
    }
    let saved = seq.get_speed();
    let mut l = [0.0f32; 1024];
    let mut r = [0.0f32; 1024];
    let mut guard: u32 = 0;

    // Coarse pass: race to ~1s before the target.
    seq.set_speed(32.0);
    while seq.get_position() < target - 1.0 && !seq.end_of_sequence() {
        seq.render(&mut l, &mut r);
        guard += 1;
        if guard > 2_000_000 {
            break;
        }
    }

    // Fine pass: close the last stretch with small steps.
    seq.set_speed(4.0);
    while seq.get_position() < target && !seq.end_of_sequence() {
        seq.render(&mut l, &mut r);
        guard += 1;
        if guard > 2_000_000 {
            break;
        }
    }

    seq.set_speed(saved);
}

/// The rodio `Source` handed to the mixer. Never ends: emits silence when idle.
pub struct SynthSource {
    engine: Arc<Mutex<Engine>>,
    sample_rate: u32,
    buf: Vec<f32>,
    pos: usize,
    /// Optional tap that a visualizer window drains from; see `visualizer.rs`.
    tap: Option<SampleTap>,
}

impl SynthSource {
    pub fn new(engine: Arc<Mutex<Engine>>, sample_rate: u32) -> Self {
        Self {
            engine,
            sample_rate,
            buf: Vec::new(),
            pos: 0,
            tap: None,
        }
    }

    /// Mirror every rendered chunk (including silence) into `tap`, so a
    /// visualizer can see exactly what's playing.
    pub fn with_tap(mut self, tap: SampleTap) -> Self {
        self.tap = Some(tap);
        self
    }
}

impl Iterator for SynthSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() {
            self.buf.resize(CHUNK_FRAMES * 2, 0.0);
            match self.engine.try_lock() {
                Ok(mut engine) => engine.render_into(&mut self.buf),
                Err(_) => self.buf.fill(0.0),
            }
            if let Some(tap) = &self.tap {
                tap.push(&self.buf);
            }
            self.pos = 0;
        }
        let sample = self.buf[self.pos];
        self.pos += 1;
        Some(sample)
    }
}

impl Source for SynthSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        NonZeroU16::new(2).expect("2 is nonzero")
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        NonZeroU32::new(self.sample_rate).expect("sample rate is nonzero")
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
