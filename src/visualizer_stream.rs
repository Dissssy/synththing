//! The visualizer's frames for the local API (`GET /v1/visualizer/...`):
//! a stream of JPEG pictures that OBS, a browser or a media player can show.
//!
//! The app offers each frame the visualizer draws (`Frames::offer`) while
//! anyone's watching; a worker thread encodes it, and every watcher's
//! connection sends the newest picture, at most as often as it asked for.
//! A frame that comes while the worker's still busy with the last one is
//! skipped, so a slow encode only lowers the stream's frame rate.

use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How good the pictures are (0 to 100).
const QUALITY: u8 = 85;

/// The newest picture, and who's watching.
pub struct Frames {
    state: Mutex<State>,
    /// Woken when a new picture is in.
    changed: Condvar,
    /// To the encoder: (width, height, `0x00RRGGBB` pixels).
    encoder: SyncSender<(usize, usize, Vec<u32>)>,
}

#[derive(Default)]
struct State {
    /// The newest picture, and how many came before it.
    jpeg: Option<Arc<Vec<u8>>>,
    count: u64,
    /// Each watcher's size, if it asked for one, by when it came.
    watchers: Vec<(u64, Option<(usize, usize)>)>,
    next_watcher: u64,
}

/// Someone watching, for as long as it's kept.
pub struct Watching {
    frames: Arc<Frames>,
    id: u64,
}

impl Drop for Watching {
    fn drop(&mut self) {
        self.frames.lock().watchers.retain(|(id, _)| *id != self.id);
    }
}

impl Frames {
    pub fn new() -> Arc<Self> {
        let (encoder, frames) = mpsc::sync_channel::<(usize, usize, Vec<u32>)>(1);
        let this = Arc::new(Self { state: Mutex::default(), changed: Condvar::new(), encoder });
        let weak = Arc::downgrade(&this);
        let spawned = std::thread::Builder::new().name("visualizer stream".into()).spawn(move || {
            for (width, height, pixels) in frames {
                let Some(this) = weak.upgrade() else { return };
                match encode(width, height, &pixels) {
                    Ok(jpeg) => {
                        let mut state = this.lock();
                        state.jpeg = Some(Arc::new(jpeg));
                        state.count += 1;
                        this.changed.notify_all();
                    }
                    Err(e) => log::warn!("visualizer stream: couldn't encode a frame: {e}"),
                }
            }
        });
        if spawned.is_err() {
            log::warn!("visualizer stream: couldn't start its encoder");
        }
        this
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Start watching, at `size` if given (the newest watcher's size wins).
    pub fn watch(self: &Arc<Self>, size: Option<(usize, usize)>) -> Watching {
        let mut state = self.lock();
        let id = state.next_watcher;
        state.next_watcher += 1;
        state.watchers.push((id, size));
        Watching { frames: Arc::clone(self), id }
    }

    /// Whether anyone's watching, and the size the newest one that asked
    /// for a size wants frames drawn at.
    pub fn wanted(&self) -> Option<Option<(usize, usize)>> {
        let state = self.lock();
        (!state.watchers.is_empty()).then(|| state.watchers.iter().rev().find_map(|(_, size)| *size))
    }

    /// A frame the visualizer drew, for the watchers (skipped while the
    /// last one's still being encoded, or if no one's watching).
    pub fn offer(&self, width: usize, height: usize, pixels: &[u32]) {
        if self.lock().watchers.is_empty() {
            return;
        }
        match self.encoder.try_send((width, height, pixels.to_vec())) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => log::warn!("visualizer stream: its encoder stopped"),
        }
    }

    /// How many pictures have come so far: a watcher starting now waits for
    /// the next one (the newest may be from before it, at another size).
    pub fn count(&self) -> u64 {
        self.lock().count
    }

    /// The first picture newer than the `seen`th (`None` within `wait`):
    /// it, and how many have come so far.
    pub fn next(&self, seen: u64, wait: Duration) -> Option<(Arc<Vec<u8>>, u64)> {
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, wait, |s| s.count <= seen || s.jpeg.is_none())
            .unwrap_or_else(|p| p.into_inner());
        state.jpeg.clone().filter(|_| state.count > seen).map(|jpeg| (jpeg, state.count))
    }
}

fn encode(width: usize, height: usize, pixels: &[u32]) -> image::ImageResult<Vec<u8>> {
    let rgb: Vec<u8> = pixels.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, QUALITY);
    image::ImageEncoder::write_image(encoder, &rgb, width as u32, height as u32, image::ExtendedColorType::Rgb8)?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchers_get_each_new_frame_and_the_newest_size() {
        let frames = Frames::new();
        assert_eq!(frames.wanted(), None);
        assert_eq!(frames.count(), 0);
        let first = frames.watch(None);
        assert_eq!(frames.wanted(), Some(None));
        let sized = frames.watch(Some((64, 36)));
        assert_eq!(frames.wanted(), Some(Some((64, 36))));
        drop(sized);
        assert_eq!(frames.wanted(), Some(None));

        frames.offer(4, 2, &[0x00ff0000; 8]);
        let (jpeg, count) = frames.next(0, Duration::from_secs(5)).expect("a frame");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8], "a JPEG");
        assert!(frames.next(count, Duration::from_millis(50)).is_none(), "nothing newer yet");
        drop(first);
        assert_eq!(frames.wanted(), None);
    }
}
