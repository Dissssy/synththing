//! The playback engine: either a rustysynth MIDI sequencer or a decoded plain
//! audio file, turning commands (play/pause/seek/speed/swap soundfont/mute
//! channel) into rendered audio. It runs entirely on the dedicated render
//! thread (`audio.rs`), no locking, since nothing else touches it.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use rustysynth::{MidiFile, MidiFileSequencer, SoundFont, Synthesizer, SynthesizerSettings};

use crate::live::{LiveCommand, LiveSynth};
use crate::visualizer::{ActiveNote, NoteChange, NotesSnapshot, StereoFrame};

pub const MIN_SPEED: f64 = 0.10;
pub const MAX_SPEED: f64 = 4.00;

/// How far ahead `notes_snapshot` reports upcoming note changes.
pub const NOTE_LOOKAHEAD_SECS: f64 = 4.0;

/// A whole plain audio file (wav/mp3/ogg/flac/...), decoded up front. Seeking
/// and speed are then just arithmetic on an index into `samples`, no
/// re-decoding needed.
pub struct DecodedAudio {
    pub samples: Vec<StereoFrame>,
    pub sample_rate: u32,
    /// See `loader::song_id`.
    pub song_id: String,
}

/// Playback head into a [`DecodedAudio`].
struct AudioFilePlayback {
    data: Arc<DecodedAudio>,
    /// Fractional index into `data.samples`, in source-sample units.
    position: f64,
}

/// A cloneable snapshot of playback state, published to the GUI each tick.
#[derive(Clone, Default)]
pub struct EngineView {
    pub track_name: Option<String>,
    pub soundfont_name: Option<String>,
    pub has_midi: bool,
    pub has_audio_file: bool,
    pub has_soundfont: bool,
    pub position: f64,
    pub length: f64,
    pub speed: f64,
    pub paused: bool,
    pub finished: bool,
    pub loop_enabled: bool,
    /// Bumps on every seek, every loop back to the start, and every newly
    /// loaded song: anything that makes `position` jump rather than run on.
    pub generation: u64,
}

pub struct Engine {
    sample_rate: u32,
    seq: Option<MidiFileSequencer>,
    midi: Option<Arc<MidiFile>>,
    audio_file: Option<AudioFilePlayback>,
    track_name: Option<String>,
    soundfont_name: Option<String>,
    length: f64,
    speed: f64,
    paused: bool,
    finished: bool,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    /// Channels the current MIDI uses, sorted; empty for a plain audio file.
    detected_channels: Vec<u8>,
    /// Per-channel visual enable flag (index = channel). Purely a hint for
    /// visualizer scripts, disabled channels still play.
    channels_enabled: [bool; 16],
    /// Restart from the top instead of finishing, when the track runs out.
    loop_enabled: bool,
    /// See `EngineView::generation`.
    generation: u64,
    /// Notes scripts play (see `live.rs`); built with the soundfont.
    live: Option<LiveSynth>,
}

impl Engine {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            seq: None,
            midi: None,
            audio_file: None,
            track_name: None,
            soundfont_name: None,
            length: 0.0,
            speed: 1.0,
            paused: true,
            finished: false,
            scratch_l: Vec::new(),
            scratch_r: Vec::new(),
            detected_channels: Vec::new(),
            channels_enabled: [true; 16],
            loop_enabled: false,
            generation: 0,
            live: None,
        }
    }

    pub fn set_loop_enabled(&mut self, enabled: bool) {
        self.loop_enabled = enabled;
    }

    pub fn view(&self) -> EngineView {
        EngineView {
            track_name: self.track_name.clone(),
            soundfont_name: self.soundfont_name.clone(),
            has_midi: self.midi.is_some(),
            has_audio_file: self.audio_file.is_some(),
            has_soundfont: self.seq.is_some(),
            position: self.position().clamp(0.0, self.length.max(0.0)),
            length: self.length,
            speed: self.speed,
            paused: self.paused,
            finished: self.finished,
            loop_enabled: self.loop_enabled,
            generation: self.generation,
        }
    }

    fn position(&self) -> f64 {
        if let Some(audio) = &self.audio_file {
            return audio.position / audio.data.sample_rate as f64;
        }
        self.seq.as_ref().map(|s| s.get_position()).unwrap_or(0.0)
    }

    /// Which MIDI notes the score holds down right now, plus the note-on/off
    /// changes coming within the next [`NOTE_LOOKAHEAD_SECS`] seconds. Always
    /// empty for a plain audio file, there's no score to read.
    pub fn notes_snapshot(&self) -> NotesSnapshot {
        let base = NotesSnapshot {
            detected_channels: self.detected_channels.clone(),
            enabled_channels: self.channels_enabled,
            ..NotesSnapshot::default()
        };

        let Some(seq) = &self.seq else {
            return base;
        };
        if self.midi.is_none() {
            return base;
        }

        // The score's held notes, then the script's (see live.rs).
        let mut active: Vec<ActiveNote> = seq
            .active_notes()
            .into_iter()
            .map(|n| ActiveNote {
                channel: n.channel as u8,
                key: n.key as u8,
                velocity: n.velocity as u8,
                from_script: false,
            })
            .collect();
        if let Some(live) = &self.live {
            active.extend(live.sounding_keys().into_iter().map(|(channel, key, velocity)| ActiveNote {
                channel,
                key,
                velocity,
                from_script: true,
            }));
        }

        NotesSnapshot {
            active,
            upcoming: seq
                .upcoming_notes(NOTE_LOOKAHEAD_SECS)
                .into_iter()
                .map(|e| NoteChange {
                    channel: e.channel as u8,
                    key: e.key as u8,
                    velocity: e.velocity as u8,
                    on: e.on,
                    seconds_until: e.time_until as f32,
                })
                .collect(),
            ..base
        }
    }

    /// Swap in a new soundfont, rebuilding the synthesizer while keeping the
    /// current MIDI file, playback position, speed and pause state. A no-op
    /// while a plain audio file is loaded (it has nothing to do with one),
    /// beyond remembering the name for whenever a MIDI is loaded next.
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
        seq.set_channel_mask(self.channel_mask());

        self.live = match LiveSynth::new(&soundfont, self.sample_rate) {
            Ok(live) => Some(live),
            Err(e) => {
                log::warn!("no live synth for script notes: {e}");
                None
            }
        };
        self.seq = Some(seq);
        self.soundfont_name = Some(name);
        Ok(())
    }

    /// Load a MIDI file and start playing it from the top (if a soundfont is loaded).
    pub fn load_midi(&mut self, midi: Arc<MidiFile>, name: String) {
        self.generation += 1;
        self.audio_file = None;
        self.length = midi.get_length();
        self.track_name = Some(name);
        self.finished = false;
        self.paused = false;
        self.detected_channels = midi.note_channels();
        self.channels_enabled = [true; 16];
        self.live_command(LiveCommand::StopAll);

        if let Some(seq) = &mut self.seq {
            seq.play(&midi, false);
            seq.set_speed(self.speed);
            seq.set_channel_mask(0xFFFF);
        }
        self.midi = Some(midi);
    }

    /// Load a plain audio file (already decoded) and start playing it from the
    /// top. No soundfont needed, there's nothing to synthesize.
    pub fn load_audio_file(&mut self, data: Arc<DecodedAudio>, name: String) {
        self.generation += 1;
        self.midi = None;
        self.length = data.samples.len() as f64 / data.sample_rate as f64;
        self.track_name = Some(name);
        self.finished = false;
        self.paused = false;
        self.detected_channels.clear();
        self.channels_enabled = [true; 16];
        self.live_command(LiveCommand::StopAll);
        self.audio_file = Some(AudioFilePlayback { data, position: 0.0 });
    }

    /// Notes from a script. Only while a MIDI is loaded (with a soundfont):
    /// the live synth borrows that song's instruments.
    pub fn live_command(&mut self, command: LiveCommand) {
        let playable = self.midi.is_some() || command == LiveCommand::StopAll;
        if let Some(live) = &mut self.live
            && playable
        {
            live.apply(command);
        }
    }

    /// Whether the live synth has anything to render.
    pub fn live_busy(&self) -> bool {
        self.midi.is_some() && self.seq.is_some() && self.live.as_ref().is_some_and(LiveSynth::busy)
    }

    /// Render the live synth's next block (interleaved stereo), whether or
    /// not the song is paused.
    pub fn render_live(&mut self, out: &mut [f32]) {
        match (&mut self.live, &self.seq) {
            (Some(live), Some(seq)) => live.render(seq.get_synthesizer(), out),
            _ => out.fill(0.0),
        }
    }

    /// Bit N set = channel N audible / visually enabled.
    fn channel_mask(&self) -> u16 {
        (0..16)
            .filter(|&i| self.channels_enabled[i])
            .map(|i| 1u16 << i)
            .sum()
    }

    /// Flip a channel's enable flag. This drives both the visualizer (through
    /// [`notes_snapshot`](Self::notes_snapshot)) and playback, a disabled
    /// channel's note-ons are dropped and its ringing voices released. No
    /// effect while a plain audio file is loaded (it has no channels).
    ///
    /// Refuses to disable the one remaining enabled channel, silence isn't
    /// a useful state to land in by toggling, and the GUI mirrors this by
    /// graying out that channel's checkbox.
    pub fn set_channel_enabled(&mut self, channel: u8, enabled: bool) {
        if !enabled && self.is_last_enabled_detected_channel(channel) {
            return;
        }
        if let Some(slot) = self.channels_enabled.get_mut(channel as usize) {
            *slot = enabled;
        }
        self.apply_channel_mask();
    }

    /// Enable every channel.
    pub fn enable_all_channels(&mut self) {
        self.channels_enabled = [true; 16];
        self.apply_channel_mask();
    }

    /// Enable only `channel`, disabling every other channel.
    pub fn solo_channel(&mut self, channel: u8) {
        for (i, slot) in self.channels_enabled.iter_mut().enumerate() {
            *slot = i == channel as usize;
        }
        self.apply_channel_mask();
    }

    fn is_last_enabled_detected_channel(&self, channel: u8) -> bool {
        !self.detected_channels.is_empty()
            && self.channels_enabled.get(channel as usize).copied().unwrap_or(false)
            && self
                .detected_channels
                .iter()
                .filter(|&&c| self.channels_enabled[c as usize])
                .count()
                <= 1
    }

    fn apply_channel_mask(&mut self) {
        let mask = self.channel_mask();
        if let Some(seq) = &mut self.seq {
            seq.set_channel_mask(mask);
        }
    }

    fn has_track(&self) -> bool {
        self.midi.is_some() || self.audio_file.is_some()
    }

    pub fn toggle_pause(&mut self) {
        if !self.has_track() {
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

    /// Seek to an absolute position in seconds. For MIDI, rustysynth has no
    /// seek, so we restart the sequence and fast-forward through it silently;
    /// for a decoded audio file it's just moving the read head.
    pub fn seek(&mut self, target: f64) {
        self.generation += 1;
        let target = target.clamp(0.0, self.length.max(0.0));

        if let Some(audio) = &mut self.audio_file {
            audio.position = target * audio.data.sample_rate as f64;
            self.finished = !self.loop_enabled && audio.position >= audio.data.samples.len() as f64;
            return;
        }

        let Some(midi) = self.midi.clone() else {
            return;
        };
        let Some(seq) = self.seq.as_mut() else {
            return;
        };
        seq.play(&midi, false);
        seq.set_speed(self.speed);
        fast_forward(seq, target, self.sample_rate);
        self.finished = seq.end_of_sequence();
    }

    pub fn seek_relative(&mut self, delta: f64) {
        self.seek(self.position() + delta);
    }

    /// Fill an interleaved stereo buffer. Called on the render thread (see
    /// `audio.rs`), never on the audio callback itself.
    pub fn render_into(&mut self, out: &mut [f32]) {
        if self.paused || self.finished {
            out.fill(0.0);
            return;
        }

        if let Some(audio) = self.audio_file.as_mut() {
            let reached_end =
                render_audio_file(audio, self.speed, self.sample_rate, self.loop_enabled, out);
            if reached_end {
                if self.loop_enabled {
                    // Wrapped back to the start inside render_audio_file.
                    self.generation += 1;
                } else {
                    self.finished = true;
                    self.paused = true;
                }
            }
            return;
        }

        if self.midi.is_none() || self.seq.is_none() {
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
            if self.loop_enabled {
                self.seek(0.0);
            } else {
                self.finished = true;
                self.paused = true;
            }
        }
    }
}

/// Linear-interpolation resample from `audio`'s native sample rate into
/// `output_rate`, advancing the read head by `speed` extra, folding "play
/// faster" and "convert sample rate" into the same step (speed changes pitch,
/// same as speeding up a tape, since there's no time-stretching here).
///
/// When `looped`, the read head wraps back to the start as soon as it passes
/// the end, mid-block if needed, so the loop is sample-accurate with no gap
/// of silence at the seam. Returns whether the read head reached (or passed)
/// the end of the data during this call.
fn render_audio_file(
    audio: &mut AudioFilePlayback,
    speed: f64,
    output_rate: u32,
    looped: bool,
    out: &mut [f32],
) -> bool {
    let samples = &audio.data.samples;
    let step = (audio.data.sample_rate as f64 / output_rate as f64) * speed;
    let frames = out.len() / 2;
    let mut reached_end = false;

    for i in 0..frames {
        let index = audio.position;
        let base = index.floor() as usize;
        if base >= samples.len() {
            out[2 * i] = 0.0;
            out[2 * i + 1] = 0.0;
            reached_end = true;
        } else {
            let frac = (index - base as f64) as f32;
            let (l0, r0) = samples[base];
            let (l1, r1) = samples.get(base + 1).copied().unwrap_or((l0, r0));
            out[2 * i] = l0 + (l1 - l0) * frac;
            out[2 * i + 1] = r0 + (r1 - r0) * frac;
        }

        audio.position += step;
        if looped && !samples.is_empty() {
            audio.position = audio.position.rem_euclid(samples.len() as f64);
        }
    }

    reached_end
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
