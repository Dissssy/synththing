//! A plain audio file (wav/mp3/ogg/flac/...) decoded into memory, filled in
//! while it plays. The loader decodes the first chunk, hands the song over
//! so it can start right away, and keeps decoding the rest on a thread of
//! its own (see `loader::load_audio_file`). Seeking and speed are then just
//! arithmetic on an index, no re-decoding needed.
//!
//! Frames are kept in fixed-size chunks that never change once added, so
//! the render thread can hold on to them without locking: it only takes
//! the lock to pick up chunks added since it last looked.

use std::sync::{Arc, Condvar, Mutex};

use crate::visualizer::StereoFrame;

/// Frames per chunk (about 0.7 s at 44.1 kHz): how much is decoded before
/// a song can start, and how often the render thread picks up more.
pub const CHUNK_FRAMES: usize = 1 << 15;

pub struct DecodedAudio {
    pub sample_rate: u32,
    /// See `loader::song_id`.
    pub song_id: String,
    /// The length the file's header gives, in frames, if it gives one:
    /// the song's length until decoding finishes.
    expected_frames: Option<usize>,
    frames: Mutex<Frames>,
    finished: Condvar,
}

#[derive(Default)]
struct Frames {
    /// All `CHUNK_FRAMES` long but the last.
    chunks: Vec<Arc<[StereoFrame]>>,
    len: usize,
    complete: bool,
}

impl DecodedAudio {
    /// An empty file still being decoded: fill it with `push`, then `finish`.
    pub fn new(sample_rate: u32, song_id: String, expected_frames: Option<usize>) -> Self {
        Self { sample_rate, song_id, expected_frames, frames: Mutex::default(), finished: Condvar::new() }
    }

    /// About how much memory the frames decoded so far take.
    pub fn memory(&self) -> usize {
        self.frames.lock().map_or(0, |f| f.len) * std::mem::size_of::<StereoFrame>()
    }

    /// A file decoded all at once.
    pub fn complete(samples: Vec<StereoFrame>, sample_rate: u32, song_id: String) -> Self {
        let audio = Self::new(sample_rate, song_id, Some(samples.len()));
        for chunk in samples.chunks(CHUNK_FRAMES) {
            audio.push(chunk.into());
        }
        audio.finish();
        audio
    }

    /// Add the next chunk. Only the last one may be shorter than
    /// `CHUNK_FRAMES`.
    pub fn push(&self, chunk: Arc<[StereoFrame]>) {
        let Ok(mut frames) = self.frames.lock() else { return };
        debug_assert!(frames.chunks.last().is_none_or(|c| c.len() == CHUNK_FRAMES));
        frames.len += chunk.len();
        frames.chunks.push(chunk);
    }

    /// Everything's decoded.
    pub fn finish(&self) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.complete = true;
        }
        self.finished.notify_all();
    }

    /// Block until everything's decoded (for rendering offline, where
    /// waiting is fine and playing silence isn't).
    pub fn wait_complete(&self) {
        let Ok(frames) = self.frames.lock() else { return };
        drop(self.finished.wait_while(frames, |f| !f.complete));
    }
}

/// The render thread's view of a [`DecodedAudio`]: the chunks it's picked
/// up so far.
pub struct FrameReader {
    pub data: Arc<DecodedAudio>,
    chunks: Vec<Arc<[StereoFrame]>>,
    len: usize,
    complete: bool,
}

impl FrameReader {
    pub fn new(data: Arc<DecodedAudio>) -> Self {
        let mut reader = Self { data, chunks: Vec::new(), len: 0, complete: false };
        reader.refresh();
        reader
    }

    /// Pick up whatever's been decoded since the last look.
    pub fn refresh(&mut self) {
        if self.complete {
            return;
        }
        let Ok(frames) = self.data.frames.lock() else { return };
        self.chunks.extend(frames.chunks[self.chunks.len()..].iter().cloned());
        self.len = frames.len;
        self.complete = frames.complete;
    }

    pub fn get(&self, index: usize) -> Option<StereoFrame> {
        self.chunks.get(index / CHUNK_FRAMES)?.get(index % CHUNK_FRAMES).copied()
    }

    /// Frames available so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether `len` is the whole file.
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// The whole song's length in frames: what's decoded once that's all
    /// of it, before then the header's length when there is one.
    pub fn total_frames(&self) -> usize {
        match self.data.expected_frames {
            Some(expected) if !self.complete => expected.max(self.len),
            _ => self.len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_sees_chunks_as_they_arrive() {
        let audio = Arc::new(DecodedAudio::new(48_000, String::new(), Some(CHUNK_FRAMES + 10)));
        audio.push(vec![(1.0, 1.0); CHUNK_FRAMES].into());
        let mut reader = FrameReader::new(Arc::clone(&audio));
        assert_eq!(reader.len(), CHUNK_FRAMES);
        assert_eq!(reader.total_frames(), CHUNK_FRAMES + 10);
        assert!(reader.get(CHUNK_FRAMES).is_none());

        audio.push(vec![(2.0, 2.0); 5].into());
        audio.finish();
        reader.refresh();
        assert!(reader.complete());
        assert_eq!(reader.get(CHUNK_FRAMES + 4), Some((2.0, 2.0)));
        // Done decoding: the real length wins over the header's.
        assert_eq!(reader.total_frames(), CHUNK_FRAMES + 5);
    }

    #[test]
    fn complete_splits_into_chunks() {
        let audio = Arc::new(DecodedAudio::complete(vec![(0.5, 0.5); CHUNK_FRAMES * 2 + 1], 44_100, String::new()));
        audio.wait_complete();
        let reader = FrameReader::new(audio);
        assert!(reader.complete());
        assert_eq!(reader.len(), CHUNK_FRAMES * 2 + 1);
        assert_eq!(reader.get(CHUNK_FRAMES * 2), Some((0.5, 0.5)));
    }
}
