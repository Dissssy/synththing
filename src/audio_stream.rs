//! What synththing plays, for the local API's listeners
//! (`GET /v1/audio/stream`): the audio as it goes out to the speakers,
//! before the volume slider, as 16-bit stereo PCM.
//!
//! The output (`audio::Output`) hands every chunk it plays to
//! [`Listeners::send`] while anyone's listening; each listener's connection
//! gets them through its own bounded queue. A listener that falls behind
//! misses chunks rather than holding up the speakers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// Stereo frames the output gathers before sending them (about 23 ms).
pub const CHUNK_FRAMES: usize = 1024;
/// Chunks waiting for a listener before newer ones are dropped (about 3 s).
const QUEUED_CHUNKS: usize = 128;
/// Listeners at once.
pub const MOST_LISTENERS: usize = 8;

/// One chunk of 16-bit little-endian stereo samples.
pub type Chunk = Arc<[u8]>;

#[derive(Default)]
pub struct Listeners {
    /// Anyone's listening: the output only gathers audio while they are.
    listening: AtomicBool,
    queues: Mutex<Vec<SyncSender<Chunk>>>,
}

impl Listeners {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// A new listener's queue, or `None` with `MOST_LISTENERS` listening
    /// already. Dropping it stops the listening.
    pub fn listen(&self) -> Option<Receiver<Chunk>> {
        let mut queues = self.queues.lock().unwrap();
        // (Listeners that went are found by trying them with nothing.)
        queues.retain(|queue| queue.try_send(Arc::from([])).is_ok_or_full());
        if queues.len() >= MOST_LISTENERS {
            return None;
        }
        let (tx, rx) = mpsc::sync_channel(QUEUED_CHUNKS);
        queues.push(tx);
        self.listening.store(true, Ordering::Relaxed);
        Some(rx)
    }

    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    /// From the output, with what it just played (interleaved, -1 to 1).
    /// Never waits: with the list busy (a listener coming or going), the
    /// chunk is skipped.
    pub fn send(&self, interleaved: &[f32]) {
        let Ok(mut queues) = self.queues.try_lock() else {
            return;
        };
        let chunk: Chunk = pcm16(interleaved).into();
        queues.retain(|queue| queue.try_send(Arc::clone(&chunk)).is_ok_or_full());
        if queues.is_empty() {
            self.listening.store(false, Ordering::Relaxed);
        }
    }
}

trait SendOutcome {
    /// Sent, or the listener's behind: it's still there either way.
    fn is_ok_or_full(&self) -> bool;
}

impl SendOutcome for Result<(), TrySendError<Chunk>> {
    fn is_ok_or_full(&self) -> bool {
        !matches!(self, Err(TrySendError::Disconnected(_)))
    }
}

fn pcm16(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
        .collect()
}

/// A WAV header for a stream that doesn't end: 16-bit stereo at
/// `sample_rate`, with the largest lengths there are.
pub fn wav_header(sample_rate: u32) -> [u8; 44] {
    let mut header = [0u8; 44];
    let mut at = 0;
    let mut put = |bytes: &[u8]| {
        header[at..at + bytes.len()].copy_from_slice(bytes);
        at += bytes.len();
    };
    put(b"RIFF");
    put(&u32::MAX.to_le_bytes());
    put(b"WAVEfmt ");
    put(&16u32.to_le_bytes());
    put(&1u16.to_le_bytes()); // PCM
    put(&2u16.to_le_bytes()); // stereo
    put(&sample_rate.to_le_bytes());
    put(&(sample_rate * 4).to_le_bytes()); // bytes a second
    put(&4u16.to_le_bytes()); // bytes a frame
    put(&16u16.to_le_bytes()); // bits a sample
    put(b"data");
    put(&u32::MAX.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listeners_get_what_plays_until_they_go() {
        let listeners = Listeners::new();
        assert!(!listeners.listening());
        let first = listeners.listen().unwrap();
        assert!(listeners.listening());
        listeners.send(&[0.0, 1.0, -1.0, 0.5]);
        assert_eq!(
            &*first.recv().unwrap(),
            &[0, 0, 0xff, 0x7f, 0x01, 0x80, 0xff, 0x3f]
        );
        drop(first);
        listeners.send(&[0.0, 0.0]);
        assert!(!listeners.listening(), "nobody's left");
    }

    #[test]
    fn there_are_only_so_many_listeners() {
        let listeners = Listeners::new();
        let held: Vec<_> = (0..MOST_LISTENERS)
            .map(|_| listeners.listen().unwrap())
            .collect();
        assert!(listeners.listen().is_none());
        drop(held);
        assert!(listeners.listen().is_some(), "gone listeners make room");
    }
}
