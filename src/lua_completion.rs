//! A small "internal LSP" for the script editor: a background-computed list
//! of autocomplete suggestions, plus hover/lookup info for a single
//! identifier (used for both mouse-hover and a keybind-at-cursor trigger —
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
    ("pixel", "pixel(x,y,{r,g,b,a}), set one pixel"),
    ("fft_left", "fft_left(samples) -> spectrum, left channel's magnitude spectrum"),
    ("fft_right", "fft_right(samples) -> spectrum, right channel's magnitude spectrum"),
    ("active_notes", "active_notes() -> notes, MIDI notes held right now"),
    ("upcoming_notes", "upcoming_notes() -> notes, note on/off changes within NOTE_LOOKAHEAD"),
    ("midi_channels", "midi_channels() -> channels, channels the current file uses"),
    ("channel_enabled", "channel_enabled(channel) -> bool, is this channel enabled"),
    ("set_channel_enabled", "set_channel_enabled(channel, enabled), mute/unmute a channel"),
    ("playback", "playback() -> {position,length,speed,paused,finished,loop_enabled}"),
    ("log", "log(message), append to this script's log (identical repeats collapse)"),
    ("debug_locals", "debug_locals([label]), snapshot the locals in scope right here, shown in Settings"),
    ("setting_bool", "setting_bool(key, default) -> bool"),
    ("setting_int", "setting_int(key, default, min, max) -> integer"),
    ("setting_float", "setting_float(key, default, min, max) -> number"),
    ("setting_color", "setting_color(key, default) -> {r,g,b}"),
    ("setting_string", "setting_string(key, default) -> string"),
    ("setting_selection", "setting_selection(key, options, defaults, max_selections) -> selected"),
    ("SAMPLE_RATE", "the engine's sample rate, in Hz"),
    ("NOTE_LOOKAHEAD", "seconds upcoming_notes() looks ahead"),
    ("DT", "seconds since the previous render() call (0.0 on the first, at most 0.25)"),
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
