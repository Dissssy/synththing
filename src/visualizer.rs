//! Scaffolding for an embedded audio visualizer, drawn as a plain pixel
//! buffer (the same shape a minifb window would want) and blitted into an
//! egui texture each frame, no separate OS window, it lives inside the app.
//!
//! Implement [`Visualizer`] on your own type, it gets `&mut self`, so it can
//! keep whatever state it wants between frames (rolling history, smoothed
//! levels, a copy of the previous frame for trails, ...), and show it with
//! [`VisualizerPanel`]. Either way it needs a [`SampleTap`], which is also
//! wired into the audio path (see `SynthSource::with_tap` in `engine.rs`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::engine::EngineView;
use crate::typing::TextEvent;

/// One stereo sample: `.0` is left, `.1` is right. A plain tuple rather than a
/// named-field struct, same layout as two adjacent `f32`s (no padding, no
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
    /// `capacity` is in stereo frames (one `(left, right)` pair each), e.g.
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

/// Where the visualizer is being shown, so a script can adapt (e.g. only
/// take over the cursor in the dedicated fullscreen).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisplayMode {
    /// A tab in the normal window.
    #[default]
    Window,
    /// A tab, with the whole app fullscreen.
    Fullscreen,
    /// The dedicated visualizer fullscreen: nothing else on screen.
    Dedicated,
}

impl DisplayMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Fullscreen => "fullscreen",
            Self::Dedicated => "dedicated",
        }
    }
}

/// Keys the app always keeps for itself, never reported to a script:
/// Escape (back to windowed), F5 (restart the script), F11 (toggle the
/// dedicated visualizer fullscreen).
pub const RESERVED_KEYS: [egui::Key; 3] = [egui::Key::Escape, egui::Key::F5, egui::Key::F11];

/// Mouse buttons a script can ask about, in `VisualizerInput`'s array order.
pub const MOUSE_BUTTONS: [egui::PointerButton; 3] =
    [egui::PointerButton::Primary, egui::PointerButton::Secondary, egui::PointerButton::Middle];

/// One frame of pointer and keyboard input, as a visualizer script sees it.
/// Mouse state only while the pointer is over the visualizer (or it has
/// focus, so a drag that leaves it still ends); keys only while it has
/// focus (clicked into, or in the dedicated fullscreen). `RESERVED_KEYS`
/// are never reported.
#[derive(Clone, Debug, Default)]
pub struct VisualizerInput {
    pub mode: DisplayMode,
    pub focused: bool,
    /// Pointer position in buffer pixels; `None` when it isn't over the
    /// visualizer.
    pub pointer: Option<(f32, f32)>,
    /// Pointer movement since last frame, in buffer pixels.
    pub pointer_delta: (f32, f32),
    /// Scroll since last frame, in points (positive y is scrolling up).
    pub scroll: (f32, f32),
    pub buttons_down: [bool; 3],
    pub buttons_pressed: [bool; 3],
    pub buttons_released: [bool; 3],
    pub keys_down: Vec<egui::Key>,
    pub keys_pressed: Vec<egui::Key>,
    pub keys_released: Vec<egui::Key>,
    /// Typing this frame, in order: characters, pastes, and key presses
    /// (with key repeat and modifiers), for typing spans and text_typed().
    pub text_events: Vec<TextEvent>,
}

impl VisualizerInput {
    /// The characters typed this frame (not pastes or keys).
    pub fn typed(&self) -> String {
        self.text_events
            .iter()
            .filter_map(|e| match e {
                TextEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// What a script asked for, cursor-wise. Hiding applies while the pointer
/// is over the visualizer; confining only in the dedicated fullscreen, while
/// it has focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorRequest {
    pub hidden: bool,
    pub confined: bool,
}

/// What `VisualizerPanel::show` reports back to the app.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShowOutput {
    pub focused: bool,
    pub cursor: CursorRequest,
}

/// Implement this to draw one frame of a visualizer.
///
/// * `buffer` is `width * height` pixels, row-major from the top-left, in
///   `0x00RRGGBB` format, write it in full each call (there's no double
///   buffering to inherit stale pixels from).
/// * `samples` are the `(left, right)` pairs rendered since the previous call
///   to `render`, oldest first. Empty when nothing new has played (paused, no
///   soundfont, or the render thread simply beat the audio thread to the
///   punch this tick), that's a normal frame, not an error.
/// * `notes` is which MIDI notes are held right now plus the note-on/off
///   changes coming up within the engine's look-ahead window.
/// * `playback` is transport state (position, length, paused, ...), the same
///   thing the GUI's own transport bar reads.
/// * `input` is this frame's mouse/keyboard state over the visualizer.
#[allow(clippy::too_many_arguments)]
pub trait Visualizer {
    fn render(
        &mut self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        samples: &[StereoFrame],
        notes: &NotesSnapshot,
        playback: &EngineView,
        input: &VisualizerInput,
    );

    /// What the visualizer wants done with the system cursor.
    fn cursor_request(&self) -> CursorRequest {
        CursorRequest::default()
    }

    /// Whether to render this frame. A visualizer running at a reduced
    /// frame rate says no in between; the panel keeps showing the last
    /// frame and saves up the audio and input for the next one.
    fn ready_for_frame(&self) -> bool {
        true
    }
}

impl VisualizerInput {
    /// Fold a later frame's input into this one (for a frame that wasn't
    /// rendered): presses, releases, movement and scrolling add up, the
    /// current state (position, held buttons and keys) is the later one's.
    fn absorb(&mut self, later: VisualizerInput) {
        self.mode = later.mode;
        self.focused = later.focused;
        self.pointer = later.pointer;
        self.pointer_delta = (self.pointer_delta.0 + later.pointer_delta.0, self.pointer_delta.1 + later.pointer_delta.1);
        self.scroll = (self.scroll.0 + later.scroll.0, self.scroll.1 + later.scroll.1);
        self.buttons_down = later.buttons_down;
        for i in 0..3 {
            self.buttons_pressed[i] |= later.buttons_pressed[i];
            self.buttons_released[i] |= later.buttons_released[i];
        }
        self.keys_down = later.keys_down;
        self.keys_pressed.extend(later.keys_pressed);
        self.keys_released.extend(later.keys_released);
        self.text_events.extend(later.text_events);
    }
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
/// displayed in, see [`MAX_VISUALIZER_PIXELS`].
pub struct VisualizerPanel<V> {
    visualizer: V,
    pixels: Vec<u32>,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    texture: Option<egui::TextureHandle>,
    /// Take keyboard focus the next time it's drawn (entering the dedicated
    /// fullscreen).
    focus_requested: bool,
    /// Input from frames that weren't rendered (reduced frame rate), saved
    /// up for the next one that is.
    pending_input: Option<VisualizerInput>,
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
            focus_requested: false,
            pending_input: None,
        }
    }

    /// Give the visualizer keyboard focus the next time it's shown.
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    pub fn visualizer(&self) -> &V {
        &self.visualizer
    }

    pub fn visualizer_mut(&mut self) -> &mut V {
        &mut self.visualizer
    }

    /// Render one frame from whatever's been pushed to `tap` since the last
    /// call, and draw it into `ui`, filling whatever space `ui` currently
    /// has available, growing or shrinking the pixel buffer to match (e.g. as
    /// the visualizer panel is resized or the editor is toggled beside it).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tap: &SampleTap,
        notes: &NotesSnapshot,
        playback: &EngineView,
        frozen: bool,
        mode: DisplayMode,
    ) -> ShowOutput {
        let available = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();

        // Keyboard focus: clicking in takes it, clicking elsewhere or Escape
        // (egui's own default) gives it up. While focused, Tab and the arrow
        // keys go to the script instead of moving focus to another widget.
        let pressed_anywhere = ctx.input(|i| i.pointer.any_pressed());
        if self.focus_requested || (pressed_anywhere && response.contains_pointer()) {
            response.request_focus();
            self.focus_requested = false;
        } else if pressed_anywhere && response.has_focus() && !response.contains_pointer() {
            response.surrender_focus();
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false },
                );
            });
        }

        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        // Frozen: show the last frame as-is, without running the script.
        if frozen && let Some(texture) = &self.texture {
            ui.painter().image(texture.id(), rect, uv, egui::Color32::WHITE);
            return ShowOutput { focused, cursor: CursorRequest::default() };
        }

        let (buf_w, buf_h) = logical_buffer_size(available);
        self.resize(buf_w, buf_h);
        let mut input = gather_input(&ctx, &response, rect, (self.width, self.height), focused, mode);

        // Reduced frame rate: show the last frame again, and keep this
        // frame's input (the audio waits in the tap) for the next render.
        if !self.visualizer.ready_for_frame()
            && let Some(texture) = &self.texture
        {
            match &mut self.pending_input {
                Some(pending) => pending.absorb(input),
                None => self.pending_input = Some(input),
            }
            ui.painter().image(texture.id(), rect, uv, egui::Color32::WHITE);
            return ShowOutput { focused, cursor: self.visualizer.cursor_request() };
        }
        if let Some(mut pending) = self.pending_input.take() {
            pending.absorb(input);
            input = pending;
        }

        let samples = tap.drain();
        self.visualizer.render(
            &mut self.pixels,
            self.width,
            self.height,
            &samples,
            notes,
            playback,
            &input,
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
            None => self.texture = Some(ctx.load_texture("visualizer", image, options)),
        }
        if let Some(texture) = &self.texture {
            // Stretched to the whole allocated rect: the texture can be
            // smaller than the panel by design (see MAX_VISUALIZER_PIXELS).
            ui.painter().image(texture.id(), rect, uv, egui::Color32::WHITE);
        }

        let cursor = self.visualizer.cursor_request();
        if cursor.hidden && input.pointer.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        ShowOutput { focused, cursor }
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

/// This frame's input over the visualizer at `rect`, converted to buffer
/// pixels (`buffer` is the buffer's size).
fn gather_input(
    ctx: &egui::Context,
    response: &egui::Response,
    rect: egui::Rect,
    buffer: (usize, usize),
    focused: bool,
    mode: DisplayMode,
) -> VisualizerInput {
    let scale_x = buffer.0 as f32 / rect.width().max(1.0);
    let scale_y = buffer.1 as f32 / rect.height().max(1.0);
    let hovered = response.contains_pointer();
    ctx.input(|i| {
        let mut input = VisualizerInput { mode, focused, ..Default::default() };
        if hovered && let Some(pos) = i.pointer.hover_pos() {
            input.pointer = Some(((pos.x - rect.min.x) * scale_x, (pos.y - rect.min.y) * scale_y));
            input.scroll = (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y);
        }
        if hovered || focused {
            let delta = i.pointer.delta();
            input.pointer_delta = (delta.x * scale_x, delta.y * scale_y);
            for (n, &button) in MOUSE_BUTTONS.iter().enumerate() {
                input.buttons_down[n] = i.pointer.button_down(button);
                input.buttons_pressed[n] = i.pointer.button_pressed(button);
                input.buttons_released[n] = i.pointer.button_released(button);
            }
        }
        if focused {
            input.keys_down = i.keys_down.iter().copied().filter(|k| !RESERVED_KEYS.contains(k)).collect();
            for event in &i.events {
                match event {
                    egui::Event::Key { key, pressed, repeat, modifiers, .. } if !RESERVED_KEYS.contains(key) => {
                        if *pressed && !*repeat {
                            input.keys_pressed.push(*key);
                        } else if !*pressed {
                            input.keys_released.push(*key);
                        }
                        if *pressed {
                            input.text_events.push(TextEvent::Key(*key, *modifiers));
                        }
                    }
                    egui::Event::Text(text) => input.text_events.push(TextEvent::Text(text.clone())),
                    egui::Event::Paste(text) => input.text_events.push(TextEvent::Paste(text.clone())),
                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                        input.text_events.push(TextEvent::Text(text.clone()));
                    }
                    _ => {}
                }
            }
        }
        input
    })
}

/// `available` scaled down (preserving aspect ratio) so its area fits within
/// [`MAX_VISUALIZER_PIXELS`], a no-op (1:1 with the panel) for ordinary
/// windowed sizes, which is already comfortably under budget.
fn logical_buffer_size(available: egui::Vec2) -> (usize, usize) {
    let w = available.x.max(1.0);
    let h = available.y.max(1.0);
    let area = w * h;
    let scale = if area > MAX_VISUALIZER_PIXELS { (MAX_VISUALIZER_PIXELS / area).sqrt() } else { 1.0 };
    (((w * scale) as usize).max(1), ((h * scale) as usize).max(1))
}
