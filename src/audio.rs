//! Decoupled audio: synthesis runs on its own thread, rendering ahead into a
//! ring buffer, so the audio callback never blocks on it and a dense section
//! that momentarily renders slower than real time just eats into the cushion
//! instead of underrunning.
//!
//! * [`AudioEngine::run`] is the render-thread loop. It owns the [`Engine`],
//!   applies [`AudioCommand`]s from the GUI, keeps the [`AudioRing`] filled to
//!   `buffer_ms` of audio, and publishes playback state into [`PlaybackShared`].
//! * [`SynthSource`] is what rodio pulls from, a pure ring-buffer consumer.
//! * The visualizer tap is fed with the *pre-volume* signal, so the volume
//!   slider changes what you hear without changing what the visualizer sees.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustysynth::{MidiFile, SoundFont};

use crate::engine::{DecodedAudio, Engine, EngineView, Snapshot};
use crate::live::LiveCommand;
use crate::visualizer::{NotesSnapshot, SampleTap};

/// How many stereo frames the consumer pulls from the ring at a time.
const CONSUMER_CHUNK_FRAMES: usize = 1024;
/// How many stereo frames the render thread synthesizes per block.
const RENDER_BLOCK_FRAMES: usize = 512;
/// Allowed range for the user's buffer slider, in milliseconds.
pub const MIN_BUFFER_MS: u32 = 40;
pub const MAX_BUFFER_MS: u32 = 500;
pub const DEFAULT_BUFFER_MS: u32 = 150;
/// The live synth's (script notes') block size and cushion: small, so a
/// note is heard soon after the script plays it.
const LIVE_BLOCK_FRAMES: usize = 128;
const LIVE_BUFFER_MS: u32 = 25;

/// GUI -> render thread. Everything the GUI used to call on `Engine` directly.
pub enum AudioCommand {
    TogglePause,
    Seek(f64),
    SeekRelative(f64),
    SetSpeed(f64),
    NudgeSpeed(f64),
    /// The song, its name, and whether it starts paused (a script asked
    /// for that with `script_options`).
    LoadMidi(Arc<MidiFile>, String, bool),
    LoadAudioFile(Arc<DecodedAudio>, String, bool),
    SetSoundFont(Arc<SoundFont>, String),
    SetChannelEnabled(u8, bool),
    EnableAllChannels,
    SoloChannel(u8),
    SetLoopEnabled(bool),
    /// Output gain, 0.0..=1.0. Does not affect the visualizer tap.
    SetVolume(f32),
    /// Target cushion in milliseconds.
    SetBufferMs(u32),
    /// Notes a script plays (`live.rs`).
    Live(LiveCommand),
}

/// Playback as of a point in the rendered audio (`at`, in song frames
/// rendered), for publishing it once that point is heard.
struct Moment {
    at: u64,
    position: f64,
    generation: u64,
    paused: bool,
    finished: bool,
    notes: NotesSnapshot,
}

/// More than enough moments for the largest buffer (one per render block).
const MAX_MOMENTS: usize = 256;

/// Render thread -> GUI. Cloned once per GUI frame.
#[derive(Clone, Default)]
pub struct PlaybackShared {
    pub view: EngineView,
    pub notes: NotesSnapshot,
}

/// A bounded FIFO of interleaved stereo samples shared between the render
/// thread (producer) and the audio callback (consumer). The consumer never
/// blocks: a lock it can't take immediately just yields silence for that
/// sample.
#[derive(Clone)]
pub struct AudioRing {
    inner: Arc<Mutex<VecDeque<f32>>>,
    capacity: usize,
    backlog: Backlog,
}

/// How much rendered audio hasn't been heard yet: what's queued in an
/// [`AudioRing`] plus what its consumer has taken but not played. Readable
/// from any thread without locking, so the visualizer can hold back what
/// hasn't played (see `SampleTap`).
#[derive(Clone, Default)]
pub struct Backlog {
    /// Samples in the ring (updated under its lock).
    queued: Arc<AtomicUsize>,
    /// Samples the consumer holds locally.
    local: Arc<AtomicUsize>,
}

impl Backlog {
    /// Stereo frames rendered but not yet played.
    pub fn unheard_frames(&self) -> usize {
        (self.queued.load(Ordering::Relaxed) + self.local.load(Ordering::Relaxed)) / 2
    }
}

impl AudioRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
            backlog: Backlog::default(),
        }
    }

    pub fn backlog(&self) -> Backlog {
        self.backlog.clone()
    }

    fn len(&self) -> usize {
        self.backlog.queued.load(Ordering::Relaxed)
    }

    pub(crate) fn push(&self, block: &[f32]) {
        let mut queue = self.inner.lock().unwrap();
        queue.extend(block.iter().copied());
        self.backlog.queued.store(queue.len(), Ordering::Relaxed);
    }

    /// Consumer side: move up to `out.capacity()` samples into `out`, returning
    /// how many. Returns 0 rather than blocking if the lock is contended.
    fn pop_into(&self, out: &mut Vec<f32>) -> usize {
        out.clear();
        let Ok(mut queue) = self.inner.try_lock() else {
            return 0;
        };
        let take = queue.len().min(out.capacity());
        out.extend(queue.drain(..take));
        self.backlog.queued.store(queue.len(), Ordering::Relaxed);
        take
    }

    /// Render side: drop everything queued, and tell the consumer (through
    /// `flush`) to drop what it holds too. Done under the lock, so audio
    /// rendered right after (from the new position) is kept.
    pub(crate) fn flush(&self, flush: &AtomicBool) {
        let mut queue = self.inner.lock().unwrap();
        queue.clear();
        self.backlog.queued.store(0, Ordering::Relaxed);
        flush.store(true, Ordering::Relaxed);
    }
}

/// The rodio `Source`: pulls from the ring, emits silence when it's empty or
/// when a flush has just discarded stale audio (seek / pause / load).
pub struct SynthSource {
    ring: AudioRing,
    flush: Arc<AtomicBool>,
    sample_rate: u32,
    buf: Vec<f32>,
    pos: usize,
}

impl SynthSource {
    pub fn new(ring: AudioRing, flush: Arc<AtomicBool>, sample_rate: u32) -> Self {
        Self {
            ring,
            flush,
            sample_rate,
            buf: Vec::with_capacity(CONSUMER_CHUNK_FRAMES * 2),
            pos: 0,
        }
    }
}

impl Iterator for SynthSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        // A flush: the render thread already emptied the ring, drop what we
        // took from it before.
        if self.flush.swap(false, Ordering::Relaxed) {
            self.buf.clear();
            self.pos = 0;
        }
        if self.pos >= self.buf.len() {
            let got = self.ring.pop_into(&mut self.buf);
            self.pos = 0;
            if got == 0 {
                self.ring.backlog.local.store(0, Ordering::Relaxed);
                return Some(0.0); // underrun / idle -> silence
            }
        }
        let sample = self.buf[self.pos];
        self.pos += 1;
        self.ring.backlog.local.store(self.buf.len() - self.pos, Ordering::Relaxed);
        Some(sample)
    }
}

impl rodio::Source for SynthSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        std::num::NonZeroU16::new(2).expect("2 is nonzero")
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        std::num::NonZeroU32::new(self.sample_rate).expect("sample rate is nonzero")
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

pub struct AudioEngine {
    engine: Engine,
    tap: SampleTap,
    ring: AudioRing,
    /// The live synth's ring, mixed with `ring` by the output.
    live_ring: AudioRing,
    flush: Arc<AtomicBool>,
    commands: Receiver<AudioCommand>,
    shared: Arc<Mutex<PlaybackShared>>,
    sample_rate: u32,
    volume: f32,
    buffer_ms: u32,
    block: Vec<f32>,
    live_block: Vec<f32>,
    /// Song frames rendered since the engine started, and what playback
    /// looked like after each block, so `publish` can report the moment
    /// being heard rather than the one just rendered.
    rendered: u64,
    history: VecDeque<Moment>,
    /// The engine as of the start of each rendered block still waiting to
    /// be heard (and the one being heard), keyed by `rendered` then: what
    /// a channel toggle rewinds to.
    snapshots: VecDeque<(u64, Snapshot)>,
    /// Mirrors the engine: don't feed the ring while paused/finished or with
    /// nothing loaded.
    should_render: bool,
    last_publish: Instant,
}

impl AudioEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: Engine,
        tap: SampleTap,
        ring: AudioRing,
        live_ring: AudioRing,
        flush: Arc<AtomicBool>,
        commands: Receiver<AudioCommand>,
        shared: Arc<Mutex<PlaybackShared>>,
        sample_rate: u32,
    ) -> Self {
        Self {
            engine,
            tap,
            ring,
            live_ring,
            flush,
            commands,
            shared,
            sample_rate,
            volume: 1.0,
            buffer_ms: DEFAULT_BUFFER_MS,
            block: vec![0.0; RENDER_BLOCK_FRAMES * 2],
            live_block: vec![0.0; LIVE_BLOCK_FRAMES * 2],
            rendered: 0,
            history: VecDeque::new(),
            snapshots: VecDeque::new(),
            should_render: false,
            last_publish: Instant::now(),
        }
    }

    pub fn run(mut self) {
        loop {
            match self.commands.recv_timeout(Duration::from_millis(4)) {
                Ok(command) => self.apply(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break, // GUI gone
            }
            while let Ok(command) = self.commands.try_recv() {
                self.apply(command);
            }

            self.refresh_flags();
            self.fill_live_ring();
            self.fill_ring();
            self.fill_live_ring();

            if self.last_publish.elapsed() >= Duration::from_millis(16) {
                self.publish();
                self.last_publish = Instant::now();
            }
        }
    }

    fn apply(&mut self, command: AudioCommand) {
        match command {
            AudioCommand::TogglePause => {
                self.engine.toggle_pause();
                self.flush_now();
            }
            AudioCommand::Seek(target) => {
                self.engine.seek(target);
                self.flush_now();
            }
            AudioCommand::SeekRelative(delta) => {
                self.engine.seek_relative(delta);
                self.flush_now();
            }
            AudioCommand::SetSpeed(speed) => self.engine.set_speed(speed),
            AudioCommand::NudgeSpeed(delta) => self.engine.nudge_speed(delta),
            AudioCommand::LoadMidi(midi, name, paused) => {
                self.engine.load_midi(midi, name, paused);
                self.flush_now();
            }
            AudioCommand::LoadAudioFile(data, name, paused) => {
                self.engine.load_audio_file(data, name, paused);
                self.flush_now();
            }
            AudioCommand::SetSoundFont(soundfont, name) => {
                // The SF2 was already validated on the GUI side; a settings
                // error here can't happen at our fixed sample rate.
                let _ = self.engine.set_soundfont(soundfont, name);
                self.flush_now();
            }
            AudioCommand::SetChannelEnabled(channel, enabled) => {
                self.rerender(|engine| engine.set_channel_enabled(channel, enabled));
            }
            AudioCommand::EnableAllChannels => self.rerender(Engine::enable_all_channels),
            AudioCommand::SoloChannel(channel) => self.rerender(|engine| engine.solo_channel(channel)),
            AudioCommand::SetLoopEnabled(enabled) => self.engine.set_loop_enabled(enabled),
            AudioCommand::SetVolume(volume) => self.volume = volume.clamp(0.0, 1.0),
            AudioCommand::SetBufferMs(ms) => {
                self.buffer_ms = ms.clamp(MIN_BUFFER_MS, MAX_BUFFER_MS);
            }
            AudioCommand::Live(command) => self.engine.live_command(command),
        }
    }

    /// Discard already-rendered audio (both the ring and whatever the consumer
    /// has locally buffered) plus queued visualizer samples, so a seek / pause
    /// / load takes effect immediately regardless of buffer size.
    fn flush_now(&mut self) {
        self.ring.flush(&self.flush);
        self.tap.clear_song();
        self.snapshots.clear();
        // Nothing rendered is waiting to be heard any more: what's heard
        // next is the engine as it is now.
        self.history.clear();
        self.remember();
    }

    /// Note what playback looks like as of everything rendered so far.
    fn remember(&mut self) {
        let view = self.engine.view();
        self.history.push_back(Moment {
            at: self.rendered,
            position: view.position,
            generation: view.generation,
            paused: view.paused,
            finished: view.finished,
            notes: self.engine.notes_snapshot(),
        });
        while self.history.len() > MAX_MOMENTS {
            self.history.pop_front();
        }
    }

    fn refresh_flags(&mut self) {
        let view = self.engine.view();
        let has_content = view.has_audio_file || (view.has_midi && view.has_soundfont);
        self.should_render = !view.paused && !view.finished && has_content;
        self.tap.set_song_running(self.should_render);
    }

    fn fill_ring(&mut self) {
        if !self.should_render {
            return;
        }
        let target_samples = self.buffer_frames() * 2;
        loop {
            let occupied = self.ring.len();
            if occupied >= target_samples || occupied + self.block.len() > self.ring.capacity {
                break;
            }
            self.render_block(0);
        }
    }

    /// Render the next block: keep a snapshot of the engine from before it
    /// (for `rerender`), then queue it for the speakers and the visualizer,
    /// all but its first `skip` frames (already heard: see `rerender`).
    fn render_block(&mut self, skip: usize) {
        if let Some(snapshot) = self.engine.snapshot() {
            self.snapshots.push_back((self.rendered, snapshot));
            self.prune_snapshots();
        }
        self.engine.render_into(&mut self.block);
        self.rendered += (self.block.len() / 2) as u64;
        self.remember();
        let from = (skip * 2).min(self.block.len());
        let fresh = &mut self.block[from..];
        self.tap.push(fresh); // pre-volume, for the visualizer
        if (self.volume - 1.0).abs() > f32::EPSILON {
            for sample in fresh.iter_mut() {
                *sample = (*sample * self.volume).clamp(-1.0, 1.0);
            }
        }
        self.ring.push(fresh);
    }

    /// Only snapshots someone could still rewind to: the one at or before
    /// what's being heard, and later ones.
    fn prune_snapshots(&mut self) {
        let heard = self.rendered.saturating_sub(self.ring.backlog().unheard_frames() as u64);
        while self.snapshots.len() > 1 && self.snapshots[1].0 <= heard {
            self.snapshots.pop_front();
        }
    }

    /// Make a change to the song's playback (a channel muted or unmuted)
    /// heard right away, without skipping: go back to the engine as it was
    /// at the moment being heard, make the change there, and render the
    /// audio that was waiting in the buffer again. Without a MIDI song
    /// (nothing to rewind), the change just applies from here on.
    fn rerender(&mut self, change: impl FnOnce(&mut Engine)) {
        let unheard = self.ring.backlog().unheard_frames() as u64;
        let heard = self.rendered.saturating_sub(unheard);
        let start = self.snapshots.iter().rposition(|(at, _)| *at <= heard);
        let Some(start) = start.filter(|_| unheard > 0 && self.should_render) else {
            change(&mut self.engine);
            return;
        };
        let (at, snapshot) = self.snapshots[start].clone();
        let until = self.rendered;

        // Out with what was waiting: the speakers', the visualizer's, and
        // what was noted about it.
        self.ring.flush(&self.flush);
        self.tap.drop_newest_song(unheard as usize);
        self.snapshots.truncate(start);
        while self.history.back().is_some_and(|m| m.at > at) {
            self.history.pop_back();
        }

        self.engine.restore(snapshot);
        change(&mut self.engine);
        self.rendered = at;
        // Render up to where it had got to, the already heard part of the
        // first block left out.
        let mut skip = (heard - at) as usize;
        while self.rendered < until {
            self.render_block(skip);
            skip = skip.saturating_sub(self.block.len() / 2);
        }
    }

    /// Keep the live synth's small cushion topped up, paused or not. It
    /// stops once its notes (and their tails) are over, and the output just
    /// mixes in silence.
    fn fill_live_ring(&mut self) {
        let target_samples = (self.sample_rate as u64 * LIVE_BUFFER_MS as u64 / 1000) as usize * 2;
        while self.engine.live_busy() && self.live_ring.len() < target_samples {
            self.engine.render_live(&mut self.live_block);
            self.tap.push_live(&self.live_block); // pre-volume, like the song's
            for sample in &mut self.live_block {
                *sample = (*sample * self.volume).clamp(-1.0, 1.0);
            }
            self.live_ring.push(&self.live_block);
        }
    }

    fn buffer_frames(&self) -> usize {
        (self.sample_rate as u64 * self.buffer_ms as u64 / 1000) as usize
    }

    /// Publish playback as it's being heard: the position, the score's
    /// notes and the paused/finished state from the latest block that has
    /// actually played (the ring holds `buffer_ms` of audio not yet heard),
    /// everything else (speed, loop, names, channels) as it is now. Notes a
    /// script plays come from the live synth as they are, being heard within
    /// a few tens of milliseconds.
    fn publish(&mut self) {
        let heard = self.rendered.saturating_sub(self.ring.backlog().unheard_frames() as u64);
        while self.history.len() > 1 && self.history[1].at <= heard {
            self.history.pop_front();
        }
        let mut view = self.engine.view();
        let mut notes = self.engine.notes_snapshot();
        if let Some(moment) = self.history.front() {
            view.position = moment.position;
            view.generation = moment.generation;
            view.paused = moment.paused;
            view.finished = moment.finished;
            notes.active = moment.notes.active.clone();
            notes.upcoming = moment.notes.upcoming.clone();
        }
        notes.active.extend(self.engine.live_notes());
        let mut shared = self.shared.lock().unwrap();
        shared.view = view;
        shared.notes = notes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A muted channel is heard muted from the moment of the click, with
    /// nothing skipped: what comes out is exactly the song played straight
    /// through up to that moment, then muted from there (as the synth does
    /// it, from the start of the block being heard).
    #[test]
    #[ignore = "needs a soundfont in the app's list and a MIDI in Downloads/Music"]
    fn muting_a_channel_doesnt_skip() {
        let config = crate::config::Config::load().unwrap();
        let sf_path = config.soundfonts.first().expect("no soundfont");
        let soundfont = Arc::new(SoundFont::new(&mut std::fs::File::open(sf_path).unwrap()).unwrap());
        let music = directories::UserDirs::new().unwrap().home_dir().join("Downloads/Music/145343_1.mid");
        let midi = crate::loader::load_midi(&music).unwrap().file;
        let rate = 44_100;
        let engine = |muted_at: Option<u64>| {
            let mut e = Engine::new(rate);
            e.set_soundfont(Arc::clone(&soundfont), "sf".into()).unwrap();
            e.load_midi(Arc::clone(&midi), "song".into(), false);
            (e, muted_at)
        };
        // Reference renders, block by block like the render thread.
        let render_ref = |(mut e, muted_at): (Engine, Option<u64>), frames: u64| {
            let mut out = Vec::new();
            let mut block = vec![0.0f32; RENDER_BLOCK_FRAMES * 2];
            let mut done = 0u64;
            while done < frames {
                if muted_at == Some(done) {
                    e.set_channel_enabled(1, false);
                }
                e.render_into(&mut block);
                out.extend_from_slice(&block);
                done += RENDER_BLOCK_FRAMES as u64;
            }
            out
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let ring = AudioRing::new(rate as usize * 4);
        let mut audio = AudioEngine::new(
            engine(None).0,
            SampleTap::new(rate as usize),
            ring.clone(),
            AudioRing::new(1024),
            Arc::new(AtomicBool::new(false)),
            rx,
            Arc::default(),
            rate,
        );
        drop(tx);
        // Skip into the song a few seconds, then fill the buffer.
        for _ in 0..300 {
            audio.refresh_flags();
            audio.render_block(0);
            let mut sink = Vec::with_capacity(RENDER_BLOCK_FRAMES * 2);
            ring.pop_into(&mut sink);
        }
        let mut heard = Vec::new();
        let start = audio.rendered as usize * 2;
        audio.fill_ring();
        assert!(ring.len() > RENDER_BLOCK_FRAMES * 4, "the buffer should hold several blocks");
        // The speakers play 3.4 blocks' worth, then the click.
        let mut taken = Vec::with_capacity(RENDER_BLOCK_FRAMES * 7 / 2 * 2);
        ring.pop_into(&mut taken);
        let played = (RENDER_BLOCK_FRAMES * 34 / 10) * 2;
        heard.extend_from_slice(&taken[..played]);
        // (The rest of `taken` is "held by the output, unplayed".)
        ring.backlog.local.store(taken.len() - played, Ordering::Relaxed);
        let at_click = audio.rendered - ring.backlog().unheard_frames() as u64;
        audio.apply(AudioCommand::SetChannelEnabled(1, false));
        let until = audio.rendered;
        let mut rest = Vec::with_capacity(ring.len());
        ring.pop_into(&mut rest);
        heard.extend_from_slice(&rest);

        // The block the click landed in starts here; the synth mutes from it.
        let block_start = at_click - at_click % RENDER_BLOCK_FRAMES as u64;
        let straight = render_ref(engine(None), until);
        let muted = render_ref(engine(Some(block_start)), until);
        let click = at_click as usize * 2;
        let mut expected = straight[start..click].to_vec();
        expected.extend_from_slice(&muted[click..until as usize * 2]);
        assert_eq!(heard.len(), expected.len(), "nothing skipped or repeated");
        assert!(heard == expected, "heard exactly the song, muted from the click");
        assert_ne!(straight[click..], muted[click..], "and the mute did change the sound");
    }

    #[test]
    fn the_visualizer_only_gets_audio_that_has_played() {
        let ring = AudioRing::new(64);
        let tap = SampleTap::new(64);
        tap.hold_back_unheard(ring.backlog(), AudioRing::new(64).backlog());
        tap.set_song_running(true);

        // Four frames rendered: in the ring and the tap, none heard yet.
        let block = [0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4];
        ring.push(&block);
        tap.push(&block);
        assert!(tap.drain().is_empty());

        // The output takes two frames and plays them.
        let mut out = Vec::with_capacity(4);
        assert_eq!(ring.pop_into(&mut out), 4);
        assert_eq!(tap.drain(), [(0.1, 0.1), (0.2, 0.2)]);

        // A flush (seek): nothing is waiting to be heard any more.
        ring.flush(&AtomicBool::new(false));
        tap.clear_song();
        ring.push(&[0.9, 0.9]);
        tap.push(&[0.9, 0.9]);
        assert!(tap.drain().is_empty());
        ring.pop_into(&mut out);
        assert_eq!(tap.drain(), [(0.9, 0.9)]);
    }
}
