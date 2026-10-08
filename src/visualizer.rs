//! Scaffolding for an embedded audio visualizer, drawn as a plain pixel
//! buffer (the same shape a minifb window would want) and blitted into an
//! egui texture each frame, no separate OS window, it lives inside the app.
//!
//! Implement [`Visualizer`] on your own type, it gets `&mut self`, so it can
//! keep whatever state it wants between frames (rolling history, smoothed
//! levels, a copy of the previous frame for trails, ...), and show it with
//! [`VisualizerPanel`]. Either way it needs a [`SampleTap`], which is also
//! wired into the audio path: the render thread in `audio.rs` pushes the
//! song's audio and the live synth's (script notes) into it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::audio::Backlog;

use eframe::egui;

use crate::engine::EngineView;
use crate::gamepad::PadFrame;
use crate::lua_visualizer::Transport;
use crate::script_host::{Effects, FrameDone, FramePipe, FrameRequest};
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
///
/// Notes a script plays (the live synth, `live.rs`) arrive separately,
/// through [`push_live`](SampleTap::push_live). While the song plays they're
/// mixed into its samples one for one, so the visualizer still gets one
/// stream at the real-time rate; while it doesn't, they come through alone.
#[derive(Clone)]
pub struct SampleTap {
    inner: Arc<Mutex<TapBuffers>>,
    capacity: usize,
}

#[derive(Default)]
struct TapBuffers {
    song: VecDeque<StereoFrame>,
    live: VecDeque<StereoFrame>,
    /// Whether the song is being rendered (playing, not paused).
    song_running: bool,
    /// How much of each stream is rendered but not heard yet; that much is
    /// held back, so the visualizer sees audio as it's heard. None (tests,
    /// headless): everything is handed out.
    song_backlog: Option<Backlog>,
    live_backlog: Option<Backlog>,
}

/// `(L, R, L, R, ...)` as pairs. An odd trailing sample (there shouldn't be
/// one) is dropped.
fn pairs(interleaved: &[f32]) -> impl Iterator<Item = StereoFrame> + '_ {
    // chunks_exact(2) rather than as_chunks::<2>() to stay on stable Rust.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    interleaved.chunks_exact(2).map(|pair| (pair[0], pair[1]))
}

/// Drop the oldest frames past `capacity`.
fn cap(buf: &mut VecDeque<StereoFrame>, capacity: usize) {
    let excess = buf.len().saturating_sub(capacity);
    if excess > 0 {
        buf.drain(..excess);
    }
}

impl SampleTap {
    /// `capacity` is in stereo frames (one `(left, right)` pair each), e.g.
    /// `sample_rate` caps it at one second.
    pub fn new(capacity: usize) -> Self {
        Self { inner: Arc::default(), capacity }
    }

    /// Called from the audio thread with a newly rendered chunk of the
    /// song, interleaved `(L, R, L, R, ...)`.
    pub fn push(&self, interleaved: &[f32]) {
        let mut buf = self.inner.lock().unwrap();
        buf.song.extend(pairs(interleaved));
        cap(&mut buf.song, self.capacity);
    }

    /// The same for the live synth (notes the script plays).
    pub fn push_live(&self, interleaved: &[f32]) {
        let mut buf = self.inner.lock().unwrap();
        buf.live.extend(pairs(interleaved));
        cap(&mut buf.live, self.capacity);
    }

    /// Hand out only audio that's been heard: the newest frames, as many as
    /// each backlog says are still waiting to play, stay in the tap.
    pub fn hold_back_unheard(&self, song: Backlog, live: Backlog) {
        let mut buf = self.inner.lock().unwrap();
        buf.song_backlog = Some(song);
        buf.live_backlog = Some(live);
    }

    /// Whether the song is being rendered: while it is, live audio waits to
    /// be mixed into its samples; while it isn't, live audio is drained alone.
    pub fn set_song_running(&self, running: bool) {
        self.inner.lock().unwrap().song_running = running;
    }

    /// Called once per rendered frame: takes every `(left, right)` pair
    /// buffered since the last call, oldest first, with the live synth's
    /// mixed in, leaving the tap empty (bar live audio still waiting for
    /// song samples to mix into).
    pub fn drain(&self) -> Vec<StereoFrame> {
        let mut guard = self.inner.lock().unwrap();
        let buf = &mut *guard;
        let heard = |queue: &VecDeque<StereoFrame>, backlog: &Option<Backlog>| {
            queue.len().saturating_sub(backlog.as_ref().map_or(0, Backlog::unheard_frames))
        };
        let song_heard = heard(&buf.song, &buf.song_backlog);
        let live_heard = heard(&buf.live, &buf.live_backlog);
        let mut frames: Vec<StereoFrame> = buf.song.drain(..song_heard).collect();
        let mixed = frames.len().min(live_heard);
        for (frame, (l, r)) in frames.iter_mut().zip(buf.live.drain(..mixed)) {
            frame.0 += l;
            frame.1 += r;
        }
        if !buf.song_running {
            frames.extend(buf.live.drain(..live_heard - mixed));
        }
        frames
    }

    /// How many frames `drain` would hand out right now.
    pub fn ready_frames(&self) -> usize {
        let buf = self.inner.lock().unwrap();
        let heard = |queue: &VecDeque<StereoFrame>, backlog: &Option<Backlog>| {
            queue.len().saturating_sub(backlog.as_ref().map_or(0, Backlog::unheard_frames))
        };
        let song = heard(&buf.song, &buf.song_backlog);
        if buf.song_running { song } else { song.max(heard(&buf.live, &buf.live_backlog)) }
    }

    /// Throw away the song's buffered audio (a seek, a pause, a new song).
    /// Live audio is kept: the live synth carries on regardless.
    pub fn clear_song(&self) {
        self.inner.lock().unwrap().song.clear();
    }

    /// Drop the newest `frames` song frames (rendered again differently).
    pub fn drop_newest_song(&self, frames: usize) {
        let mut buf = self.inner.lock().unwrap();
        let keep = buf.song.len().saturating_sub(frames);
        buf.song.truncate(keep);
    }
}

/// A MIDI note the score currently holds down.
#[derive(Clone, Copy, Debug)]
pub struct ActiveNote {
    pub channel: u8,
    pub key: u8,
    pub velocity: u8,
    /// Played by the visualizer script (`play_note`, `note_on`,
    /// sequences) rather than the song.
    pub from_script: bool,
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
pub const RESERVED_KEYS: [egui::Key; 5] =
    [egui::Key::Escape, egui::Key::F5, egui::Key::F9, egui::Key::F10, egui::Key::F11];

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
    /// Game controllers (`gamepad.rs`): buttons and stick directions held,
    /// pressed and released, the analogue axes, and who's connected.
    pub pads: PadFrame,
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
#[derive(Clone, Copy, Debug)]
pub struct ShowOutput {
    pub focused: bool,
    pub cursor: CursorRequest,
    /// The script drew a new frame this time (not frozen, not a frame
    /// skipped at a reduced frame rate).
    pub rendered: bool,
    /// Where the picture is on screen (smaller than the panel when a fixed
    /// size is letterboxed into it).
    pub image_rect: egui::Rect,
}

impl Default for ShowOutput {
    fn default() -> Self {
        Self { focused: false, cursor: CursorRequest::default(), rendered: false, image_rect: egui::Rect::NOTHING }
    }
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
        self.pads.down = later.pads.down;
        self.pads.pressed.extend(later.pads.pressed);
        self.pads.released.extend(later.pads.released);
        self.pads.axes = later.pads.axes;
        self.pads.connected = later.pads.connected;
    }
}

/// Upper bound on how many pixels the visualizer actually renders, no matter
/// how large the panel showing it is. Every bundled script does real
/// per-pixel work (filling rects, running the alpha blender, scanning the
/// quantized-run palette, ...), so buffer size directly sets render cost:
/// fine at a windowed panel's few hundred thousand pixels, not at a
/// fullscreen 1440p+ one's several million. Above this budget the logical
/// buffer shrinks (same aspect ratio) and gets scaled back up to fill the
/// panel with nearest-neighbor filtering, trading a bit of crispness for
/// render cost that stays flat regardless of window or monitor size.
const MAX_VISUALIZER_PIXELS: f32 = 1280.0 * 720.0;

/// How long the app waits for a frame it just asked the script for. Most
/// take a few milliseconds and are shown at once, as if drawn right here;
/// one that takes longer is shown when it's done, and the last frame stays
/// up meanwhile, so a slow script never holds up the window.
const FRAME_WAIT: Duration = Duration::from_millis(50);

/// Shows the frames the script (on its own thread, `ScriptHost`) draws,
/// as one `ui.image(...)` each time [`show`](Self::show) is called, and
/// hands it each frame's audio, notes and input. The frames can be smaller
/// than the panel showing them, see [`MAX_VISUALIZER_PIXELS`]. It holds the
/// script's frame side ([`FramePipe`]); the app keeps the control side, so
/// the panel can be shared by the windows that show it.
pub struct VisualizerPanel {
    frames: FramePipe,
    /// The last frame the script drew (`0x00RRGGBB`), `width` by `height`.
    pixels: Vec<u32>,
    /// An old frame's buffer, for the next frame to draw into.
    spare: Vec<u32>,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    texture: Option<egui::TextureHandle>,
    /// Take keyboard focus the next time it's drawn (entering the dedicated
    /// fullscreen).
    focus_requested: bool,
    /// Input since the last frame the script was handed (it was busy, or
    /// running at a reduced frame rate), saved up for the next one.
    pending_input: Option<VisualizerInput>,
    /// The audio the last drawn frame was given (for recording).
    last_samples: Vec<StereoFrame>,
    /// This frame's controller input, from the app (`set_pads`).
    pads: PadFrame,
    /// What the script asked for in the frames drawn since `take_effects`.
    effects: Effects,
}

impl VisualizerPanel {
    pub fn new(frames: FramePipe, width: usize, height: usize) -> Self {
        Self {
            frames,
            pixels: vec![0; width * height],
            spare: Vec::new(),
            rgba: vec![0; width * height * 4],
            width,
            height,
            texture: None,
            focus_requested: false,
            pending_input: None,
            last_samples: Vec::new(),
            pads: PadFrame::default(),
            effects: Effects::default(),
        }
    }

    /// This frame's controller input, for the next `show`.
    pub fn set_pads(&mut self, pads: PadFrame) {
        self.pads = pads;
    }

    /// For recording: the audio the last drawn frame was given, and its
    /// pixels (`0x00RRGGBB`). A recorder may swap the pixel buffer out for
    /// another of the same size; the next frame redraws it in full anyway.
    pub fn recording_parts(&mut self) -> (&[StereoFrame], &mut Vec<u32>) {
        (&self.last_samples, &mut self.pixels)
    }

    /// Give the visualizer keyboard focus the next time it's shown.
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// What the script asked the app to do (mute channels, play notes,
    /// seek, ...) in the frames drawn since the last call.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut self.effects)
    }

    /// Hand the script this frame's audio (whatever's been pushed to `tap`
    /// since the last frame it was handed), notes, playback and input, and
    /// draw its latest frame into `ui`, filling whatever space `ui`
    /// currently has, the frames growing or shrinking to match (e.g. as the
    /// panel is resized or the editor is toggled beside it). With
    /// `fixed_size` (recording), frames are exactly that size instead,
    /// shown as large as fits with black bars around them. `hold`: don't
    /// ask for a frame this time (a recording asks exactly when a video
    /// frame is due), and `timestep` is the one it's asked for with.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tap: &SampleTap,
        notes: &NotesSnapshot,
        playback: &EngineView,
        transport: Transport,
        frozen: bool,
        mode: DisplayMode,
        fixed_size: Option<(usize, usize)>,
        hold: bool,
        timestep: Option<f64>,
    ) -> ShowOutput {
        let available = ui.available_size();
        let (panel_rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let rect = match fixed_size {
            Some((w, h)) => {
                ui.painter().rect_filled(panel_rect, 0.0, egui::Color32::BLACK);
                letterbox(panel_rect, w as f32 / h.max(1) as f32)
            }
            None => panel_rect,
        };

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
            return ShowOutput { focused, cursor: CursorRequest::default(), rendered: false, image_rect: rect };
        }

        let (buf_w, buf_h) = fixed_size.unwrap_or_else(|| logical_buffer_size(available));
        let mut input = gather_input(&ctx, &response, rect, (buf_w, buf_h), focused, mode);
        input.pads = std::mem::take(&mut self.pads);
        let pointer_over = input.pointer.is_some();
        match &mut self.pending_input {
            Some(pending) => pending.absorb(input),
            None => self.pending_input = Some(input),
        }
        self.frames.set_repaint_context(&ctx);

        // A frame the script finished in the background since last time;
        // else, unless it's still busy, held for a recording or running at
        // a reduced frame rate, ask for the next one (the audio waits in
        // the tap meanwhile).
        let mut done = self.frames.take_frame(Duration::ZERO);
        if done.is_none() && !hold && self.frames.ready_for_frame() {
            self.frames.request_frame(FrameRequest {
                width: buf_w,
                height: buf_h,
                pixels: std::mem::take(&mut self.spare),
                samples: tap.drain(),
                notes: notes.clone(),
                playback: playback.clone(),
                input: self.pending_input.take().unwrap_or_default(),
                transport,
                timestep,
            });
            done = self.frames.take_frame(FRAME_WAIT);
        }
        let rendered = done.is_some();
        if let Some(done) = done {
            self.accept(done, &ctx, rect);
        }
        if let Some(texture) = &self.texture {
            // Stretched to the whole allocated rect: the texture can be
            // smaller than the panel by design (see MAX_VISUALIZER_PIXELS).
            ui.painter().image(texture.id(), rect, uv, egui::Color32::WHITE);
        }

        let cursor = self.frames.cursor();
        if cursor.hidden && pointer_over {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        ShowOutput { focused, cursor, rendered, image_rect: rect }
    }

    /// Take a frame the script drew: its pixels into the texture, its
    /// audio for recording, and what it asked for into `effects`.
    fn accept(&mut self, done: FrameDone, ctx: &egui::Context, rect: egui::Rect) {
        if (done.width, done.height) != (self.width, self.height) {
            self.width = done.width;
            self.height = done.height;
            self.rgba = vec![0; done.width * done.height * 4];
            // Dropped rather than resized in place: simplest way to be sure
            // the next `load_texture` call picks up the new dimensions.
            self.texture = None;
        }
        self.spare = std::mem::replace(&mut self.pixels, done.pixels);
        self.last_samples = done.samples;
        let effects = done.effects;
        self.effects.channel_requests.extend(effects.channel_requests);
        self.effects.live_commands.extend(effects.live_commands);
        self.effects.playback_requests.extend(effects.playback_requests);

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
        // blurry, and it's a no-op cost-wise when buffer == panel size. A
        // buffer bigger than its spot on screen (recording at a fixed size
        // into a small panel) is shrunk smoothly instead.
        let on_screen = rect.width() * ctx.pixels_per_point();
        let options =
            if self.width as f32 > on_screen { egui::TextureOptions::LINEAR } else { egui::TextureOptions::NEAREST };
        match &mut self.texture {
            Some(texture) => texture.set(image, options),
            None => self.texture = Some(ctx.load_texture("visualizer", image, options)),
        }
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
    // Over the picture itself, not the black bars around a letterboxed one.
    let hovered = response.contains_pointer() && ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p));
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
/// The largest rect of aspect ratio `aspect` (width / height) that fits in
/// `outer`, centered.
fn letterbox(outer: egui::Rect, aspect: f32) -> egui::Rect {
    let (w, h) = (outer.width(), outer.height());
    let size = if w / h.max(1.0) > aspect { egui::vec2(h * aspect, h) } else { egui::vec2(w, w / aspect) };
    egui::Rect::from_center_size(outer.center(), size)
}

fn logical_buffer_size(available: egui::Vec2) -> (usize, usize) {
    let w = available.x.max(1.0);
    let h = available.y.max(1.0);
    let area = w * h;
    let scale = if area > MAX_VISUALIZER_PIXELS { (MAX_VISUALIZER_PIXELS / area).sqrt() } else { 1.0 };
    (((w * scale) as usize).max(1), ((h * scale) as usize).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_audio_mixes_into_the_song_one_for_one() {
        let tap = SampleTap::new(100);
        tap.set_song_running(true);
        tap.push(&[0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);
        tap.push_live(&[1.0, 2.0, 1.0, 2.0, 1.0, 2.0, 1.0, 2.0]);
        // Three song frames out, each with a live frame added; the fourth
        // live frame waits for the next song frame.
        let frames = tap.drain();
        assert_eq!(frames.len(), 3);
        assert!((frames[2].0 - 1.3).abs() < 1e-6 && (frames[2].1 - 2.3).abs() < 1e-6);
        tap.push(&[0.5, 0.5]);
        assert_eq!(tap.drain(), [(1.5, 2.5)]);
        assert!(tap.drain().is_empty());
    }

    #[test]
    fn live_audio_comes_through_alone_while_the_song_is_stopped() {
        let tap = SampleTap::new(100);
        tap.push_live(&[0.25, 0.5, 0.25, 0.5]);
        assert_eq!(tap.drain(), [(0.25, 0.5), (0.25, 0.5)]);
        tap.push_live(&[0.25, 0.5]);
        tap.clear_song();
        assert_eq!(tap.drain(), [(0.25, 0.5)], "a seek doesn't throw away live audio");
    }
}
