//! The audio device's output, open only while there's something to hear.
//!
//! An open output keeps the device's audio path running and calls back for
//! samples hundreds of times a second, silence or not. On a phone that's a
//! wake lock Android puts down to the app and a CPU that never sleeps, so
//! after [`QUIET_FOR`] with nothing to play the output is closed, and it's
//! opened again (on the device that's then the default) as soon as there
//! is: a song or a script's notes rendered and waiting in their rings, the
//! wordmark's hop, or the API's audio stream listened to.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::{AudioRing, Backlog, Output, Volume};
use crate::audio_stream::Listeners;

/// How long the output stays open with nothing to play.
const QUIET_FOR: Duration = Duration::from_secs(10);

/// How long after failing to open the output it's tried again.
const RETRY_AFTER: Duration = Duration::from_secs(5);

pub struct AudioOutput {
    /// What the output plays, made again for each opening.
    song: AudioRing,
    flush: Arc<AtomicBool>,
    live: AudioRing,
    volume: Volume,
    listeners: Arc<Listeners>,
    sample_rate: u32,
    /// What's rendered and not yet heard.
    song_backlog: Backlog,
    live_backlog: Backlog,
    /// The open output; `None` while closed.
    sink: Option<rodio::MixerDeviceSink>,
    /// When there was last something to play.
    last_heard: Instant,
    /// When opening last failed, if it did (logged once, not every try).
    failed_at: Option<Instant>,
}

impl AudioOutput {
    pub fn new(song: AudioRing, flush: Arc<AtomicBool>, live: AudioRing, volume: Volume, listeners: Arc<Listeners>, sample_rate: u32) -> Self {
        Self {
            song_backlog: song.backlog(),
            live_backlog: live.backlog(),
            song,
            flush,
            live,
            volume,
            listeners,
            sample_rate,
            sink: None,
            last_heard: Instant::now(),
            failed_at: None,
        }
    }

    /// Open the output if it isn't: its mixer, for a sound of its own.
    pub fn mixer(&mut self) -> Option<&rodio::mixer::Mixer> {
        self.last_heard = Instant::now();
        if self.sink.is_none() && self.failed_at.is_none_or(|at| at.elapsed() >= RETRY_AFTER) {
            self.sink = self.open();
        }
        self.sink.as_ref().map(|sink| sink.mixer())
    }

    /// Open the output when there's something to play, and close it after
    /// [`QUIET_FOR`] without. Called every tick.
    pub fn tick(&mut self) {
        let wanted = self.song_backlog.unheard_frames() > 0 || self.live_backlog.unheard_frames() > 0 || self.listeners.listening();
        if wanted {
            self.mixer();
        } else if self.sink.is_some() && self.last_heard.elapsed() >= QUIET_FOR {
            log::debug!("nothing to play: closing the audio output");
            self.sink = None;
        }
    }

    fn open(&mut self) -> Option<rodio::MixerDeviceSink> {
        match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                // (Both rings are empty whenever it's closed: a new reader
                // misses nothing.)
                sink.mixer().add(Output::new(
                    self.song.clone(),
                    Arc::clone(&self.flush),
                    self.live.clone(),
                    self.volume.clone(),
                    Arc::clone(&self.listeners),
                    self.sample_rate,
                ));
                self.failed_at = None;
                log::debug!("opened the audio output");
                Some(sink)
            }
            Err(e) => {
                if self.failed_at.is_none() {
                    log::error!("couldn't open an audio output device: {e}");
                }
                self.failed_at = Some(Instant::now());
                None
            }
        }
    }
}
