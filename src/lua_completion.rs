//! The script editor's background worker and host API table. Every edit
//! sends the source to [`CompletionWorker`], which analyzes it off the UI
//! thread (`lua_analysis`: scopes, warnings, completions, signatures) and
//! keeps the newest result; a stale one finishing late is dropped. Also
//! hover/lookup info for a single identifier.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::lua_analysis::{self, Analysis};

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
    ("sprite", "sprite(id, x, y, [scale or {scale, flip_x, flip_y, src = {x, y, w, h}, palette = {[i] = color}, tint = color}]) -> width, height: draw a sprite, or part of one, optionally recolored"),
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
    ("playback", "playback() -> {position, length, speed, paused, finished, loop_enabled, generation, song_name, song_path, song_id}, times in song seconds"),
    ("set_paused", "set_paused(paused): pause or resume playback (applied after this frame)"),
    ("seek", "seek(seconds): jump to a song position, seek(0) restarts (applied after this frame)"),
    ("set_speed", "set_speed(speed): playback speed, 1.0 is normal (applied after this frame)"),
    ("playlist", "playlist() -> {name, playing, current, entries = {{name, length, missing}, ...}} or nil: the current playlist, read-only"),
    ("play_track", "play_track(index): play the current playlist's index-th song"),
    ("next_track", "next_track(): the Next button"),
    ("previous_track", "previous_track(): the Previous button"),
    ("set_loop", "set_loop(mode): \"off\", \"one\" or \"all\""),
    ("set_shuffle", "set_shuffle(on): shuffle the playlist or not"),
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
    HOST_API
        .iter()
        .chain(lua_analysis::LUA_GLOBALS)
        .find(|&&(name, _)| name == word)
        .map(|&(_, detail)| detail)
}

/// One autocomplete suggestion.
#[derive(Clone)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
}

/// Analyzes the script off the UI thread on every [`request`](Self::request),
/// keeping only the result of the most recent request: an in-flight one
/// that's since been superseded by a newer edit finishes but is discarded
/// rather than overwriting a fresher result.
pub struct CompletionWorker {
    generation: Arc<AtomicU64>,
    analysis: Arc<Mutex<Arc<Analysis>>>,
    /// Globals a script has without defining them.
    known: Arc<HashSet<String>>,
}

impl CompletionWorker {
    /// `host_globals`: the app's own globals, from a real script VM.
    pub fn new(host_globals: Vec<String>) -> Self {
        let worker = Self {
            generation: Arc::new(AtomicU64::new(0)),
            analysis: Arc::default(),
            known: Arc::new(lua_analysis::known_globals(host_globals)),
        };
        worker.request(String::new());
        worker
    }

    /// Call whenever the script source changes.
    pub fn request(&self, source: String) {
        let this_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = Arc::clone(&self.generation);
        let analysis = Arc::clone(&self.analysis);
        let known = Arc::clone(&self.known);
        thread::spawn(move || {
            let result = Arc::new(lua_analysis::analyze(&source, &known));
            if generation.load(Ordering::SeqCst) == this_generation {
                *analysis.lock().unwrap() = result;
            }
        });
    }

    /// The most recent analysis (possibly an edit or two behind).
    pub fn analysis(&self) -> Arc<Analysis> {
        Arc::clone(&self.analysis.lock().unwrap())
    }

    /// Every host function, for the Functions list.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        HOST_API
            .iter()
            .map(|&(label, detail)| Suggestion { label: label.to_string(), detail: detail.to_string() })
            .collect()
    }
}
