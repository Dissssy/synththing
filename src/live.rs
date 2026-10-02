//! Notes a visualizer script plays itself (`play_note`, `note_on`,
//! sequences), on a second synthesizer next to the song's.
//!
//! The song's synthesizer renders `buffer_ms` ahead of what's heard, so a
//! note handed to it would sound that much late; fine for a score, sluggish
//! for playing along on the keyboard. The live synth instead renders into a
//! small ring of its own (see `audio.rs`), mixed in at the output, so a key
//! press is heard within a few tens of milliseconds. Before every block it
//! copies the song synthesizer's channel state (instrument, volume, pan,
//! pitch bend, ...), so a script note on channel 3 sounds like the song's
//! channel 3, and it never trips over the song's own note-offs.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

/// Keep rendering this long after the last note ends, for the release and
/// reverb tails, then go idle until the next note.
const TAIL_SECONDS: f64 = 3.0;

/// One note for the live synth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveNote {
    /// Seconds from now until it starts.
    pub delay: f64,
    /// How long it sounds, in seconds; `None` holds it until released.
    pub length: Option<f64>,
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
    /// Which sequence playback it belongs to (0 for single notes), so
    /// `Stop` can end just that one.
    pub tag: u64,
}

/// Script -> live synth.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveCommand {
    Play(Vec<LiveNote>),
    /// Release the held (no-length) notes on this channel and key.
    Release { channel: u8, key: u8 },
    /// Cancel one sequence playback: its pending notes, and release its
    /// sounding ones.
    Stop(u64),
    /// Release everything and cancel everything pending.
    StopAll,
}

/// What the schedule tells the synthesizer to do.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SynthOp {
    On { channel: u8, key: u8, velocity: u8 },
    Off { channel: u8, key: u8 },
    AllOff,
}

/// A note waiting to start, at a sample time on the live clock.
struct Pending {
    at: u64,
    length: Option<u64>,
    channel: u8,
    key: u8,
    velocity: u8,
    tag: u64,
}

/// A note sounding now.
struct Sounding {
    channel: u8,
    key: u8,
    velocity: u8,
    tag: u64,
    /// When it ends; `None` = held until released.
    off_at: Option<u64>,
}

/// When notes start and end, on a clock of samples rendered. Kept apart
/// from the synthesizer so it can be tested without a soundfont.
struct Schedule {
    sample_rate: u32,
    clock: u64,
    pending: Vec<Pending>,
    sounding: Vec<Sounding>,
}

impl Schedule {
    fn new(sample_rate: u32) -> Self {
        Self { sample_rate, clock: 0, pending: Vec::new(), sounding: Vec::new() }
    }

    fn apply(&mut self, command: LiveCommand, ops: &mut Vec<SynthOp>) {
        match command {
            LiveCommand::Play(notes) => {
                let samples = |s: f64| (s.max(0.0) * self.sample_rate as f64).round() as u64;
                for note in notes {
                    self.pending.push(Pending {
                        at: self.clock + samples(note.delay),
                        length: note.length.map(|l| samples(l).max(1)),
                        channel: note.channel.min(15),
                        key: note.key.min(127),
                        velocity: note.velocity.clamp(1, 127),
                        tag: note.tag,
                    });
                }
            }
            LiveCommand::Release { channel, key } => {
                self.end_where(|s| s.off_at.is_none() && s.channel == channel && s.key == key, ops);
            }
            LiveCommand::Stop(tag) => {
                self.pending.retain(|p| p.tag != tag);
                self.end_where(|s| s.tag == tag, ops);
            }
            LiveCommand::StopAll => {
                self.pending.clear();
                self.sounding.clear();
                ops.push(SynthOp::AllOff);
            }
        }
    }

    /// Move the clock on by `frames`: end what's over and start what's due
    /// before then (at block granularity, a few milliseconds).
    fn advance(&mut self, frames: u64, ops: &mut Vec<SynthOp>) {
        let end = self.clock + frames;
        self.end_where(|s| s.off_at.is_some_and(|at| at < end), ops);
        let mut due = Vec::new();
        self.pending.retain(|p| {
            let now = p.at < end;
            if now {
                due.push((p.at, p.length, p.channel, p.key, p.velocity, p.tag));
            }
            !now
        });
        due.sort_by_key(|d| d.0);
        for (at, length, channel, key, velocity, tag) in due {
            ops.push(SynthOp::On { channel, key, velocity });
            self.sounding.push(Sounding { channel, key, velocity, tag, off_at: length.map(|l| at + l) });
        }
        self.clock = end;
    }

    /// End the sounding notes matching `which`. A key only gets its
    /// note-off once nothing else is still sounding it, so a note started
    /// again before an earlier one's end isn't cut short by that end.
    fn end_where(&mut self, which: impl Fn(&Sounding) -> bool, ops: &mut Vec<SynthOp>) {
        let mut ended = Vec::new();
        self.sounding.retain(|s| {
            let end = which(s);
            if end {
                ended.push((s.channel, s.key));
            }
            !end
        });
        for (channel, key) in ended {
            if !self.sounding.iter().any(|s| s.channel == channel && s.key == key)
                && !ops.contains(&SynthOp::Off { channel, key })
            {
                ops.push(SynthOp::Off { channel, key });
            }
        }
    }

    fn idle(&self) -> bool {
        self.pending.is_empty() && self.sounding.is_empty()
    }

    /// (channel, key, velocity) of each key sounding, once per key (the
    /// latest note's velocity when it's sounding twice).
    fn sounding_keys(&self) -> Vec<(u8, u8, u8)> {
        let mut keys: Vec<(u8, u8, u8)> = Vec::new();
        for s in &self.sounding {
            match keys.iter_mut().find(|(c, k, _)| *c == s.channel && *k == s.key) {
                Some(entry) => entry.2 = s.velocity,
                None => keys.push((s.channel, s.key, s.velocity)),
            }
        }
        keys
    }
}

pub struct LiveSynth {
    synth: Synthesizer,
    schedule: Schedule,
    /// Samples rendered since anything was sounding or pending.
    quiet: u64,
    ops: Vec<SynthOp>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl LiveSynth {
    pub fn new(soundfont: &Arc<SoundFont>, sample_rate: u32) -> Result<Self> {
        let settings = SynthesizerSettings::new(sample_rate as i32);
        let synth = Synthesizer::new(soundfont, &settings).map_err(|e| anyhow!("{e}"))?;
        Ok(Self {
            synth,
            schedule: Schedule::new(sample_rate),
            quiet: u64::MAX,
            ops: Vec::new(),
            scratch_l: Vec::new(),
            scratch_r: Vec::new(),
        })
    }

    pub fn apply(&mut self, command: LiveCommand) {
        if matches!(command, LiveCommand::Play(_)) {
            self.quiet = 0;
        }
        self.schedule.apply(command, &mut self.ops);
        self.run_ops();
    }

    /// (channel, key, velocity) of the keys the script has sounding now,
    /// for `active_notes()`.
    pub fn sounding_keys(&self) -> Vec<(u8, u8, u8)> {
        self.schedule.sounding_keys()
    }

    /// Whether there's anything to hear: notes sounding or coming, or a
    /// tail still ringing out.
    pub fn busy(&self) -> bool {
        !self.schedule.idle() || self.quiet < (TAIL_SECONDS * self.schedule.sample_rate as f64) as u64
    }

    /// Render the next block into an interleaved stereo buffer, sounding
    /// like `song`'s channels.
    pub fn render(&mut self, song: &Synthesizer, out: &mut [f32]) {
        self.synth.copy_channels_from(song);
        let frames = out.len() / 2;
        self.schedule.advance(frames as u64, &mut self.ops);
        self.run_ops();

        self.scratch_l.resize(frames, 0.0);
        self.scratch_r.resize(frames, 0.0);
        self.synth.render(&mut self.scratch_l, &mut self.scratch_r);
        for i in 0..frames {
            out[2 * i] = self.scratch_l[i];
            out[2 * i + 1] = self.scratch_r[i];
        }

        if self.schedule.idle() && self.synth.active_voice_count() == 0 {
            self.quiet = self.quiet.saturating_add(frames as u64);
        } else {
            self.quiet = 0;
        }
    }

    fn run_ops(&mut self) {
        for op in self.ops.drain(..) {
            match op {
                SynthOp::On { channel, key, velocity } => {
                    self.synth.note_on(channel as i32, key as i32, velocity as i32)
                }
                SynthOp::Off { channel, key } => self.synth.note_off(channel as i32, key as i32),
                SynthOp::AllOff => self.synth.note_off_all(false),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(delay: f64, length: Option<f64>, key: u8, tag: u64) -> LiveNote {
        LiveNote { delay, length, channel: 0, key, velocity: 100, tag }
    }

    fn on(key: u8) -> SynthOp {
        SynthOp::On { channel: 0, key, velocity: 100 }
    }

    fn off(key: u8) -> SynthOp {
        SynthOp::Off { channel: 0, key }
    }

    /// Run `frames` more samples and return what the synth was told.
    fn step(schedule: &mut Schedule, frames: u64) -> Vec<SynthOp> {
        let mut ops = Vec::new();
        schedule.advance(frames, &mut ops);
        ops
    }

    #[test]
    fn notes_start_and_end_on_the_live_clock() {
        let mut schedule = Schedule::new(1000); // 1 sample = 1 ms
        let mut ops = Vec::new();
        schedule.apply(LiveCommand::Play(vec![note(0.0, Some(0.05), 60, 0), note(0.02, None, 64, 7)]), &mut ops);
        assert!(ops.is_empty(), "nothing happens until the clock moves");
        assert_eq!(step(&mut schedule, 10), [on(60)]);
        assert_eq!(step(&mut schedule, 10), []);
        assert_eq!(step(&mut schedule, 10), [on(64)]);
        assert_eq!(schedule.sounding_keys(), [(0, 60, 100), (0, 64, 100)]);
        assert_eq!(step(&mut schedule, 20), []);
        assert_eq!(step(&mut schedule, 10), [off(60)], "50 ms after it started");
        assert_eq!(step(&mut schedule, 1000), [], "a held note stays");
        schedule.apply(LiveCommand::Stop(7), &mut ops);
        assert_eq!(ops, [off(64)]);
        assert!(schedule.idle());
    }

    #[test]
    fn release_only_ends_held_notes_and_stop_all_ends_everything() {
        let mut schedule = Schedule::new(1000);
        let mut ops = Vec::new();
        schedule.apply(LiveCommand::Play(vec![note(0.0, None, 60, 0), note(0.0, Some(1.0), 62, 0)]), &mut ops);
        step(&mut schedule, 10);
        schedule.apply(LiveCommand::Release { channel: 0, key: 60 }, &mut ops);
        schedule.apply(LiveCommand::Release { channel: 0, key: 62 }, &mut ops);
        assert_eq!(ops, [off(60)]);
        ops.clear();
        schedule.apply(LiveCommand::Play(vec![note(0.5, None, 65, 0)]), &mut ops);
        schedule.apply(LiveCommand::StopAll, &mut ops);
        assert_eq!(ops, [SynthOp::AllOff]);
        assert!(schedule.idle(), "the pending note is cancelled too");
    }

    /// End to end with a real soundfont (the first in the app's list):
    /// a note comes out, sounding like the song channel's instrument.
    #[test]
    #[ignore = "needs a soundfont in the app's Soundfonts list"]
    fn the_live_synth_sounds_with_a_real_soundfont() {
        let config = crate::config::Config::load().unwrap();
        let path = config.soundfonts.first().expect("no soundfont configured");
        let soundfont = Arc::new(SoundFont::new(&mut std::fs::File::open(path).unwrap()).unwrap());
        let mut song = Synthesizer::new(&soundfont, &SynthesizerSettings::new(44_100)).unwrap();
        song.process_midi_message(2, 0xC0, 40, 0); // violin on channel 2
        let mut live = LiveSynth::new(&soundfont, 44_100).unwrap();
        let mut block = vec![0.0f32; 512 * 2];
        let loudness = |block: &[f32]| block.iter().map(|s| s * s).sum::<f32>();

        live.render(&song, &mut block);
        assert_eq!(loudness(&block), 0.0, "silent before any note");
        live.apply(LiveCommand::Play(vec![LiveNote { delay: 0.0, length: Some(0.2), channel: 2, key: 60, velocity: 110, tag: 0 }]));
        let mut heard = 0.0;
        for _ in 0..10 {
            live.render(&song, &mut block);
            heard += loudness(&block);
        }
        assert!(heard > 0.0, "the note sounds");
        assert!(live.busy());
    }

    #[test]
    fn a_retriggered_key_isnt_cut_short_by_the_earlier_note_ending() {
        let mut schedule = Schedule::new(1000);
        let mut ops = Vec::new();
        schedule.apply(LiveCommand::Play(vec![note(0.0, Some(0.05), 60, 0), note(0.03, Some(0.05), 60, 0)]), &mut ops);
        assert_eq!(step(&mut schedule, 10), [on(60)]);
        assert_eq!(step(&mut schedule, 30), [on(60)]);
        assert_eq!(step(&mut schedule, 20), [], "the first one ended, but the key is still sounding");
        assert_eq!(step(&mut schedule, 30), [off(60)]);
    }
}
