//! Scaffolding for an embedded audio visualizer, drawn as a plain pixel
//! buffer (the same shape a minifb window would want) and blitted into an
//! egui texture each frame — no separate OS window, it lives inside the app.
//!
//! Implement [`Visualizer`] on your own type — it gets `&mut self`, so it can
//! keep whatever state it wants between frames (rolling history, smoothed
//! levels, a copy of the previous frame for trails, ...) — and show it with
//! [`VisualizerPanel`]. Either way it needs a [`SampleTap`], which is also
//! wired into the audio path (see `SynthSource::with_tap` in `engine.rs`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::engine::EngineView;

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

/// A MIDI note the score currently holds down.
#[derive(Clone, Copy, Debug)]
pub struct ActiveNote {
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
}

/// An upcoming note-on or note-off within the look-ahead window.
#[derive(Clone, Copy, Debug)]
pub struct NoteChange {
    pub channel: u8,
    pub key: u8,
    /// Velocity of a note-on; `0` for a note-off.
    pub velocity: u8,
    /// `true` for a note-on, `false` for a note-off.
    pub on: bool,
    /// Seconds from the current playback position until this change happens.
    pub seconds_until: f32,
}

/// Per-frame data the visualizer needs beyond the audio samples: what notes
/// are sounding now, which change soon, and which MIDI channels exist and are
/// currently enabled. Built by `Engine::notes_snapshot`.
#[derive(Clone, Default)]
pub struct NotesSnapshot {
    pub active: Vec<ActiveNote>,
    pub upcoming: Vec<NoteChange>,
    /// Channels (0-15) the current MIDI file actually uses, sorted.
    pub detected_channels: Vec<u8>,
    /// Per-channel enable flag (index = channel). A disabled channel still
    /// plays; it's a hint for scripts to filter it visually.
    pub enabled_channels: [bool; 16],
}

/// Implement this to draw one frame of a visualizer.
///
/// * `buffer` is `width * height` pixels, row-major from the top-left, in
///   `0x00RRGGBB` format — write it in full each call (there's no double
///   buffering to inherit stale pixels from).
/// * `samples` are the `(left, right)` pairs rendered since the previous call
///   to `render`, oldest first. Empty when nothing new has played (paused, no
///   soundfont, or the render thread simply beat the audio thread to the
///   punch this tick) — that's a normal frame, not an error.
/// * `notes` is which MIDI notes are held right now plus the note-on/off
///   changes coming up within the engine's look-ahead window.
/// * `playback` is transport state (position, length, paused, ...) — the same
///   thing the GUI's own transport bar reads.
pub trait Visualizer {
    fn render(
        &mut self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        samples: &[StereoFrame],
        notes: &NotesSnapshot,
        playback: &EngineView,
    );
}

/// Upper bound on how many pixels the visualizer actually renders, no matter
/// how large the panel showing it is. Every bundled script does real
/// per-pixel work (filling rects, running the alpha blender, scanning the
/// quantized-run palette, ...), so buffer size directly sets render cost —
/// fine at a windowed panel's few hundred thousand pixels, not at a
/// fullscreen 1440p+ one's several million. Above this budget the logical
/// buffer shrinks (same aspect ratio) and gets scaled back up to fill the
/// panel with nearest-neighbor filtering, trading a bit of crispness for
/// render cost that stays flat regardless of window or monitor size.
const MAX_VISUALIZER_PIXELS: f32 = 1280.0 * 720.0;

/// Owns a fixed-size pixel buffer, a `V`, and the egui texture that mirrors
/// them, and draws it all as one `ui.image(...)` each time [`show`](Self::show)
/// is called. The buffer's own resolution can be smaller than the panel it's
/// displayed in — see [`MAX_VISUALIZER_PIXELS`].
pub struct VisualizerPanel<V> {
    visualizer: V,
    pixels: Vec<u32>,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    texture: Option<egui::TextureHandle>,
}

impl<V: Visualizer> VisualizerPanel<V> {
    pub fn new(visualizer: V, width: usize, height: usize) -> Self {
        Self {
            visualizer,
            pixels: vec![0; width * height],
            rgba: vec![0; width * height * 4],
            width,
            height,
            texture: None,
        }
    }

    pub fn visualizer(&self) -> &V {
        &self.visualizer
    }

    pub fn visualizer_mut(&mut self) -> &mut V {
        &mut self.visualizer
    }

    /// Render one frame from whatever's been pushed to `tap` since the last
    /// call, and draw it into `ui` — filling whatever space `ui` currently
    /// has available, growing or shrinking the pixel buffer to match (e.g. as
    /// the visualizer panel is resized or the editor is toggled beside it).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tap: &SampleTap,
        notes: &NotesSnapshot,
        playback: &EngineView,
    ) {
        let available = ui.available_size();
        let (buf_w, buf_h) = logical_buffer_size(available);
        self.resize(buf_w, buf_h);

        let samples = tap.drain();
        self.visualizer.render(
            &mut self.pixels,
            self.width,
            self.height,
            &samples,
            notes,
            playback,
        );

        // chunks_exact_mut(4) rather than as_chunks_mut::<4>() to stay on stable Rust.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let rgba_pixels = self.rgba.chunks_exact_mut(4);
        for (pixel, rgba) in self.pixels.iter().zip(rgba_pixels) {
            rgba[0] = (pixel >> 16) as u8; // R
            rgba[1] = (pixel >> 8) as u8; // G
            rgba[2] = *pixel as u8; // B
            rgba[3] = 255; // A
        }

        let image = egui::ColorImage::from_rgba_unmultiplied([self.width, self.height], &self.rgba);
        // Nearest, not linear: when the buffer is downscaled from the panel
        // size, this is what keeps the upscale crisp/pixel-art rather than
        // blurry, and it's a no-op cost-wise when buffer == panel size.
        let options = egui::TextureOptions::NEAREST;
        match &mut self.texture {
            Some(texture) => texture.set(image, options),
            None => self.texture = Some(ui.ctx().load_texture("visualizer", image, options)),
        }

        if let Some(texture) = &self.texture {
            // Explicit target size rather than the `ui.image(...)` shorthand
            // (which draws at the texture's own native size) — the texture
            // can be smaller than `available` by design, and still needs to
            // fill it.
            ui.add(egui::Image::new((texture.id(), texture.size_vec2())).fit_to_exact_size(available));
        }
    }

    fn resize(&mut self, width: usize, height: usize) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width * height];
        self.rgba = vec![0; width * height * 4];
        // Dropped rather than resized in place: simplest way to be sure the
        // next `load_texture` call picks up the new dimensions.
        self.texture = None;
    }
}

/// `available` scaled down (preserving aspect ratio) so its area fits within
/// [`MAX_VISUALIZER_PIXELS`] — a no-op (1:1 with the panel) for ordinary
/// windowed sizes, which is already comfortably under budget.
fn logical_buffer_size(available: egui::Vec2) -> (usize, usize) {
    let w = available.x.max(1.0);
    let h = available.y.max(1.0);
    let area = w * h;
    let scale = if area > MAX_VISUALIZER_PIXELS { (MAX_VISUALIZER_PIXELS / area).sqrt() } else { 1.0 };
    (((w * scale) as usize).max(1), ((h * scale) as usize).max(1))
}
