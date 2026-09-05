//! Scaffolding for a minifb-based audio visualizer window.
//!
//! Implement [`Visualizer`] on your own type — it gets `&mut self`, so it can
//! keep whatever state it wants between frames (rolling history, smoothed
//! levels, a copy of the previous frame for trails, ...) — and drive it with
//! [`run_window`]/[`spawn_window`], or wrap it in a [`VisualizerController`]
//! to open and close the window on demand (e.g. from a hotkey). Either way it
//! needs a [`SampleTap`], which is also wired into the audio path (see
//! `SynthSource::with_tap` in `engine.rs`).
//!
//! Note for portability: minifb requires the window to live on the process's
//! main thread on macOS. This scaffolding spawns it on a background thread,
//! which is fine on Windows and Linux (our target here) but would need
//! restructuring — window on the main thread, TUI on a worker thread — to
//! also run on macOS.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use minifb::{Key, Window, WindowOptions};

/// One stereo sample: `.0` is left, `.1` is right. A plain tuple rather than a
/// named-field struct — same layout as two adjacent `f32`s (no padding, no
/// indirection), so this costs nothing over the raw interleaved form.
pub type StereoFrame = (f32, f32);

/// A bounded, thread-safe feed of `(left, right)` sample pairs: the audio
/// thread [`push`](SampleTap::push)es interleaved chunks as it renders, and
/// once per frame the visualizer [`drain`](SampleTap::drain)s every pair
/// played since the last frame. If nobody drains for a while, the buffer
/// stops growing at `capacity` and quietly drops the oldest frames first,
/// rather than using unbounded memory.
#[derive(Clone)]
pub struct SampleTap {
    inner: Arc<Mutex<VecDeque<StereoFrame>>>,
    capacity: usize,
}

impl SampleTap {
    /// `capacity` is in stereo frames (one `(left, right)` pair each) — e.g.
    /// `sample_rate` caps it at one second.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    /// Called from the audio thread with a newly rendered chunk of
    /// interleaved `(L, R, L, R, ...)` samples; paired up here so producers
    /// don't need to know about the pair representation. An odd trailing
    /// sample (there shouldn't be one) is dropped.
    pub fn push(&self, interleaved: &[f32]) {
        let mut buf = self.inner.lock().unwrap();
        // chunks_exact(2) rather than as_chunks::<2>() to stay on stable Rust.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let pairs = interleaved.chunks_exact(2).map(|pair| (pair[0], pair[1]));
        buf.extend(pairs);
        let excess = buf.len().saturating_sub(self.capacity);
        if excess > 0 {
            buf.drain(..excess);
        }
    }

    /// Called once per rendered frame: takes every `(left, right)` pair
    /// buffered since the last call, oldest first, leaving the tap empty.
    pub fn drain(&self) -> Vec<StereoFrame> {
        let mut buf = self.inner.lock().unwrap();
        buf.drain(..).collect()
    }
}

/// Implement this to draw one frame of a visualizer.
///
/// * `buffer` is `width * height` pixels, row-major from the top-left, in
///   minifb's `0x00RRGGBB` format — write it in full each call (there's no
///   double buffering to inherit stale pixels from).
/// * `samples` are the `(left, right)` pairs rendered since the previous call
///   to `render`, oldest first. Empty when nothing new has played (paused, no
///   soundfont, or the render thread simply beat the audio thread to the
///   punch this tick) — that's a normal frame, not an error.
pub trait Visualizer {
    fn render(&mut self, buffer: &mut [u32], width: usize, height: usize, samples: &[StereoFrame]);
}

/// Open a window and drive `visualizer` against it until it's closed, Escape
/// is pressed, or `running` is flipped to `false` from elsewhere. Blocks the
/// calling thread — use [`spawn_window`] to run it alongside something else,
/// like the TUI's own event loop.
pub fn run_window<V: Visualizer>(
    mut visualizer: V,
    tap: SampleTap,
    width: usize,
    height: usize,
    title: &str,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let mut window = Window::new(title, width, height, WindowOptions::default())
        .context("failed to open visualizer window")?;
    window.set_target_fps(60);

    let mut buffer = vec![0u32; width * height];
    while running.load(Ordering::Relaxed) && window.is_open() && !window.is_key_down(Key::Escape) {
        let samples = tap.drain();
        visualizer.render(&mut buffer, width, height, &samples);
        window.update_with_buffer(&buffer, width, height)?;
    }
    Ok(())
}

/// Run [`run_window`] on its own OS thread so it doesn't block the caller.
/// Returns the thread handle alongside a flag you can clear to ask the window
/// to close (it also closes itself on Escape or the window's own close
/// button — either way, the flag and thread both reflect that once it does).
/// Errors (e.g. no display available) are printed to stderr from that thread
/// rather than propagated, since nothing joins it on the happy path.
pub fn spawn_window<V: Visualizer + Send + 'static>(
    visualizer: V,
    tap: SampleTap,
    width: usize,
    height: usize,
    title: &str,
) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let running = Arc::new(AtomicBool::new(true));
    let running_in_thread = Arc::clone(&running);
    let title = title.to_string();
    let handle = std::thread::spawn(move || {
        if let Err(e) = run_window(visualizer, tap, width, height, &title, running_in_thread) {
            eprintln!("visualizer window error: {e:#}");
        }
    });
    (handle, running)
}

/// Tracks whether a visualizer window is currently open and lets you flip it
/// on or off at runtime — e.g. from a hotkey — reopening with a fresh `V`
/// each time rather than trying to resume old state.
pub struct VisualizerController<V> {
    tap: SampleTap,
    width: usize,
    height: usize,
    title: String,
    handle: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
    _visualizer: PhantomData<fn() -> V>,
}

impl<V: Visualizer + Default + Send + 'static> VisualizerController<V> {
    pub fn new(tap: SampleTap, width: usize, height: usize, title: impl Into<String>) -> Self {
        Self {
            tap,
            width,
            height,
            title: title.into(),
            handle: None,
            _visualizer: PhantomData,
        }
    }

    /// `true` if a window is currently open. Also notices (and forgets) a
    /// window that was closed from the outside — Escape or the close button —
    /// since the last check, so a stale handle can't get stuck "open".
    pub fn is_open(&mut self) -> bool {
        if matches!(&self.handle, Some((handle, _)) if handle.is_finished()) {
            self.handle = None;
        }
        self.handle.is_some()
    }

    /// Open the window if it's closed, close it if it's open.
    pub fn toggle(&mut self) {
        if self.is_open() {
            self.close();
        } else {
            self.open();
        }
    }

    fn open(&mut self) {
        let (handle, running) =
            spawn_window(V::default(), self.tap.clone(), self.width, self.height, &self.title);
        self.handle = Some((handle, running));
    }

    fn close(&mut self) {
        if let Some((handle, running)) = self.handle.take() {
            running.store(false, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}
