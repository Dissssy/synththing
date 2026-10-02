//! A small "internal LSP" for the script editor: a background-computed list
//! of autocomplete suggestions, plus hover/lookup info for a single
//! identifier (used for both mouse-hover and a keybind-at-cursor trigger;
//! see `app.rs`'s editor UI).
//!
//! Today the analysis is just our fixed host API table (`HOST_API`), which
//! doesn't depend on the script's own source at all. It's still run through
//! the same request/background/discard-stale-result machinery real analysis
//! will eventually need (parsing locals in scope at the cursor, listing Lua
//! stdlib members, ...) so that becoming slower later is a change to
//! `analyze`, not a rewrite of how the editor talks to it, see the repo
//! TODO for what's deliberately not built yet.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// name, one-line description, the single source of truth for both the
/// suggestion list and hover lookups, and also consulted by `lua_highlight`
/// to color host-function calls distinctly from ordinary identifiers.
pub const HOST_API: &[(&str, &str)] = &[
    ("clear", "clear({r,g,b}), fill the whole frame with a color"),
    ("line", "line(x0,y0,x1,y1,{r,g,b,a}), draw a line"),
    ("rect", "rect(x0,y0,x1,y1,{r,g,b,a}), fill an axis-aligned rectangle"),
    ("text", "text(x, y, string, {r,g,b,a}, [height]) -> width, height: pixel text (Monogram), height = line height in pixels"),
    ("text_size", "text_size(string, [height]) -> width, height: measure text without drawing it"),
    ("FONT_HEIGHT", "12, the pixel font's own line height; whole multiples stay crisp"),
    ("sprite_register", "sprite_register({image = rows of palette indices, palette = colors}) -> id: register once, 0 is transparent"),
    ("sprite", "sprite(id, x, y, [scale or {scale, flip_x, flip_y, src = {x, y, w, h}}]) -> width, height: draw a sprite, or part of one"),
    ("sprite_size", "sprite_size(id) -> width, height: a sprite's size in its own pixels"),
    ("circle", "circle(x, y, radius, {r,g,b,a}): a filled circle"),
    ("triangle", "triangle(x1, y1, x2, y2, x3, y3, {r,g,b,a}): a filled triangle"),
    ("polygon", "polygon(points, {r,g,b,a}): a filled polygon; points {x1, y1, ...} or {{x, y}, ...}"),
    ("pixel", "pixel(x,y,{r,g,b,a}), set one pixel"),
    ("fft_left", "fft_left(samples) -> spectrum, left channel's magnitude spectrum"),
    ("fft_right", "fft_right(samples) -> spectrum, right channel's magnitude spectrum"),
    ("beat", "beat([seconds]) -> beats: song position in quarter notes (nil without a MIDI score)"),
    ("time_at_beat", "time_at_beat(beat) -> seconds: when a beat falls, in song seconds"),
    ("bar", "bar([seconds]) -> bar, beat_in_bar: bar number from 1, beats into it from 0"),
    ("tempo", "tempo([seconds]) -> bpm: quarter notes per minute"),
    ("time_signature", "time_signature([seconds]) -> numerator, denominator"),
    ("level_left", "level_left() -> number: this frame's left loudness (RMS), 0 when silent"),
    ("level_right", "level_right() -> number: this frame's right loudness (RMS)"),
    ("onset", "onset() -> hit, strength: whether a sound just started this frame"),
    ("notes_between", "notes_between(t0, t1) -> notes: whole notes sounding in the window, {id, channel, key, velocity, start, stop}"),
    ("active_notes", "active_notes() -> notes, MIDI notes held right now: {channel, key, velocity, source (\"song\" or \"script\")}"),
    ("upcoming_notes", "upcoming_notes() -> notes, note on/off changes within NOTE_LOOKAHEAD"),
    ("midi_channels", "midi_channels() -> channels, channels the current file uses"),
    ("channel_enabled", "channel_enabled(channel) -> bool, is this channel enabled"),
    ("set_channel_enabled", "set_channel_enabled(channel, enabled), mute/unmute a channel"),
    ("playback", "playback() -> {position, length, speed, paused, finished, loop_enabled, generation}, times in song seconds"),
    ("set_paused", "set_paused(paused): pause or resume playback (applied after this frame)"),
    ("seek", "seek(seconds): jump to a song position (applied after this frame)"),
    ("store_get", "store_get(key) -> value: the script's saved data, nil if none"),
    ("store_set", "store_set(key, value): save script data across restarts (nil deletes)"),
    ("log", "log(message), append to this script's log (identical repeats collapse)"),
    ("debug_locals", "debug_locals([label]), snapshot the locals in scope right here, shown in the Script Settings tab"),
    ("setting_bool", "setting_bool(key, default) -> bool"),
    ("setting_int", "setting_int(key, default, min, max) -> integer"),
    ("setting_float", "setting_float(key, default, min, max) -> number"),
    ("setting_color", "setting_color(key, default) -> {r,g,b}"),
    ("setting_string", "setting_string(key, default) -> string"),
    ("setting_selection", "setting_selection(key, options, defaults, max_selections) -> selected"),
    ("mouse", "mouse() -> x, y (buffer pixels), or nil while the pointer isn't over the visualizer"),
    ("mouse_delta", "mouse_delta() -> dx, dy: pointer movement since last frame, in buffer pixels"),
    ("scroll", "scroll() -> dx, dy: scrolling since last frame (positive y is up)"),
    ("mouse_down", "mouse_down([button]) -> bool: held right now; \"left\" (default), \"right\", \"middle\""),
    ("mouse_pressed", "mouse_pressed([button]) -> bool: went down this frame"),
    ("mouse_released", "mouse_released([button]) -> bool: went up this frame"),
    ("input_register", "input_register(name, default_keys) -> id: a rebindable control (Script Settings > Controls), once at the top"),
    ("input", "input(id) -> \"pressed\", \"held\", \"released\" or \"up\": the action's state this frame"),
    ("input_down", "input_down(id) -> bool: the action is held (pressed this frame or earlier)"),
    ("text_typed", "text_typed() -> string: the characters typed this frame, while focused"),
    ("typing_begin", "typing_begin([text], [{max_length, multiline}]): start a typing span; the app edits, you draw"),
    ("typing_state", "typing_state() -> {active, done, cancelled, text, cursor, line, column, key}"),
    ("typing_end", "typing_end() -> text: stop the typing span"),
    ("play_note", "play_note(key, [{channel, velocity, duration, delay}]) -> bool: play a note on the song's instrument (MIDI only)"),
    ("note_on", "note_on(key, [{channel, velocity}]) -> bool: start a note held until note_off (MIDI only)"),
    ("note_off", "note_off(key, [channel]): release a note started with note_on"),
    ("stop_notes", "stop_notes(): release every note the script is playing, cancel its sequences"),
    ("notes_playable", "notes_playable() -> bool: whether play_note works now (a MIDI song with a soundfont)"),
    ("sequence_register", "sequence_register(notes) -> id: notes in beats, {key, at, length, velocity, channel} or key numbers; once, at the top"),
    ("sequence_play", "sequence_play(id, [{channel, transpose, tempo, delay}]) -> handle or nil: play it, at the song's tempo by default"),
    ("sequence_stop", "sequence_stop(handle): stop one sequence_play"),
    ("has_focus", "has_focus() -> bool: whether keys go to this script (clicked into, or dedicated fullscreen)"),
    ("display_mode", "display_mode() -> \"window\", \"fullscreen\" or \"dedicated\""),
    ("set_cursor_visible", "set_cursor_visible(visible): hide the system cursor over the visualizer (draw your own)"),
    ("set_cursor_locked", "set_cursor_locked(locked): keep the cursor on screen, dedicated fullscreen only"),
    ("SAMPLE_RATE", "the engine's sample rate, in Hz"),
    ("NOTE_LOOKAHEAD", "seconds upcoming_notes() looks ahead"),
    ("DT", "seconds since the previous render() call (0.0 on the first, at most 0.25)"),
    ("TIME", "seconds since this script started, wall clock, uncapped"),
    ("FRAME", "frames rendered since this script started (1 on the first)"),
    ("render", "render(width, height, left, right), define this; called once per frame"),
];

/// Hover/lookup info for a single identifier, by exact name match against
/// [`HOST_API`]. Cheap enough (linear scan of ~20 entries) to call directly
/// from the UI thread every frame the mouse is over the editor, no need to
/// route this through [`CompletionWorker`].
pub fn lookup(word: &str) -> Option<&'static str> {
    HOST_API.iter().find(|&&(name, _)| name == word).map(|&(_, detail)| detail)
}

/// One autocomplete suggestion.
#[derive(Clone)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
}

/// Computes suggestions off the UI thread on every [`request`](Self::request),
/// keeping only the result of the most recent request, an in-flight one
/// that's since been superseded by a newer edit finishes but is discarded
/// rather than overwriting a fresher result.
pub struct CompletionWorker {
    generation: Arc<AtomicU64>,
    suggestions: Arc<Mutex<Vec<Suggestion>>>,
}

impl CompletionWorker {
    pub fn new() -> Self {
        let worker = Self {
            generation: Arc::new(AtomicU64::new(0)),
            suggestions: Arc::new(Mutex::new(Vec::new())),
        };
        worker.request(String::new());
        worker
    }

    /// Call whenever the script source changes. `source` isn't used by
    /// today's analysis, but the request already carries it for when that
    /// changes (see module docs).
    pub fn request(&self, source: String) {
        let this_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = Arc::clone(&self.generation);
        let suggestions = Arc::clone(&self.suggestions);
        thread::spawn(move || {
            let result = analyze(&source);
            if generation.load(Ordering::SeqCst) == this_generation {
                *suggestions.lock().unwrap() = result;
            }
        });
    }

    /// The most recently completed (non-stale) suggestion list.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        self.suggestions.lock().unwrap().clone()
    }
}

impl Default for CompletionWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Today: just the fixed host API, regardless of `source`. The parameter is
/// already here for when this grows into real parsing (locals in scope at
/// the cursor, Lua stdlib members, ...), deliberately parked, see TODO.
fn analyze(_source: &str) -> Vec<Suggestion> {
    HOST_API
        .iter()
        .map(|&(label, detail)| Suggestion { label: label.to_string(), detail: detail.to_string() })
        .collect()
}
