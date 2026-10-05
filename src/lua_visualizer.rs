//! Lua-scripted visualizer.
//!
//! A script defines `function render(width, height, left, right)`, called
//! once per frame with the newly-arrived stereo samples as two flat,
//! 1-indexed tables of numbers. It draws with four host-provided functions:
//!
//! * `clear({r, g, b})`
//! * `line(x0, y0, x1, y1, {r, g, b [, a]})`
//! * `rect(x0, y0, x1, y1, {r, g, b [, a]})`
//! * `pixel(x, y, {r, g, b [, a]})`
//!
//! Colors are plain Lua tables. `a` is opacity 0.0..1.0, default 1.0 (an
//! opaque draw overwrites; a translucent one alpha-blends over what's there).
//! The interpreted cost of reading a color table is paid once per draw call
//! (queued as a packed `u32`), never once per pixel.
//!
//! Other host functions:
//! * `fft_left(left)` / `fft_right(right)`, windowed magnitude spectrum
//!   (length `spectrum.rs`'s `FFT_SIZE / 2`) of a channel's last ~1024
//!   samples, computed in Rust so scripts stay fast without their own FFT.
//! * `active_notes()` / `upcoming_notes()`, notes held now / changing within
//!   `NOTE_LOOKAHEAD` seconds.
//! * `midi_channels()`, the channels the current file uses.
//! * `channel_enabled(channel)`, the GUI's per-channel toggle (true if
//!   nothing is detected yet).
//! * `set_channel_enabled(channel, enabled)`, a script can mute/unmute a
//!   channel too, not just read its state (e.g. a game script silencing the
//!   channel for a dead player). Goes through the same command as the GUI's
//!   checkboxes, so it can't disable the last remaining enabled channel
//!   either.
//! * `playback()`, transport state: `{position, length, speed, paused,
//!   finished, loop_enabled, generation}` plus which song (`song_name`,
//!   `song_path`, `song_id`). The thing to check before writing into a
//!   scrolling history buffer, so pausing freezes the picture instead of
//!   scrolling through a frozen spectrum.
//! * `log(message)`, appends to this script's log, shown in the Settings
//!   popup. Identical consecutive messages collapse into one entry with a
//!   count, so a `log()` called every frame doesn't flood the pane.
//! * `DT`, seconds since the previous `render` call (0.0 on the first,
//!   capped at 0.25).
//! * Input: `mouse()`, `mouse_delta()`, `scroll()`,
//!   `mouse_down/pressed/released([button])`, `has_focus()`,
//!   `display_mode()`; keys through rebindable actions (`input_register`,
//!   `input`, `input_down`) and typing (`text_typed`, `typing_*`). Mouse
//!   while the pointer is over the visualizer, keys while it has focus;
//!   Escape is never reported.
//! * Playing notes: `play_note`, `note_on` / `note_off`, `stop_notes`,
//!   `notes_playable`, `sequence_register` / `sequence_play` /
//!   `sequence_stop`, on the live synth (`live.rs`), queued and passed on by
//!   the app like `set_channel_enabled`.
//! * `text(x, y, string, color, [height])` / `text_size(string, [height])`,
//!   pixel text in the built-in Monogram font (see `pixel_font.rs`).
//! * `circle`, `triangle`, `polygon`: filled shapes.
//! * `sprite_register` / `sprite` / `sprite_size`: palette-indexed pixel
//!   images, converted to colors once at registration, drawn
//!   nearest-neighbor, optionally recolored per draw (`palette`, `tint`).
//! * `beat`, `time_at_beat`, `bar`, `tempo`, `time_signature`: the MIDI
//!   file's musical timing (nil for plain audio).
//! * `level_left`, `level_right` (RMS) and `onset()` (spectral flux), audio
//!   features computed in Rust.
//! * `notes_between(t0, t1)`, every note sounding in a time window, whole:
//!   `{id, channel, key, velocity, start, stop}` (see `midi_notes.rs`).
//! * `set_paused(paused)` / `seek(seconds)`, playback control, queued like
//!   `set_channel_enabled` and carried out by the app.
//! * `store_get(key)` / `store_set(key, value)`, the script's own saved data
//!   (`<script>.lua.store.json`), separate from user-facing settings.
//! * `FRAME` (frames since the script started) and `TIME` (seconds since
//!   it started, wall clock, uncapped).
//! * Cursor: `set_cursor_visible(visible)` (applies over the visualizer) and
//!   `set_cursor_locked(locked)` (keeps it on screen, dedicated fullscreen
//!   only).
//!
//! Settings: a script can expose typed, user-editable values via
//! `setting_bool/int/float/color/string/selection(key, ...)`, called every
//! frame, the first call each compile registers the descriptor (default,
//! range, options); every call after that just returns the live value, so
//! editing a slider in the Settings window updates the running script
//! immediately, without a recompile or disturbing any history the script is
//! keeping. Values persist to `<script>.lua.settings.json` next to the
//! script and are re-applied on the next load, falling back to the script's
//! own default for anything the sidecar doesn't have.
//!
//! Live reload: [`LuaVisualizer::set_source`] recompiles against a fresh Lua
//! VM. A script that fails to compile, or doesn't define `render`, leaves the
//! previously working script (if any) running, the error is recorded but
//! never blanks the view, so a mid-edit typo doesn't stop playback.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use eframe::egui;
use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value};
use serde::Deserialize;

use crate::config::{config_dir, nice_name};
use crate::engine::{EngineView, NOTE_LOOKAHEAD_SECS};
use crate::midi_notes::NoteList;
use crate::pixel_font;
use crate::spectrum::{self, OnsetDetector, SpectrumAnalyzer};
use crate::gamepad::{AXIS_NAMES, PadInput};
use crate::live::{LiveCommand, LiveNote};
use crate::playlist::LoopMode;
use crate::typing::TypingSpan;
use crate::visualizer::{
    ActiveNote, CursorRequest, NoteChange, NotesSnapshot, RESERVED_KEYS, StereoFrame, Visualizer,
    VisualizerInput,
};

// BUNDLED_SCRIPTS: &[(&str, &str, &str)] of (category, file name,
// contents), generated by build.rs from the .lua files in
// assets/visualizers/{visualizers,games,templates,examples}, see that file.
// Adding, removing, or renaming a bundled script is a file move in assets/,
// not an edit here.
include!(concat!(env!("OUT_DIR"), "/bundled_scripts.rs"));

const BLANK_SCRIPT_TEMPLATE: &str = "\
-- New visualizer. `render` is called once per frame with the window size
-- and the new stereo samples (flat, 1-indexed tables) since the last frame.
--
-- Draw with clear/line/rect/pixel; color tables take an optional `a` (0..1).
--   clear({r,g,b})  line(x0,y0,x1,y1,{r,g,b,a})  rect(...)  pixel(x,y,{r,g,b,a})
--   circle(x,y,r,color)  triangle(x1,y1,x2,y2,x3,y3,color)  polygon(points,color)
--   sprite_register({image = rows, palette = colors}) once, then sprite(id, x, y, scale)
--   text(x, y, string, color, height) / text_size(string, height)  pixel text
-- fft_left(left) / fft_right(right)     magnitude spectrum, computed in Rust
-- level_left() / level_right() / onset() loudness, and whether a sound just started
-- beat() / bar() / tempo()              musical timing from the MIDI file
-- notes_between(t0, t1)                 whole notes with start/stop times
-- active_notes() / upcoming_notes()     notes held now / changing soon
-- midi_channels() / channel_enabled(c)  the file's channels and their toggles
-- set_channel_enabled(c, enabled)       mute/unmute a channel from the script
-- play_note(key, {channel, velocity, duration})  play a note on the song's instruments
-- playback() / set_paused(p) / seek(t)  transport state and control
-- playlist() / play_track(i) / next_track()  the current playlist
-- store_get(key) / store_set(key, v)    data saved across restarts
-- FRAME / TIME                          frame count, seconds since start
-- log(message) / DT                     debug log, seconds since last frame
-- mouse() / has_focus()                 the mouse over the visualizer (see Docs)
-- input_register(name, keys) / input(id) rebindable keys; typing_begin() for text
-- setting_bool/int/float/color/string/selection(key, ...)  user-editable values
-- settings_preset(name, {key = value, ...})  a named set of them to pick
-- recording() / recording_ready() / recording_done()  taking part in recordings

function render(width, height, left, right)
    clear({ r = 16, g = 16, b = 24 })
end
";

/// The folder user (and bundled default) visualizer scripts live in,
/// creating it and seeding the defaults into it on first use only, a
/// `.seeded` marker (distinct from the scripts' own presence) remembers that
/// the one-time seed already happened, so a bundled default the user deletes
/// stays deleted on later launches instead of quietly reappearing. "Restore
/// default" (`bundled_default`) is the deliberate way to bring one back.
pub fn scripts_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("visualizers");
    fs::create_dir_all(&dir)?;
    seed_bundled(&dir)?;
    Ok(dir)
}

/// Add the bundled visualizers and games to `dir` that haven't been added
/// there before (see `scripts_dir`).
fn seed_bundled(dir: &Path) -> Result<()> {
    let marker = dir.join(".seeded");
    // The marker lists the bundled scripts already added, one per line, so
    // ones bundled later are added once too, and a deleted one stays
    // deleted. An empty marker is from before it kept a list: everything
    // bundled up to 0.1.8 had been added then.
    let mut seeded: Vec<String> = match fs::read_to_string(&marker) {
        Ok(text) if text.trim().is_empty() => SEEDED_BY_0_1_8.iter().map(|s| s.to_string()).collect(),
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    };
    let mut added = false;
    for &(_, name, contents) in BUNDLED_SCRIPTS.iter().filter(|(category, ..)| is_added(category)) {
        if !seeded.iter().any(|s| s == name) {
            seed_default(dir, name, contents)?;
            seeded.push(name.to_string());
            added = true;
        }
    }
    if added || !marker.exists() {
        fs::write(&marker, seeded.join("\n"))?;
    }
    Ok(())
}

/// What the one-time seed had added by 0.1.8, for installs from then.
const SEEDED_BY_0_1_8: &[&str] =
    &["waveform.lua", "fft.lua", "keyboard.lua", "spectrogram.lua", "letters.lua", "snake.lua", "pulse.lua", "disco.lua"];

/// Bundled visualizers and games go into the user's folder; templates and
/// examples are only offered by "New".
fn is_added(category: &str) -> bool {
    matches!(category, "visualizers" | "games")
}

/// A bundled script's category ("visualizers", "games", "templates",
/// "examples") by its file name.
pub fn bundled_category(file_name: &str) -> Option<&'static str> {
    BUNDLED_SCRIPTS.iter().find(|(_, name, _)| *name == file_name).map(|(category, ..)| *category)
}

/// How a category is headed in the lists.
pub fn category_title(category: &str) -> &'static str {
    match category {
        "visualizers" => "Visualizers",
        "games" => "Games",
        "templates" => "Templates",
        "examples" => "Examples",
        _ => "Your scripts",
    }
}

fn seed_default(dir: &Path, name: &str, contents: &str) -> Result<()> {
    let path = dir.join(name);
    if !path.exists() {
        fs::write(path, contents)?;
    }
    Ok(())
}

/// Every `.lua` file directly inside `dir`, sorted by name.
pub fn list_scripts(dir: &Path) -> Vec<PathBuf> {
    let mut scripts: Vec<PathBuf> = fs::read_dir(dir)
        .map(|read_dir| {
            read_dir
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("lua"))
                .collect()
        })
        .unwrap_or_default();
    scripts.sort();
    scripts
}

/// Starting points offered by the "New" button, as (category, name,
/// contents): a blank script, then every bundled script by category.
/// Display names are the bare file stem, the same convention
/// `display_name`/`nice_name` use for a script's own name elsewhere.
pub fn new_script_templates() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut templates = vec![("templates", "Blank", BLANK_SCRIPT_TEMPLATE)];
    for &(category, name, contents) in BUNDLED_SCRIPTS {
        templates.push((category, name.strip_suffix(".lua").unwrap_or(name), contents));
    }
    templates.sort_by_key(|(category, ..)| ["templates", "examples", "visualizers", "games"].iter().position(|c| c == category));
    templates
}

/// Create `<dir>/untitled-N.lua` (picking the first free `N`) from `content`
/// and return its path.
pub fn new_script_from_template(dir: &Path, content: &str) -> Result<PathBuf> {
    let mut n = 1;
    let path = loop {
        let candidate = dir.join(format!("untitled-{n}.lua"));
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
    };
    fs::write(&path, content)?;
    Ok(path)
}

/// Write a script's source to its file: the error to show, if it failed.
pub fn save_script(path: &Path, source: &str) -> Option<String> {
    fs::write(path, source).err().map(|e| format!("failed to save {}: {e}", path.display()))
}

pub fn display_name(path: &Path) -> String {
    nice_name(path)
}

/// Rename the script at `old` to `<new_name>.lua` in the same folder, taking
/// its settings sidecar along, and return the new path. Refuses names that
/// can't be Windows file names and names already taken (a change of case
/// only is fine: same file).
pub fn rename_script(old: &Path, new_name: &str) -> Result<PathBuf> {
    let name = new_name.trim();
    validate_script_name(name)?;
    let new = old.with_file_name(format!("{name}.lua"));
    if new == old {
        return Ok(new);
    }
    let same_file = match (old.canonicalize(), new.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if new.exists() && !same_file {
        anyhow::bail!("there's already a script called {name}.lua");
    }
    fs::rename(old, &new).with_context(|| format!("renaming {}", old.display()))?;
    for (old_sidecar, new_sidecar) in [
        (sidecar_path(old), sidecar_path(&new)),
        (store_path(old), store_path(&new)),
        (controls_path(old), controls_path(&new)),
        (user_presets_path(old), user_presets_path(&new)),
        (crate::library::installed_path(old), crate::library::installed_path(&new)),
    ] {
        if old_sidecar.exists() {
            fs::rename(&old_sidecar, &new_sidecar)
                .with_context(|| format!("renaming {}", old_sidecar.display()))?;
        }
    }
    Ok(new)
}

/// A script name (without `.lua`) that works as a file name everywhere,
/// Windows' rules being the strict ones.
fn validate_script_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("the name can't be empty");
    }
    if let Some(c) = name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control()) {
        anyhow::bail!("names can't contain '{c}'");
    }
    if name.starts_with('.') || name.ends_with('.') || name.ends_with(' ') {
        anyhow::bail!("names can't start with a dot or end with a dot or space");
    }
    let upper = name.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if device {
        anyhow::bail!("'{name}' is a reserved name on Windows");
    }
    Ok(())
}

/// The script line (1-based) a compile or runtime error points at, from
/// mlua's `[string "visualizer"]:12: ...` location prefix (the chunk name is
/// set in `compile`). The first one in the message is the error itself;
/// later ones are the stack traceback.
pub fn error_line(error: &str) -> Option<usize> {
    const MARKER: &str = "\"visualizer\"]:";
    let start = error.find(MARKER)? + MARKER.len();
    let digits: String = error[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&line| line > 0)
}

/// Every global a script starts with (the app's functions and constants,
/// deprecated ones included, and Lua's libraries), read from a real
/// script VM, for the editor's "isn't defined" check.
pub fn host_global_names() -> Vec<String> {
    let visualizer = LuaVisualizer::new(BLANK_SCRIPT_TEMPLATE.to_string(), None, 44_100);
    let Some(compiled) = &visualizer.compiled else { return Vec::new() };
    compiled
        .lua
        .globals()
        .pairs::<Value, Value>()
        .filter_map(|pair| match pair {
            Ok((Value::String(name), _)) => name.to_str().ok().map(|n| n.to_string()),
            _ => None,
        })
        .filter(|name| name != "render")
        .collect()
}

/// The current embedded source for a bundled visualizer or game (the ones
/// added to the user's folder), by file name: how the app offers "Restore
/// default" when one has drifted from the shipped version. Templates and
/// examples are never added under their own name ("New" writes a fresh
/// `untitled-N.lua`), so there's nothing of theirs to restore.
pub fn bundled_default(file_name: &str) -> Option<&'static str> {
    BUNDLED_SCRIPTS
        .iter()
        .find(|&&(category, name, _)| name == file_name && is_added(category))
        .map(|&(_, _, contents)| contents)
}

#[derive(Clone)]
enum DrawCommand {
    Clear(u32),
    Line { x0: f32, y0: f32, x1: f32, y1: f32, color: u32 },
    Rect { x0: f32, y0: f32, x1: f32, y1: f32, color: u32 },
    Pixel { x: i32, y: i32, color: u32 },
    Circle { x: f32, y: f32, radius: f32, color: u32 },
    Polygon { points: Vec<(f32, f32)>, color: u32 },
    /// `src` is the part of the sprite to draw: (x, y, width, height) in
    /// sprite pixels, already clipped to the sprite. `colors` replaces the
    /// sprite's palette for this draw (`palette` / `tint` options), indexed
    /// like `Sprite::palette`.
    Sprite {
        index: usize,
        x: f32,
        y: f32,
        scale: f32,
        flip_x: bool,
        flip_y: bool,
        src: (usize, usize, usize, usize),
        colors: Option<Rc<[u32]>>,
    },
}

/// A sprite a script registered: its palette-indexed image turned into
/// packed colors once, at registration, so a plain draw is just copying.
/// The indices are kept too, for draws that recolor it.
struct Sprite {
    width: usize,
    height: usize,
    /// Row-major `0xAARRGGBB`; alpha 0 is transparent (palette index 0).
    pixels: Vec<u32>,
    /// Row-major palette indices, 0 = transparent.
    indices: Vec<u32>,
    /// `0xAARRGGBB` per palette index; `palette[0]` is transparent.
    palette: Vec<u32>,
}

/// `color` multiplied by `tint` (both `0xAARRGGBB`), channel by channel.
fn tint_color(color: u32, tint: u32) -> u32 {
    let channel = |shift: u32| {
        let a = (color >> shift) & 0xFF;
        let b = (tint >> shift) & 0xFF;
        ((a * b + 127) / 255) << shift
    };
    channel(24) | channel(16) | channel(8) | channel(0)
}

/// How long one run of a script's code (its top level when it loads, or
/// one `render()` call) may take before the watchdog stops it, unless
/// `set_watchdog_limit` says otherwise.
pub const WATCHDOG_LIMIT: Duration = Duration::from_secs(3);
/// The most memory each script's Lua may use, in bytes (0: no limit).
/// Set for the whole process by one that runs scripts it can't trust
/// (`synththing preview`, which a library server runs on uploads).
static MEMORY_LIMIT: AtomicUsize = AtomicUsize::new(0);

/// Limit the memory of scripts compiled from now on (see `MEMORY_LIMIT`).
pub fn set_memory_limit(bytes: usize) {
    MEMORY_LIMIT.store(bytes, Ordering::Relaxed);
}

/// The start of the error a script stopped by the watchdog gets.
const WATCHDOG_TAG: &str = "stopped by the watchdog";
/// The start of the error a script stopped with the stop flag gets.
const STOPPED_TAG: &str = "stopped with the Stop button";

fn watchdog_message(limit: Duration) -> String {
    format!(
        "{WATCHDOG_TAG}: the script ran for over {} seconds without finishing (an endless loop, or too much \
         work in one go?). Fix it and save, or press Restart (F5).",
        limit.as_secs()
    )
}

fn stopped_message() -> String {
    format!("{STOPPED_TAG} while it was busy. Fix it and save, or press Restart (F5).")
}

/// Whether `error` is the watchdog or the stop flag stopping the script.
fn is_stop_error(error: &str) -> bool {
    error.contains(WATCHDOG_TAG) || error.contains(STOPPED_TAG)
}

/// Most sprites one script can register: plenty for any real use, and it
/// turns "registered a sprite inside render() every frame" into an error
/// instead of memory that grows forever.
const MAX_SPRITES: usize = 10_000;

/// Most points `polygon()` takes (a typo shouldn't be able to ask for
/// millions).
const MAX_POLYGON_POINTS: usize = 4096;

// --- settings -----------------------------------------------------------

/// A setting's current (or default) value. The type is always known from
/// context (the matching [`SettingKind`] variant), so there's no separate
/// discriminant check needed beyond the `match` arms already doing one.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Plain RGB, no alpha, draw-call alpha is a separate, per-call concern.
    Color([u8; 3]),
    String(String),
    /// Selected option labels, oldest-selected first, that order is what
    /// lets a new pick past `max_selections` evict the oldest one rather
    /// than needing a separate disabled-checkbox state.
    Selection(Vec<String>),
}

#[derive(Clone, Debug)]
pub enum SettingKind {
    Bool,
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    Color,
    String,
    Selection { options: Vec<String>, max_selections: usize },
}

#[derive(Clone, Debug)]
pub struct SettingDescriptor {
    pub key: String,
    pub kind: SettingKind,
    pub value: SettingValue,
    /// What the script gave as its default (made to fit, like the value).
    pub default: SettingValue,
    pub labels: ItemLabels,
}

/// A named set of setting values: one a script offers
/// (`settings_preset`), or one the user saved (`<script>.lua.presets.json`).
/// Values are as the settings file keeps them.
#[derive(Clone, Debug, PartialEq, serde::Serialize, Deserialize)]
pub struct SettingsPreset {
    pub name: String,
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// `value` (as the settings file and presets keep it) as a value of a
/// setting of `kind`, made to fit; `None` if it can't be one.
pub fn setting_from_json(kind: &SettingKind, value: &serde_json::Value) -> Option<SettingValue> {
    Some(match kind {
        SettingKind::Bool => SettingValue::Bool(value.as_bool()?),
        SettingKind::Int { min, max } => SettingValue::Int(value.as_i64().or_else(|| value.as_f64().map(|f| f.round() as i64))?.clamp(*min, *max)),
        SettingKind::Float { min, max } => SettingValue::Float(value.as_f64()?.clamp(*min, *max)),
        SettingKind::Color => {
            let channel = |c: &str| value.get(c).and_then(serde_json::Value::as_f64).map(|v| v.clamp(0.0, 255.0) as u8);
            SettingValue::Color([channel("r")?, channel("g")?, channel("b")?])
        }
        SettingKind::String => SettingValue::String(value.as_str()?.to_string()),
        SettingKind::Selection { options, max_selections } => {
            let picked: Vec<String> = value
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|s| options.contains(s))
                .take(*max_selections)
                .collect();
            SettingValue::Selection(picked)
        }
    })
}

/// A user's presets for a script: `<script>.lua.presets.json`.
pub fn user_presets_path(script_path: &Path) -> PathBuf {
    let mut name = script_path.as_os_str().to_os_string();
    name.push(".presets.json");
    PathBuf::from(name)
}

pub fn load_user_presets(script_path: &Path) -> Vec<SettingsPreset> {
    fs::read_to_string(user_presets_path(script_path)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_user_presets(script_path: &Path, presets: &[SettingsPreset]) -> std::io::Result<()> {
    let path = user_presets_path(script_path);
    if presets.is_empty() {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    fs::write(path, serde_json::to_string_pretty(presets).unwrap_or_default())
}

/// The settings that differ from their defaults, as a line of Lua that
/// offers them as a preset: `settings_preset("name", { speed = 2 })`.
pub fn preset_line(name: &str, descriptors: &[SettingDescriptor]) -> String {
    let lua_string = |s: &str| format!("{s:?}");
    let value = |v: &SettingValue| match v {
        SettingValue::Bool(b) => b.to_string(),
        SettingValue::Int(i) => i.to_string(),
        SettingValue::Float(f) => format!("{f}"),
        SettingValue::Color([r, g, b]) => format!("{{ r = {r}, g = {g}, b = {b} }}"),
        SettingValue::String(s) => lua_string(s),
        SettingValue::Selection(items) => {
            format!("{{ {} }}", items.iter().map(|s| lua_string(s)).collect::<Vec<_>>().join(", "))
        }
    };
    let is_name = |k: &str| {
        k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !crate::lua_analysis::KEYWORDS.contains(&k)
    };
    let fields: Vec<String> = descriptors
        .iter()
        .filter(|d| d.value != d.default)
        .map(|d| {
            let key = if is_name(&d.key) { d.key.clone() } else { format!("[{}]", lua_string(&d.key)) };
            format!("{key} = {}", value(&d.value))
        })
        .collect();
    format!("settings_preset({}, {{ {} }})", lua_string(name), fields.join(", "))
}

/// `source` with `line` (a `settings_preset` call) added under its leading
/// comment, or in place of the one with the same name.
pub fn with_preset_line(source: &str, name: &str, line: &str) -> String {
    let same = format!("settings_preset({:?},", name);
    let mut lines: Vec<&str> = source.lines().collect();
    if let Some(at) = lines.iter().position(|l| l.trim_start().starts_with(&same)) {
        lines[at] = line;
    } else {
        let after_comment = lines.iter().position(|l| !l.trim_start().starts_with("--")).unwrap_or(lines.len());
        let insert = if after_comment > 0 { after_comment } else { 0 };
        if after_comment > 0 {
            lines.insert(insert, line);
            lines.insert(insert, "");
        } else {
            lines.insert(0, "");
            lines.insert(0, line);
        }
    }
    let mut text = lines.join("\n");
    if source.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Where a setting or control is listed, and what it's for: the optional
/// last argument of `setting_*` and `input_register`,
/// `{ group = "Movement", info = "..." }`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemLabels {
    /// The group it's listed under (`None`: General).
    pub group: Option<String>,
    /// A longer explanation, shown on hover.
    pub info: Option<String>,
}

/// Read the `{ group, info }` table `function` was given, if any.
fn item_labels(function: &str, table: Option<Table>) -> mlua::Result<Option<ItemLabels>> {
    let Some(table) = table else { return Ok(None) };
    let mut labels = ItemLabels::default();
    for pair in table.pairs::<String, Value>() {
        let (key, value) = pair?;
        let text = match value {
            Value::String(s) => s.to_str()?.trim().to_string(),
            other => {
                return Err(mlua::Error::runtime(format!(
                    "{function}: {key} is text, not a {}",
                    other.type_name()
                )));
            }
        };
        let text = Some(text).filter(|t| !t.is_empty());
        match key.as_str() {
            "group" => labels.group = text,
            "info" => labels.info = text,
            other => {
                return Err(mlua::Error::runtime(format!(
                    "{function}: no option `{other}` (there are group and info)"
                )));
            }
        }
    }
    Ok(Some(labels))
}

/// Registered settings for one compiled script, seeded from the sidecar file
/// (if any) and filled in as the script's `setting_*` calls register each
/// one. Lives inside [`Compiled`], a fresh store per successful compile, so
/// switching scripts (or a script changing its own option list) can't leave
/// stale descriptors behind.
#[derive(Default)]
struct SettingsStore {
    descriptors: Vec<SettingDescriptor>,
    /// Sidecar values not yet claimed by a registering `setting_*` call.
    pending: HashMap<String, serde_json::Value>,
    /// What `settings_preset` offered, in order.
    presets: Vec<SettingsPreset>,
}

impl SettingsStore {
    fn find(&self, key: &str) -> Option<&SettingDescriptor> {
        self.descriptors.iter().find(|d| d.key == key)
    }

    /// Give `key` the group and info a `setting_*` call passed (calls
    /// without them leave what an earlier one gave).
    fn label(&mut self, key: &str, labels: Option<ItemLabels>) {
        if let Some(labels) = labels
            && let Some(d) = self.descriptors.iter_mut().find(|d| d.key == key)
        {
            d.labels = labels;
        }
    }

    fn get_or_register_bool(&mut self, key: &str, default: bool) -> bool {
        if let Some(SettingValue::Bool(v)) = self.find(key).map(|d| &d.value) {
            return *v;
        }
        let initial = self.pending.get(key).and_then(|v| v.as_bool()).unwrap_or(default);
        self.descriptors.push(SettingDescriptor {
            key: key.to_string(),
            kind: SettingKind::Bool,
            value: SettingValue::Bool(initial),
            default: SettingValue::Bool(default),
            labels: ItemLabels::default(),
        });
        initial
    }

    fn get_or_register_int(&mut self, key: &str, default: i64, min: i64, max: i64) -> i64 {
        let (min, max) = (min.min(max), min.max(max));
        if let Some(SettingValue::Int(v)) = self.find(key).map(|d| &d.value) {
            return *v;
        }
        let initial = self
            .pending
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .map(|v| v.clamp(min, max))
            .unwrap_or_else(|| default.clamp(min, max));
        self.descriptors.push(SettingDescriptor {
            key: key.to_string(),
            kind: SettingKind::Int { min, max },
            value: SettingValue::Int(initial),
            default: SettingValue::Int(default.clamp(min, max)),
            labels: ItemLabels::default(),
        });
        initial
    }

    fn get_or_register_float(&mut self, key: &str, default: f64, min: f64, max: f64) -> f64 {
        let (min, max) = (min.min(max), min.max(max));
        if let Some(SettingValue::Float(v)) = self.find(key).map(|d| &d.value) {
            return *v;
        }
        let initial = self
            .pending
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .map(|v| v.clamp(min, max))
            .unwrap_or_else(|| default.clamp(min, max));
        self.descriptors.push(SettingDescriptor {
            key: key.to_string(),
            kind: SettingKind::Float { min, max },
            value: SettingValue::Float(initial),
            default: SettingValue::Float(default.clamp(min, max)),
            labels: ItemLabels::default(),
        });
        initial
    }

    fn get_or_register_color(&mut self, key: &str, default: [u8; 3]) -> [u8; 3] {
        #[derive(Deserialize)]
        struct ColorJson {
            r: u8,
            g: u8,
            b: u8,
        }

        if let Some(SettingValue::Color(v)) = self.find(key).map(|d| &d.value) {
            return *v;
        }
        let initial = self
            .pending
            .get(key)
            .and_then(|v| serde_json::from_value::<ColorJson>(v.clone()).ok())
            .map(|c| [c.r, c.g, c.b])
            .unwrap_or(default);
        self.descriptors.push(SettingDescriptor {
            key: key.to_string(),
            kind: SettingKind::Color,
            value: SettingValue::Color(initial),
            default: SettingValue::Color(default),
            labels: ItemLabels::default(),
        });
        initial
    }

    fn get_or_register_string(&mut self, key: &str, default: &str) -> String {
        if let Some(SettingValue::String(v)) = self.find(key).map(|d| &d.value) {
            return v.clone();
        }
        let initial = self
            .pending
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| default.to_string());
        self.descriptors.push(SettingDescriptor {
            key: key.to_string(),
            kind: SettingKind::String,
            value: SettingValue::String(initial.clone()),
            default: SettingValue::String(default.to_string()),
            labels: ItemLabels::default(),
        });
        initial
    }

    fn get_or_register_selection(
        &mut self,
        key: &str,
        options: Vec<String>,
        defaults: Vec<String>,
        max_selections: usize,
    ) -> Vec<String> {
        if let Some(SettingValue::Selection(v)) = self.find(key).map(|d| &d.value) {
            return v.clone();
        }
        let max_selections = max_selections.max(1);
        let valid_defaults: Vec<String> =
            defaults.into_iter().filter(|d| options.contains(d)).take(max_selections).collect();
        let from_sidecar = self.pending.get(key).and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .filter(|s| options.contains(s))
                .take(max_selections)
                .collect::<Vec<_>>()
        });
        let initial = from_sidecar.filter(|v| !v.is_empty()).unwrap_or_else(|| valid_defaults.clone());
        self.descriptors.push(SettingDescriptor {
            key: key.to_string(),
            kind: SettingKind::Selection { options, max_selections },
            value: SettingValue::Selection(initial.clone()),
            default: SettingValue::Selection(valid_defaults),
            labels: ItemLabels::default(),
        });
        initial
    }

    /// Apply a value edited through the Settings window. Selection values are
    /// defensively truncated (keeping the most recent) in case a caller
    /// hands back more than `max_selections`, the UI itself should never do
    /// this, but a setting is cheap to protect either way.
    fn set(&mut self, key: &str, value: SettingValue) {
        if let Some(d) = self.descriptors.iter_mut().find(|d| d.key == key) {
            if let (SettingKind::Selection { max_selections, .. }, SettingValue::Selection(mut v)) =
                (&d.kind, value.clone())
            {
                if v.len() > *max_selections {
                    v = v.split_off(v.len() - *max_selections);
                }
                d.value = SettingValue::Selection(v);
            } else {
                d.value = value;
            }
        }
    }
}

fn sidecar_path(script_path: &Path) -> PathBuf {
    let mut name = script_path.as_os_str().to_os_string();
    name.push(".settings.json");
    PathBuf::from(name)
}

fn load_pending_settings(path: Option<&Path>) -> HashMap<String, serde_json::Value> {
    let Some(path) = path else { return HashMap::new() };
    fs::read_to_string(sidecar_path(path))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_sidecar(script_path: &Path, store: &SettingsStore) {
    let map: serde_json::Map<String, serde_json::Value> = store
        .descriptors
        .iter()
        .map(|d| (d.key.clone(), setting_value_to_json(&d.value)))
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&serde_json::Value::Object(map)) {
        let _ = fs::write(sidecar_path(script_path), json);
    }
}

pub fn setting_value_to_json(v: &SettingValue) -> serde_json::Value {
    match v {
        SettingValue::Bool(b) => serde_json::Value::Bool(*b),
        SettingValue::Int(i) => serde_json::Value::from(*i),
        SettingValue::Float(f) => serde_json::Value::from(*f),
        SettingValue::Color([r, g, b]) => serde_json::json!({ "r": r, "g": g, "b": b }),
        SettingValue::String(s) => serde_json::Value::String(s.clone()),
        SettingValue::Selection(items) => serde_json::Value::from(items.clone()),
    }
}

// --- log history ----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    /// How many times this exact message repeated immediately in a row.
    pub count: u32,
}

/// A 60 fps frame, in milliseconds: the render-time budget.
const FRAME_BUDGET_MS: f32 = 1000.0 / 60.0;
/// How long a script's render time has to average over budget, without a
/// break, before it's dropped to 30 fps.
const SLOW_FOR: Duration = Duration::from_secs(3);
/// Minimum time between renders at the reduced rate: a 30 fps frame, less
/// a little slack so a repaint arriving slightly early still renders.
pub const HALF_RATE_INTERVAL: Duration = Duration::from_micros(29_300);

/// A script's render times: the last second's for the readout and the
/// 30 fps fallback, and the whole run's for `run-script`'s summary.
#[derive(Default)]
struct Perf {
    /// (when, milliseconds) for renders in the last second.
    recent: VecDeque<(Instant, f32)>,
    /// When the last-second average went over budget (cleared when it
    /// comes back under).
    slow_since: Option<Instant>,
    half_rate: bool,
    frames: u64,
    total_ms: f64,
    worst_ms: f32,
}

/// The numbers shown in the Debug window (and at the end of `run-script`).
#[derive(Clone, Copy, Debug, Default)]
pub struct PerfSummary {
    /// Average and worst render time over the last second, milliseconds.
    pub avg_ms: f32,
    pub max_ms: f32,
    /// Running at 30 fps because it was too slow for 60.
    pub half_rate: bool,
    /// Average and worst over everything rendered since the script loaded.
    pub overall_avg_ms: f32,
    pub overall_max_ms: f32,
    pub frames: u64,
}

impl Perf {
    /// Record one render. Returns a message the first time this pushes the
    /// script down to 30 fps.
    fn record(&mut self, now: Instant, ms: f32) -> Option<String> {
        self.frames += 1;
        self.total_ms += f64::from(ms);
        self.worst_ms = self.worst_ms.max(ms);
        self.recent.push_back((now, ms));
        while self.recent.front().is_some_and(|&(t, _)| now.duration_since(t) > Duration::from_secs(1)) {
            self.recent.pop_front();
        }
        let avg = self.recent.iter().map(|&(_, ms)| ms).sum::<f32>() / self.recent.len() as f32;
        if avg <= FRAME_BUDGET_MS {
            self.slow_since = None;
            return None;
        }
        let since = *self.slow_since.get_or_insert(now);
        if !self.half_rate && now.duration_since(since) >= SLOW_FOR {
            self.half_rate = true;
            return Some(format!(
                "render() has averaged {avg:.1} ms for {} s, over the {FRAME_BUDGET_MS:.1} ms a 60 fps frame allows: running this script at 30 fps",
                SLOW_FOR.as_secs()
            ));
        }
        None
    }

    fn summary(&self) -> PerfSummary {
        let n = self.recent.len().max(1) as f32;
        PerfSummary {
            avg_ms: self.recent.iter().map(|&(_, ms)| ms).sum::<f32>() / n,
            max_ms: self.recent.iter().map(|&(_, ms)| ms).fold(0.0, f32::max),
            half_rate: self.half_rate,
            overall_avg_ms: if self.frames == 0 { 0.0 } else { (self.total_ms / self.frames as f64) as f32 },
            overall_max_ms: self.worst_ms,
            frames: self.frames,
        }
    }
}

/// Longest `DT` a script ever sees, in seconds (see `render`).
const MAX_DT: f64 = 0.25;

const MAX_LOG_ENTRIES: usize = 200;
const MAX_LOG_MESSAGE_LEN: usize = 500;

/// A script's `log()` output plus its own runtime errors, in one
/// chronological, browser-console-style list: an entry identical to the most
/// recent one just bumps its count instead of repeating, so a `log()` called
/// every frame doesn't flood the pane. Lives inside [`Compiled`], fresh per
/// successful compile, so switching scripts starts from a clean slate.
#[derive(Default)]
struct LogHistory {
    entries: Vec<LogEntry>,
    /// Goes up with every change, so a copy is only needed when it moved.
    revision: u64,
    /// The "copy script logs to the app log" preference, shared by every
    /// compile (`LuaVisualizer::set_app_log`).
    to_app_log: Rc<Cell<bool>>,
    /// The script asked for that itself (`script_options({ app_log = true })`).
    script_wants_app_log: bool,
    /// The script's name, to label what's copied.
    name: String,
}

impl LogHistory {
    /// A `log()` message from the script, copied to the app's log too when
    /// the preference or the script says so, or for `log_app` (`always`).
    /// Repeats aren't copied: a script logging the same thing every frame
    /// would flood it.
    fn push_from_script(&mut self, message: String, always: bool) {
        if self.push(LogLevel::Info, message)
            && (always || self.to_app_log.get() || self.script_wants_app_log)
            && let Some(entry) = self.entries.last()
        {
            log::info!(target: "synththing::script", "{}: {}", self.name, entry.message);
        }
    }

    /// Returns whether it's a new entry (not a repeat of the last one).
    fn push(&mut self, level: LogLevel, mut message: String) -> bool {
        self.revision += 1;
        if message.len() > MAX_LOG_MESSAGE_LEN {
            message.truncate(MAX_LOG_MESSAGE_LEN);
            message.push_str("...");
        }
        if let Some(last) = self.entries.last_mut()
            && last.level == level
            && last.message == message
        {
            last.count += 1;
            return false;
        }
        self.entries.push(LogEntry { level, message, count: 1 });
        if self.entries.len() > MAX_LOG_ENTRIES {
            let excess = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(0..excess);
        }
        true
    }
}

// --- debug locals ---------------------------------------------------------
//
// `debug_locals([label])`, a script calls it by name at whatever specific
// point its author wants a snapshot from, e.g.
// `debug_locals("after collision check")` deep inside a loop, which is the
// thing a fixed "show me the module's top-level state" view can't do. It
// reads real Lua locals via the debug C API (lua_getstack/lua_getlocal on
// the calling frame), mlua's safe API has no equivalent, since locals
// (unlike globals) aren't a Lua *value* reachable through any table, just
// stack slots in a running frame.
//
// An earlier version of this also had an editor gutter with a clickable
// line marker that injected the call automatically, so you never had to
// type it, dropped (not just undocumented) after it turned out to be
// laggy (a button per source line adds up) and gutter/text alignment in
// egui's TextEdit is its own can of worms. Calling it by hand is the
// whole interface now.

/// One entry in a `debug_locals()` snapshot's variable tree.
#[derive(Clone, Debug)]
pub struct DebugVar {
    pub name: String,
    /// A short rendering of the value; for a table, "table (N entries)".
    pub value: String,
    pub children: Vec<DebugVar>,
}

/// `debug_locals()`'s most recent result: the label it was called with (if
/// any) plus the locals in scope at that call site.
#[derive(Clone, Debug, Default)]
pub struct DebugSnapshot {
    pub label: Option<String>,
    pub vars: Vec<DebugVar>,
}

/// The script's own state, for the Debug window (`state_snapshot`).
#[derive(Clone, Debug, Default)]
pub struct StateSnapshot {
    /// Its top-level `local`s, as its functions hold them (upvalues).
    pub locals: Vec<StateVar>,
    /// The globals it made (not the host's API or Lua's libraries).
    pub globals: Vec<StateVar>,
}

/// One variable, or one entry of an opened table, in a `StateSnapshot`.
#[derive(Clone, Debug, Default)]
pub struct StateVar {
    pub name: String,
    /// A short rendering of the value; for a table, "table (N entries)".
    pub value: String,
    /// Where it is, for opening it: `"locals"` or `"globals"`, its name,
    /// then each key down to it.
    pub path: Vec<String>,
    /// A table with something in it: it can be opened.
    pub expandable: bool,
    /// Its entries, if it's open (`StateRequest`), up to the number asked.
    pub children: Vec<StateVar>,
    /// Entries left out past that.
    pub more: usize,
}

/// What of the script's state to read: only the tables opened, and only so
/// many entries of each (more on request), so reading it costs what's on
/// screen, not what the script holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateRequest {
    /// Opened tables, by `StateVar::path`, and how many entries to show.
    pub open: HashMap<Vec<String>, usize>,
}

/// Entries an opened table shows at first, and how many more "+" adds.
pub const STATE_PAGE: usize = 50;
/// Tables are counted up to this many entries ("100000+ entries").
const MAX_STATE_COUNT: usize = 100_000;

/// Depth/breadth caps so a huge or self-referential table (snake.lua's
/// `occ`, say) can't make one `debug_locals()` call expensive or hang.
const MAX_DEBUG_DEPTH: usize = 4;
const MAX_DEBUG_ENTRIES: usize = 200;
/// And a cap on the values one `debug_locals()` call describes in all: past
/// it, tables are shown without their contents (a chart of thousands of
/// notes, say).
const MAX_DEBUG_VALUES: usize = 5_000;
/// How many of the script's functions a state snapshot looks inside for
/// upvalues.
const MAX_STATE_FUNCTIONS: usize = 500;
/// Lua caps a frame's visible locals well below this in practice; it's just
/// a hard backstop against ever looping on a malformed debug frame.
const MAX_DEBUG_LOCALS: std::ffi::c_int = 200;

/// Reads the locals of whatever Lua function is one frame up the call stack
/// from here, i.e. the script code that's in the middle of calling
/// `debug_locals()` right now. Safety: `exec_raw_lua` hands us the same
/// `lua_State` mlua itself is driving, from inside a callback mlua invoked
/// *from* that exact Lua call, so level 1 is guaranteed to exist and be a
/// Lua (not C) frame; `lua_getlocal`'s contract is to push the value it
/// names and return null once names run out, which is what the loop below
/// assumes.
fn capture_caller_locals(lua: &Lua) -> Vec<DebugVar> {
    lua.exec_raw_lua(|raw| unsafe {
        let state = raw.state();
        let mut ar: mlua::ffi::lua_Debug = std::mem::zeroed();
        if mlua::ffi::lua_getstack(state, 1, &mut ar) == 0 {
            return Vec::new();
        }

        let mut vars = Vec::new();
        let mut budget = MAX_DEBUG_VALUES;
        for n in 1..=MAX_DEBUG_LOCALS {
            let name_ptr = mlua::ffi::lua_getlocal(state, &ar, n);
            if name_ptr.is_null() {
                break;
            }
            let name = std::ffi::CStr::from_ptr(name_ptr).to_string_lossy().into_owned();
            // lua_getlocal already pushed the value; read (and pop) it
            // regardless of whether we keep it, to leave the stack as we
            // found it.
            let value: Value = raw.pop().unwrap_or(Value::Nil);
            // Lua names its own internal temporaries (loop control state
            // etc.) with a leading '(', not something the script wrote.
            if !name.starts_with('(') {
                vars.push(describe_value(&name, &value, 0, &mut budget));
            }
        }
        vars
    })
}

/// `value` as a row (and, for a table, its contents as rows under it),
/// counting each one against `budget`.
fn describe_value(name: &str, value: &Value, depth: usize, budget: &mut usize) -> DebugVar {
    *budget = budget.saturating_sub(1);
    if let Value::Table(_) = value
        && *budget == 0
    {
        return DebugVar { name: name.to_string(), value: "table (not expanded)".to_string(), children: Vec::new() };
    }
    if let Value::Table(table) = value
        && depth < MAX_DEBUG_DEPTH
    {
        let mut children = Vec::new();
        for pair in table.pairs::<Value, Value>() {
            let Ok((key, val)) = pair else { continue };
            if children.len() >= MAX_DEBUG_ENTRIES {
                children.push(DebugVar {
                    name: "...".to_string(),
                    value: "(truncated)".to_string(),
                    children: Vec::new(),
                });
                break;
            }
            children.push(describe_value(&describe_scalar(&key), &val, depth + 1, budget));
        }
        return DebugVar { name: name.to_string(), value: format!("table ({} entries)", children.len()), children };
    }
    DebugVar { name: name.to_string(), value: describe_scalar(value), children: Vec::new() }
}

/// The upvalues of `function` (name and value), `_ENV` left out; none for
/// a Rust function. Safety: as `capture_caller_locals`, on mlua's own
/// state; the function is pushed, each `lua_getupvalue` pushes one value
/// that's popped straight away, and the function is popped at the end.
fn upvalues(lua: &Lua, function: &Function) -> Vec<(String, Value)> {
    lua.exec_raw_lua(|raw| unsafe {
        let state = raw.state();
        if mlua::ffi::lua_checkstack(state, 3) == 0 || raw.push(function.clone()).is_err() {
            return Vec::new();
        }
        let mut found = Vec::new();
        if mlua::ffi::lua_iscfunction(state, -1) == 0 {
            for n in 1..=255 {
                let name_ptr = mlua::ffi::lua_getupvalue(state, -1, n);
                if name_ptr.is_null() {
                    break;
                }
                let name = std::ffi::CStr::from_ptr(name_ptr).to_string_lossy().into_owned();
                let value: Value = raw.pop().unwrap_or(Value::Nil);
                if !name.is_empty() && name != "_ENV" {
                    found.push((name, value));
                }
            }
        }
        mlua::ffi::lua_pop(state, 1);
        found
    })
}

/// The script's state: the globals it made (anything not in
/// `host_globals`), and its top-level locals, found as the upvalues of its
/// functions (theirs, and those of the functions they hold, and so on).
/// Functions themselves aren't listed, only looked inside. Tables are only
/// read into as far as `request` opens them.
fn script_state(lua: &Lua, host_globals: &HashSet<String>, request: &StateRequest) -> StateSnapshot {
    let mut globals = Vec::new();
    let mut queue = Vec::new();
    let mut own: Vec<(String, Value)> = lua
        .globals()
        .pairs::<Value, Value>()
        .filter_map(Result::ok)
        .filter_map(|(key, value)| match key {
            Value::String(name) => Some((name.to_string_lossy(), value)),
            _ => None,
        })
        .filter(|(name, _)| !host_globals.contains(name))
        .collect();
    own.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in own {
        match value {
            Value::Function(f) => queue.push(f),
            value => globals.push(describe_state(&name, &value, vec!["globals".into(), name.clone()], request)),
        }
    }
    let mut seen_functions = HashSet::new();
    let mut found: Vec<(String, Value)> = Vec::new();
    let mut names = HashSet::new();
    let mut next = 0;
    while next < queue.len() && seen_functions.len() < MAX_STATE_FUNCTIONS {
        let function = queue[next].clone();
        next += 1;
        if !seen_functions.insert(function.to_pointer()) {
            continue;
        }
        for (name, value) in upvalues(lua, &function) {
            match value {
                Value::Function(f) => queue.push(f),
                value => {
                    if names.insert(name.clone()) {
                        found.push((name, value));
                    }
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let locals = found
        .iter()
        .map(|(name, value)| describe_state(name, value, vec!["locals".into(), name.clone()], request))
        .collect();
    StateSnapshot { locals, globals }
}

/// One variable for a `StateSnapshot`: a table's entries only if
/// `request` has it open, in key order (numbers first), as many as asked.
fn describe_state(name: &str, value: &Value, path: Vec<String>, request: &StateRequest) -> StateVar {
    let Value::Table(table) = value else {
        return StateVar { name: name.to_string(), value: describe_scalar(value), path, ..StateVar::default() };
    };
    let count = table.pairs::<Value, Value>().take(MAX_STATE_COUNT).count();
    let shown = if count == MAX_STATE_COUNT { format!("table ({MAX_STATE_COUNT}+ entries)") } else { format!("table ({count} entries)") };
    let mut var = StateVar { name: name.to_string(), value: shown, path, expandable: count > 0, ..StateVar::default() };
    let Some(&limit) = request.open.get(&var.path) else { return var };
    let mut entries: Vec<(Value, Value)> = table.pairs::<Value, Value>().filter_map(Result::ok).collect();
    entries.sort_by(|(a, _), (b, _)| state_key_order(a, b));
    var.more = entries.len().saturating_sub(limit);
    var.children = entries
        .iter()
        .take(limit)
        .map(|(key, value)| {
            // (The path tells "1" from 1; the name reads plainly.)
            let mut path = var.path.clone();
            path.push(describe_scalar(key));
            let name = match key {
                Value::String(s) => s.to_string_lossy(),
                other => describe_scalar(other),
            };
            describe_state(&name, value, path, request)
        })
        .collect();
    var
}

/// Table keys in a readable order: numbers (by value), then everything
/// else by how it reads.
fn state_key_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    let number = |v: &Value| match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    };
    match (number(a), number(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => describe_scalar(a).cmp(&describe_scalar(b)),
    }
}

fn describe_scalar(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("{:?}", s.to_string_lossy()),
        Value::Function(_) => "<function>".to_string(),
        Value::Table(_) => "<table>".to_string(), // only hit past MAX_DEBUG_DEPTH
        _ => "<value>".to_string(),
    }
}

/// Everything tied to one successfully-compiled script: its own Lua VM (a
/// fresh one per reload, so stale globals from a previous version of the
/// script can't linger), the `render` function, the draw-command queue its
/// draw calls feed, and its settings/log state, all reset together on
/// reload, since they're meaningless carried over to a different script.
/// This frame's audio levels and onset, computed in Rust before the
/// script runs (`level_left`, `level_right`, `onset`).
#[derive(Default)]
struct AudioFeatures {
    level_left: f32,
    level_right: f32,
    onset: bool,
    onset_strength: f32,
    /// Set the first time a script calls `onset()`: from then on the
    /// detector runs every frame (it needs a running history), and not
    /// before, so scripts that don't use it don't pay for it.
    onset_wanted: bool,
}

/// Which song is loaded, for `playback()`.
#[derive(Clone, Default)]
struct SongIdentity {
    path: Option<String>,
    id: Option<String>,
    /// The app's playlist and transport modes, refreshed each frame.
    transport: Transport,
    /// Songs loaded so far (`playback().song_loads`), the same one again
    /// included.
    loads: u64,
    /// The visualizer is being recorded to a video (`playback().recording`).
    recording: bool,
    /// The recording the script is part of (`recording()`).
    take: Option<RecordingTake>,
}

/// What a recording asks of the script (`recording()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingTake {
    pub phase: RecordPhase,
    pub kind: RecordKind,
    /// The script plays itself ("auto"), rather than being played
    /// ("manual"). Only ever true for a script with `record_auto`.
    pub auto: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordPhase {
    /// Nothing's going into the video yet: the script gets ready (with
    /// `record_prepare`) or the song is starting.
    Preparing,
    Recording,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Song,
    Playlist,
    Free,
}

impl RecordPhase {
    fn name(self) -> &'static str {
        match self {
            RecordPhase::Preparing => "preparing",
            RecordPhase::Recording => "recording",
        }
    }
}

impl RecordKind {
    fn name(self) -> &'static str {
        match self {
            RecordKind::Song => "song",
            RecordKind::Playlist => "playlist",
            RecordKind::Free => "free",
        }
    }
}

/// How a script asks the app to behave while it's running
/// (`script_options`). Set again on every compile or restart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptOptions {
    /// Songs load paused at the start, for the script to start them.
    pub start_paused: bool,
    /// Its `log()` messages are copied to the app's log, whatever the
    /// preference says.
    pub app_log: bool,
    /// A recording waits for it to say it's ready (`recording_ready`)
    /// before each song, and for it to say the take is over
    /// (`recording_done`).
    pub record_prepare: bool,
    /// It can play itself, for recording (the Record window offers Auto).
    pub record_auto: bool,
}

/// The `script_options` keys, for the error naming them.
const SCRIPT_OPTIONS: &str = "start_paused, app_log, record_prepare and record_auto";

/// What a script can see of the app's playback beyond the engine: the
/// loop and shuffle modes, and the current playlist (read-only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transport {
    pub loop_mode: LoopMode,
    pub shuffle: bool,
    pub playlist: Option<PlaylistView>,
}

/// The playlist that's playing, or else the one open in the Playlists tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistView {
    pub name: String,
    /// Whether it's the one playing (rather than just open).
    pub playing: bool,
    /// The entry playing, 0-based.
    pub current: Option<usize>,
    pub entries: Vec<PlaylistEntryView>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistEntryView {
    pub name: String,
    pub length: Option<f64>,
    pub missing: bool,
}

/// Playback changes a script asked for (`set_paused`, `seek`, ...),
/// carried out by the app after the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaybackRequest {
    Pause(bool),
    Seek(f64),
    /// Play this entry (0-based) of the playlist `playlist()` shows.
    PlayTrack(usize),
    NextTrack,
    PreviousTrack,
    Speed(f64),
    Loop(LoopMode),
    Shuffle(bool),
    /// `recording_ready()`: done preparing, record from here.
    RecordingReady,
    /// `recording_done()`: this song's take is over.
    RecordingDone,
}

/// Saved data a script owns (`store_get`/`store_set`), in
/// `<script>.lua.store.json`. Written when changed, at most about once a
/// second, plus whenever the script is replaced or the app closes.
struct ScriptStore {
    /// Where it's saved; `None` for a script with no file (in-memory only).
    path: Option<PathBuf>,
    values: serde_json::Map<String, serde_json::Value>,
    dirty: bool,
    last_saved: Instant,
}

/// How often a changing store is written out, at most.
const STORE_SAVE_INTERVAL: Duration = Duration::from_secs(1);

impl ScriptStore {
    fn load(script: Option<&Path>) -> Self {
        let path = script.map(store_path);
        let values = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, values, dirty: false, last_saved: Instant::now() }
    }

    fn save(&mut self, force: bool) {
        if !self.dirty || (!force && self.last_saved.elapsed() < STORE_SAVE_INTERVAL) {
            return;
        }
        self.dirty = false;
        self.last_saved = Instant::now();
        let Some(path) = &self.path else {
            return;
        };
        let result = serde_json::to_string_pretty(&self.values)
            .map_err(anyhow::Error::from)
            .and_then(|json| fs::write(path, json).map_err(Into::into));
        if let Err(e) = result {
            log::warn!("couldn't save script data to {}: {e}", path.display());
        }
    }
}

/// `<script>.lua.store.json`, next to the script.
fn store_path(script_path: &Path) -> PathBuf {
    let mut name = script_path.as_os_str().to_os_string();
    name.push(".store.json");
    PathBuf::from(name)
}

/// `<script>.lua.controls.json`: the user's key bindings for the script's
/// actions, by action name.
fn controls_path(script_path: &Path) -> PathBuf {
    let mut name = script_path.as_os_str().to_os_string();
    name.push(".controls.json");
    PathBuf::from(name)
}

/// An input action a script registered (`input_register`): a name, the
/// keys it starts with, and the keys bound to it now.
#[derive(Clone, Debug)]
pub struct Action {
    pub name: String,
    pub defaults: Vec<Binding>,
    pub bindings: Vec<Binding>,
    pub labels: ItemLabels,
}

/// What an action can be bound to: a key, or a controller input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    Key(egui::Key),
    Pad(PadInput),
    /// A mouse button over the visualizer: 0 left, 1 right, 2 middle (as
    /// `MOUSE_BUTTONS`).
    Mouse(u8),
}

/// The names of the mouse buttons as bindings, in `MOUSE_BUTTONS` order.
const MOUSE_BINDINGS: [&str; 3] = ["mouse_left", "mouse_right", "mouse_middle"];

impl Binding {
    /// The name scripts and the saved controls use: `"space"`, `"pad_a"`,
    /// `"mouse_left"`.
    pub fn name(self) -> &'static str {
        match self {
            Binding::Key(key) => key.name(),
            Binding::Pad(pad) => pad.name(),
            Binding::Mouse(button) => MOUSE_BINDINGS[usize::from(button).min(2)],
        }
    }

    /// How the Controls list shows it.
    pub fn label(self) -> &'static str {
        match self {
            Binding::Key(key) => key.name(),
            Binding::Pad(pad) => pad.label(),
            Binding::Mouse(button) => ["Mouse left", "Mouse right", "Mouse middle"][usize::from(button).min(2)],
        }
    }
}

/// A key, controller input or mouse button by name (`"a"`, `"space"`,
/// `"pad_a"`, `"pad_lstick_left"`, `"mouse_left"`). `Ok(None)` for a key
/// the app keeps for itself.
fn parse_binding(name: &str) -> mlua::Result<Option<Binding>> {
    if name.to_ascii_lowercase().starts_with("mouse_") {
        return MOUSE_BINDINGS
            .iter()
            .position(|m| m.eq_ignore_ascii_case(name))
            .map(|i| Some(Binding::Mouse(i as u8)))
            .ok_or_else(|| {
                mlua::Error::runtime(format!(
                    "unknown mouse button '{name}' (there's \"mouse_left\", \"mouse_right\", \"mouse_middle\")"
                ))
            });
    }
    if name.to_ascii_lowercase().starts_with("pad_") {
        return PadInput::from_name(name).map(|p| Some(Binding::Pad(p))).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "unknown controller input '{name}' (try \"pad_a\", \"pad_dpad_up\", \"pad_lstick_left\", \"pad_start\")"
            ))
        });
    }
    Ok(parse_key(name)?.map(Binding::Key))
}

/// A script's actions, plus the bindings saved for it (kept even for
/// actions the current code doesn't register, so they come back if it
/// does again).
struct Controls {
    actions: Vec<Action>,
    path: Option<PathBuf>,
    saved: serde_json::Map<String, serde_json::Value>,
}

impl Controls {
    fn load(script: Option<&Path>) -> Self {
        let path = script.map(controls_path);
        let saved = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { actions: Vec::new(), path, saved }
    }

    /// The bindings saved for `name`, if any (unknown key names dropped).
    fn saved_bindings(&self, name: &str) -> Option<Vec<Binding>> {
        let list = self.saved.get(name)?.as_array()?;
        Some(list.iter().filter_map(|v| v.as_str()).filter_map(|k| parse_binding(k).ok().flatten()).collect())
    }

    fn save(&mut self) {
        for action in &self.actions {
            let names: Vec<serde_json::Value> =
                action.bindings.iter().map(|k| serde_json::Value::from(k.name())).collect();
            self.saved.insert(action.name.clone(), serde_json::Value::Array(names));
        }
        let Some(path) = &self.path else { return };
        let result = serde_json::to_string_pretty(&self.saved)
            .map_err(anyhow::Error::from)
            .and_then(|json| fs::write(path, json).map_err(Into::into));
        if let Err(e) = result {
            log::warn!("couldn't save controls to {}: {e}", path.display());
        }
    }
}

/// How far an action bound to `bindings` is pressed, 0 to 1: the most any
/// of them is (keys 0 or 1, triggers and stick directions how far).
fn action_value(input: &VisualizerInput, bindings: &[Binding]) -> f32 {
    bindings
        .iter()
        .map(|b| match b {
            Binding::Key(k) => {
                if input.keys_down.contains(k) {
                    1.0
                } else {
                    0.0
                }
            }
            Binding::Pad(p) => input.pads.value(*p),
            Binding::Mouse(m) => {
                if input.buttons_down.get(usize::from(*m)).copied().unwrap_or(false) {
                    1.0
                } else {
                    0.0
                }
            }
        })
        .fold(0.0, f32::max)
}

/// "pressed" (went down this frame), "held", "released" (went up this
/// frame) or "up", for an action bound to `bindings`.
fn action_state(input: &VisualizerInput, bindings: &[Binding]) -> &'static str {
    let any = |keys: &[egui::Key], pads: &[PadInput], mouse: &[bool; 3]| {
        bindings.iter().any(|b| match b {
            Binding::Key(k) => keys.contains(k),
            Binding::Pad(p) => pads.contains(p),
            Binding::Mouse(m) => mouse.get(usize::from(*m)).copied().unwrap_or(false),
        })
    };
    if any(&input.keys_pressed, &input.pads.pressed, &input.buttons_pressed) {
        "pressed"
    } else if any(&input.keys_down, &input.pads.down, &input.buttons_down) {
        "held"
    } else if any(&input.keys_released, &input.pads.released, &input.buttons_released) {
        "released"
    } else {
        "up"
    }
}

struct Compiled {
    /// The source this was compiled from, so `restart` can run the same
    /// script again even after a newer edit failed to compile.
    source: String,
    /// The watchdog's deadline for the code running now (see `compile`).
    deadline: Rc<Cell<Option<Instant>>>,
    /// The watchdog stopped `render()`: it isn't called again until the
    /// script is changed or restarted, or every frame would freeze too.
    stopped: Cell<bool>,
    /// What it asked for with `script_options`.
    options: Rc<Cell<ScriptOptions>>,
    lua: Lua,
    render: Function,
    commands: Rc<RefCell<Vec<DrawCommand>>>,
    settings: Rc<RefCell<SettingsStore>>,
    log: Rc<RefCell<LogHistory>>,
    /// `(channel, enabled)` requests queued by `set_channel_enabled`, drained
    /// by the host once per frame and turned into real `AudioCommand`s, the
    /// script can't touch the engine directly, just ask.
    channel_requests: Rc<RefCell<Vec<(u8, bool)>>>,
    /// The most recent `debug_locals()` snapshot: an optional label plus
    /// whatever was in scope at that call site. `None` until the script
    /// calls it at least once.
    debug_snapshot: Rc<RefCell<Option<DebugSnapshot>>>,
    /// What the script asked for via `set_cursor_visible`/`set_cursor_locked`.
    /// Per script: a newly loaded one starts with a normal cursor.
    cursor: Rc<RefCell<CursorRequest>>,
    playback_requests: Rc<RefCell<Vec<PlaybackRequest>>>,
    store: Rc<RefCell<ScriptStore>>,
    /// Sprites the script registered (`sprite_register`), per script.
    sprites: Rc<RefCell<Vec<Sprite>>>,
    /// Input actions (`input_register`) and their bindings.
    controls: Rc<RefCell<Controls>>,
    /// The typing span (`typing_begin` ... `typing_end`).
    typing: Rc<RefCell<TypingSpan>>,
    /// Frames rendered since this script started (`FRAME`).
    frame: Cell<u64>,
    /// `TIME`: seconds since its first frame, as of the last render.
    time: Cell<f64>,
    /// The globals there before the script ran (the host's API, Lua's
    /// libraries), so its own can be told apart (`state_snapshot`).
    host_globals: HashSet<String>,
}

impl Drop for Compiled {
    /// A script being replaced (or the app closing) saves its store.
    fn drop(&mut self) {
        self.store.borrow_mut().save(true);
    }
}

pub struct LuaVisualizer {
    sample_rate: u32,
    path: Option<PathBuf>,
    source: String,
    compiled: Option<Compiled>,
    /// Set when the most recent `set_source` call failed (to compile, or,
    /// from `set_source_after_save`, to write to disk); cleared the next time
    /// a `set_source` call succeeds outright. Deliberately independent of
    /// `runtime_error`: the old script kept running after a bad edit might
    /// go on rendering perfectly fine, and a successful frame from it
    /// shouldn't make the bad edit's error vanish before it's even been
    /// read, that was the actual bug, not just overly-transient UI.
    compile_error: Option<String>,
    /// Set from the outcome of the most recent `render` call; cleared the
    /// next time one succeeds. This one *is* meant to be that transient:
    /// it reflects whether the script is erroring right now.
    runtime_error: Option<String>,
    spectrum_left: Rc<RefCell<SpectrumAnalyzer>>,
    spectrum_right: Rc<RefCell<SpectrumAnalyzer>>,
    /// The current frame's note info, shared with the `active_notes()` /
    /// `upcoming_notes()` Lua functions.
    notes: Rc<RefCell<NotesSnapshot>>,
    /// The current frame's transport state, shared with `playback()`.
    /// Ground truth, not script-specific, unlike `settings`/`log`, it
    /// persists across recompiles rather than resetting per script.
    playback: Rc<RefCell<EngineView>>,
    /// The current frame's mouse/keyboard input, shared with the input
    /// functions. Ground truth like `playback`, so it outlives recompiles.
    input: Rc<RefCell<VisualizerInput>>,
    /// Every note of the current MIDI file (empty for plain audio), for
    /// `notes_between`. Song data, so it outlives recompiles.
    note_list: Rc<RefCell<Arc<NoteList>>>,
    /// The current song's path and id, for `playback()`. Song data, so it
    /// outlives recompiles.
    song: Rc<RefCell<SongIdentity>>,
    /// Notes the script plays (`play_note`, sequences), for the app to pass
    /// to the live synth. Outlives recompiles, so the "stop everything"
    /// a new compile queues still gets delivered.
    live: Rc<RefCell<LiveQueue>>,
    features: Rc<RefCell<AudioFeatures>>,
    onset_detector: OnsetDetector,
    last_render_instant: Option<Instant>,
    /// Seconds per frame for DT and TIME instead of the wall clock: set
    /// for headless runs, which render frames much faster than real time.
    fixed_timestep: Option<f64>,
    /// Render times, and the 30 fps fallback. Kept through Restart and song
    /// changes; reset when the code changes or another script loads.
    perf: Perf,
    /// Set from anywhere (another thread) to stop the script's code running
    /// now, as the watchdog would. Whoever sets it clears it.
    stop: Arc<AtomicBool>,
    /// How long the watchdog lets one run of the script's code take.
    watchdog_limit: Duration,
    /// Goes up with every compile and restart (a fresh log), for
    /// `log_revision`.
    compiles: u64,
    /// The "copy script logs to the app log" preference.
    app_log: Rc<Cell<bool>>,
}

impl LuaVisualizer {
    pub fn new(source: String, path: Option<PathBuf>, sample_rate: u32) -> Self {
        let mut visualizer = Self {
            sample_rate,
            path,
            source: String::new(),
            compiled: None,
            compile_error: None,
            runtime_error: None,
            spectrum_left: Rc::new(RefCell::new(SpectrumAnalyzer::new())),
            spectrum_right: Rc::new(RefCell::new(SpectrumAnalyzer::new())),
            notes: Rc::new(RefCell::new(NotesSnapshot::default())),
            playback: Rc::new(RefCell::new(EngineView::default())),
            input: Rc::new(RefCell::new(VisualizerInput::default())),
            note_list: Rc::new(RefCell::new(Arc::default())),
            song: Rc::default(),
            live: Rc::default(),
            features: Rc::default(),
            onset_detector: OnsetDetector::new(sample_rate),
            last_render_instant: None,
            fixed_timestep: None,
            perf: Perf::default(),
            stop: Arc::default(),
            watchdog_limit: WATCHDOG_LIMIT,
            compiles: 0,
            app_log: Rc::default(),
        };
        visualizer.set_source(source);
        visualizer
    }

    /// The same, with a watchdog limit other than `WATCHDOG_LIMIT` from the
    /// start (the script's top level runs right away).
    pub fn with_watchdog(source: String, path: Option<PathBuf>, sample_rate: u32, limit: Duration) -> Self {
        let mut visualizer = Self::new(String::new(), path, sample_rate);
        visualizer.watchdog_limit = limit;
        visualizer.set_source(source);
        visualizer
    }

    /// Copy the script's `log()` messages to the app's log (the
    /// preference; a script can also ask for it with `script_options`).
    pub fn set_app_log(&mut self, on: bool) {
        self.app_log.set(on);
    }

    /// The flag that stops the script's code running now (see `stop`).
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Whether a script is running (it compiled at some point).
    pub fn is_running(&self) -> bool {
        self.compiled.is_some()
    }

    /// Changes whenever the log does (`log_entries`).
    pub fn log_revision(&self) -> (u64, u64) {
        (self.compiles, self.compiled.as_ref().map_or(0, |c| c.log.borrow().revision))
    }


    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }

    /// The running script's file was renamed to `path` (its sidecars already
    /// moved along with it): point everything, the script's store included,
    /// at the new name. Call `flush_store` before renaming the files.
    pub fn renamed_to(&mut self, path: PathBuf) {
        if let Some(compiled) = &self.compiled {
            compiled.store.borrow_mut().path = Some(store_path(&path));
            compiled.controls.borrow_mut().path = Some(controls_path(&path));
        }
        self.path = Some(path);
    }

    /// Frames are `step` seconds apart (for DT and TIME) rather than timed
    /// by the wall clock; `None` for real time.
    pub fn set_fixed_timestep(&mut self, step: Option<f64>) {
        self.fixed_timestep = step;
    }

    /// Keep the running script's store in memory only: it still reads
    /// what's saved, but writes nothing (headless test runs).
    pub fn detach_store(&mut self) {
        if let Some(compiled) = &self.compiled {
            compiled.store.borrow_mut().path = None;
        }
    }

    /// Write the running script's store out now if it has unsaved changes.
    pub fn flush_store(&mut self) {
        if let Some(compiled) = &self.compiled {
            compiled.store.borrow_mut().save(true);
        }
    }

    /// The current song's file and id (`loader::song_id`), for
    /// `playback().song_path` / `song_id`.
    pub fn set_song(&mut self, path: Option<&Path>, id: Option<String>) {
        let mut song = self.song.borrow_mut();
        song.path = path.map(|p| p.display().to_string());
        song.id = id;
        song.loads += 1;
    }

    /// Whether the visualizer is being recorded (`playback().recording`),
    /// and the recording it's part of (`recording()`).
    pub fn set_recording(&mut self, recording: bool, take: Option<RecordingTake>) {
        let mut song = self.song.borrow_mut();
        song.recording = recording;
        song.take = take;
    }

    /// What the running script asked for with `script_options`.
    pub fn options(&self) -> ScriptOptions {
        self.compiled.as_ref().map(|c| c.options.get()).unwrap_or_default()
    }

    /// The app's loop/shuffle modes and current playlist, for `playlist()`
    /// and `playback()`. Call every frame before rendering.
    pub fn set_transport(&mut self, transport: Transport) {
        let mut song = self.song.borrow_mut();
        if song.transport != transport {
            song.transport = transport;
        }
    }

    /// The current song's notes, for `notes_between` (empty for audio).
    pub fn set_note_list(&mut self, notes: Arc<NoteList>) {
        *self.note_list.borrow_mut() = notes;
    }

    /// Notes the script played since the last call, oldest first, for the
    /// app to send to the live synth.
    pub fn take_live_commands(&mut self) -> Vec<LiveCommand> {
        std::mem::take(&mut self.live.borrow_mut().commands)
    }

    /// Playback changes the script asked for since the last call, oldest
    /// first, for the app to carry out.
    pub fn take_playback_requests(&mut self) -> Vec<PlaybackRequest> {
        let Some(compiled) = &self.compiled else { return Vec::new() };
        compiled.playback_requests.borrow_mut().drain(..).collect()
    }

    /// The error to show right now: a standing compile/save error from the
    /// last edit takes priority (it won't clear on its own, see
    /// `compile_error`'s docs), falling back to the current frame's runtime
    /// error if there isn't one. `None` means the running script is healthy.
    pub fn error(&self) -> Option<&str> {
        self.compile_error.as_deref().or(self.runtime_error.as_deref())
    }

    /// The active script's registered settings, in declaration order.
    pub fn settings(&self) -> Vec<SettingDescriptor> {
        self.compiled.as_ref().map(|c| c.settings.borrow().descriptors.clone()).unwrap_or_default()
    }

    /// The presets the script offers (`settings_preset`).
    pub fn presets(&self) -> Vec<SettingsPreset> {
        self.compiled.as_ref().map(|c| c.settings.borrow().presets.clone()).unwrap_or_default()
    }

    /// Apply a value edited through the Settings window and persist it to the
    /// script's sidecar file (if it has one, a script with no path yet,
    /// e.g. mid-creation, just keeps the change in memory).
    pub fn set_setting(&mut self, key: &str, value: SettingValue) {
        let Some(compiled) = &self.compiled else { return };
        compiled.settings.borrow_mut().set(key, value);
        if let Some(path) = &self.path {
            save_sidecar(path, &compiled.settings.borrow());
        }
    }

    /// The active script's log + runtime-error history, oldest first.
    pub fn log_entries(&self) -> Vec<LogEntry> {
        self.compiled.as_ref().map(|c| c.log.borrow().entries.clone()).unwrap_or_default()
    }

    pub fn clear_log(&mut self) {
        if let Some(compiled) = &self.compiled {
            let mut log = compiled.log.borrow_mut();
            log.entries.clear();
            log.revision += 1;
        }
    }

    /// The most recent `debug_locals()` snapshot, if the script has ever
    /// called it since the last compile.
    pub fn debug_snapshot(&self) -> Option<DebugSnapshot> {
        self.compiled.as_ref().and_then(|c| c.debug_snapshot.borrow().clone())
    }

    /// The script's state as of now: its top-level locals and its own
    /// globals (the Debug window's Variables), with the tables `request`
    /// opens. `None` with no script running.
    pub fn state_snapshot(&self, request: &StateRequest) -> Option<StateSnapshot> {
        self.compiled.as_ref().map(|c| script_state(&c.lua, &c.host_globals, request))
    }

    /// Channel mute/unmute requests the script made via `set_channel_enabled`
    /// since the last call, oldest first. The caller (the GUI) is expected to
    /// turn each into an `AudioCommand::SetChannelEnabled`.
    pub fn take_channel_requests(&mut self) -> Vec<(u8, bool)> {
        let Some(compiled) = &self.compiled else { return Vec::new() };
        compiled.channel_requests.borrow_mut().drain(..).collect()
    }

    /// Recompile against `new_source` on a fresh Lua VM. On success the new
    /// script takes over immediately; on failure the error is recorded (and
    /// also logged against the still-running old script, if there is one:
    /// "your last edit didn't take" is as much a fact about that script's
    /// history as a runtime error would be) and whatever script was
    /// previously running keeps running.
    pub fn set_source(&mut self, new_source: String) {
        self.source = new_source;
        let pending_settings = load_pending_settings(self.path.as_deref());
        match compile(
            &self.source,
            self.sample_rate,
            &self.spectrum_left,
            &self.spectrum_right,
            &self.notes,
            &self.playback,
            &self.song,
            &self.input,
            &self.note_list,
            &self.live,
            &self.features,
            self.path.as_deref(),
            pending_settings,
            &self.stop,
            self.watchdog_limit,
            &self.app_log,
        ) {
            Ok(compiled) => {
                self.compiled = Some(compiled);
                self.compiles += 1;
                self.live.borrow_mut().stop_all();
                self.compile_error = None;
                // New code (or another script): judge its speed afresh.
                self.perf = Perf::default();
            }
            Err(e) => {
                if let Some(compiled) = &self.compiled {
                    compiled.log.borrow_mut().push(LogLevel::Error, format!("compile error: {e}"));
                }
                self.compile_error = Some(e);
            }
        }
    }

    /// The script's input actions and their bindings, for the Controls list.
    pub fn actions(&self) -> Vec<Action> {
        self.compiled.as_ref().map(|c| c.controls.borrow().actions.clone()).unwrap_or_default()
    }

    /// Rebind action `index` (from `actions()`) and save it.
    pub fn set_action_bindings(&mut self, index: usize, bindings: Vec<Binding>) {
        if let Some(compiled) = &self.compiled {
            let mut controls = compiled.controls.borrow_mut();
            if let Some(action) = controls.actions.get_mut(index) {
                action.bindings = bindings;
                controls.save();
            }
        }
    }

    /// The script's render times and frame rate, for the readout.
    pub fn perf_summary(&self) -> PerfSummary {
        self.perf.summary()
    }

    /// Back to 60 fps after the fallback kicked in ("Try 60 fps again"). If
    /// it's still too slow it drops again after another few seconds.
    pub fn retry_full_frame_rate(&mut self) {
        self.perf.half_rate = false;
        self.perf.slow_since = None;
        self.perf.recent.clear();
    }

    /// Start the running script over from scratch: a fresh Lua VM running
    /// the same source, settings re-read from its sidecar, all of its own
    /// state (history, scores, ...) gone. Restarts what's actually running,
    /// not the latest edit, which might not compile; an error from the
    /// latest edit stays reported. Returns false if nothing is running.
    pub fn restart(&mut self) -> bool {
        let Some(source) = self.compiled.as_ref().map(|c| c.source.clone()) else {
            return false;
        };
        let pending_settings = load_pending_settings(self.path.as_deref());
        match compile(
            &source,
            self.sample_rate,
            &self.spectrum_left,
            &self.spectrum_right,
            &self.notes,
            &self.playback,
            &self.song,
            &self.input,
            &self.note_list,
            &self.live,
            &self.features,
            self.path.as_deref(),
            pending_settings,
            &self.stop,
            self.watchdog_limit,
            &self.app_log,
        ) {
            Ok(compiled) => {
                self.compiled = Some(compiled);
                self.compiles += 1;
                self.live.borrow_mut().stop_all();
                self.runtime_error = None;
                self.last_render_instant = None;
                true
            }
            // Can't really happen (it compiled before), but if it does, the
            // old VM just keeps running.
            Err(e) => {
                if let Some(compiled) = &self.compiled {
                    compiled.log.borrow_mut().push(LogLevel::Error, format!("restart failed: {e}"));
                }
                false
            }
        }
    }

    /// Apply `new_source`, which the caller has already tried writing to
    /// the script's file (`save_script`); `save_error` is how that went,
    /// shown along with any compile error.
    pub fn set_source_after_save(&mut self, new_source: String, save_error: Option<String>) {
        self.set_source(new_source);
        // Computed before `set_source` (which would otherwise overwrite it
        // the moment the script itself compiles fine) and reapplied after,
        // merged with whatever compile error that call left behind.
        if let Some(save_error) = save_error {
            self.compile_error = Some(match self.compile_error.take() {
                Some(compile_error) => format!("{save_error}\n{compile_error}"),
                None => save_error,
            });
        }
    }
}

impl Visualizer for LuaVisualizer {
    fn render(
        &mut self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        samples: &[StereoFrame],
        notes: &NotesSnapshot,
        playback: &EngineView,
        input: &VisualizerInput,
    ) {
        let render_began = Instant::now();
        buffer.fill(0);
        *self.notes.borrow_mut() = notes.clone();
        *self.playback.borrow_mut() = playback.clone();
        *self.input.borrow_mut() = input.clone();
        if let Some(compiled) = &self.compiled {
            compiled.typing.borrow_mut().update(&input.text_events, input.focused);
        }

        let now = Instant::now();
        // Capped, so a stretch of not rendering at all (Visualizer tab
        // hidden, Preferences open) reads as one slow frame, not a jump.
        // With a fixed timestep (headless runs), every frame is exactly
        // that long instead.
        let (dt, advance) = match (self.last_render_instant, self.fixed_timestep) {
            (None, _) => (0.0, 0.0),
            (Some(_), Some(step)) => (step, step),
            (Some(prev), None) => {
                let real = now.duration_since(prev).as_secs_f64();
                (real.min(MAX_DT), real)
            }
        };
        self.last_render_instant = Some(now);

        let Some(compiled) = &self.compiled else {
            return;
        };
        let _ = compiled.lua.globals().set("DT", dt);
        compiled.frame.set(compiled.frame.get() + 1);
        // TIME adds up each frame's length (real, uncapped; or the fixed
        // step), so it stays continuous when the timestep switches (a
        // recording starting or stopping).
        let time = if compiled.frame.get() == 1 { 0.0 } else { compiled.time.get() + advance };
        compiled.time.set(time);
        let _ = compiled.lua.globals().set("FRAME", compiled.frame.get());
        let _ = compiled.lua.globals().set("TIME", time);

        let left: Vec<f32> = samples.iter().map(|&(l, _)| l).collect();
        let right: Vec<f32> = samples.iter().map(|&(_, r)| r).collect();
        {
            let mut features = self.features.borrow_mut();
            features.level_left = spectrum::rms(&left);
            features.level_right = spectrum::rms(&right);
            if features.onset_wanted {
                let mono: Vec<f32> = samples.iter().map(|&(l, r)| (l + r) * 0.5).collect();
                (features.onset, features.onset_strength) = self.onset_detector.update(&mono);
            }
        }

        let build_tables = (|| -> mlua::Result<(Table, Table)> {
            Ok((
                vec_to_table(&compiled.lua, &left)?,
                vec_to_table(&compiled.lua, &right)?,
            ))
        })();

        if compiled.stopped.get() {
            return; // stopped by the watchdog: the error stays up
        }
        compiled.deadline.set(Some(Instant::now() + self.watchdog_limit));
        let outcome: mlua::Result<()> = match build_tables {
            Ok((left_table, right_table)) => compiled
                .render
                .call((width, height, left_table, right_table)),
            Err(e) => Err(e),
        };
        compiled.deadline.set(None);

        self.runtime_error = outcome.err().map(|e| e.to_string());
        if let Some(e) = &self.runtime_error {
            if is_stop_error(e) {
                compiled.stopped.set(true);
                compiled.commands.borrow_mut().clear();
                log::warn!(target: "synththing::script", "{e}");
            }
            compiled.log.borrow_mut().push(LogLevel::Error, e.clone());
        }

        for cmd in compiled.commands.borrow_mut().drain(..) {
            rasterize(buffer, width, height, cmd, &compiled.sprites.borrow());
        }
        compiled.store.borrow_mut().save(false);

        let ms = render_began.elapsed().as_secs_f32() * 1000.0;
        if let Some(message) = self.perf.record(Instant::now(), ms) {
            compiled.log.borrow_mut().push(LogLevel::Info, message.clone());
            log::info!(target: "synththing::script", "{message}");
        }
    }

    fn cursor_request(&self) -> CursorRequest {
        self.compiled.as_ref().map(|c| *c.cursor.borrow()).unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
fn compile(
    source: &str,
    sample_rate: u32,
    spectrum_left: &Rc<RefCell<SpectrumAnalyzer>>,
    spectrum_right: &Rc<RefCell<SpectrumAnalyzer>>,
    notes: &Rc<RefCell<NotesSnapshot>>,
    playback: &Rc<RefCell<EngineView>>,
    song: &Rc<RefCell<SongIdentity>>,
    input: &Rc<RefCell<VisualizerInput>>,
    note_list: &Rc<RefCell<Arc<NoteList>>>,
    live: &Rc<RefCell<LiveQueue>>,
    features: &Rc<RefCell<AudioFeatures>>,
    script_path: Option<&Path>,
    pending_settings: HashMap<String, serde_json::Value>,
    stop: &Arc<AtomicBool>,
    watchdog_limit: Duration,
    app_log: &Rc<Cell<bool>>,
) -> Result<Compiled, String> {
    // No io/os/ffi/debug: a visualizer script has no legitimate reason to
    // touch the filesystem or spawn processes, even one the user just wrote.
    // Coroutines are plain Lua (for spreading work over frames), and the
    // watchdog's hook carries over into them.
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::COROUTINE;
    let lua = Lua::new_with(libs, LuaOptions::new()).map_err(|e| e.to_string())?;
    let memory_limit = MEMORY_LIMIT.load(Ordering::Relaxed);
    if memory_limit > 0 {
        lua.set_memory_limit(memory_limit).map_err(|e| e.to_string())?;
    }

    // The watchdog: script code that runs past its deadline (an endless
    // loop, or far too much work in one go), or that's asked to stop (the
    // stop flag), is stopped with an error, checked every few thousand Lua
    // instructions, instead of freezing the app. The deadline is set
    // around each run of the script's code.
    let deadline: Rc<Cell<Option<Instant>>> = Rc::default();
    {
        let deadline = Rc::clone(&deadline);
        let stop = Arc::clone(stop);
        lua.set_global_hook(mlua::HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
            if stop.load(Ordering::Relaxed) {
                return Err(mlua::Error::runtime(stopped_message()));
            }
            match deadline.get() {
                Some(at) if Instant::now() > at => Err(mlua::Error::runtime(watchdog_message(watchdog_limit))),
                _ => Ok(mlua::VmState::Continue),
            }
        })
        .map_err(|e| e.to_string())?;
    }

    let commands: Rc<RefCell<Vec<DrawCommand>>> = Rc::new(RefCell::new(Vec::new()));
    let settings = Rc::new(RefCell::new(SettingsStore {
        descriptors: Vec::new(),
        pending: pending_settings,
        presets: Vec::new(),
    }));
    let log = Rc::new(RefCell::new(LogHistory {
        to_app_log: Rc::clone(app_log),
        name: script_path.map_or_else(|| "script".to_string(), display_name),
        ..LogHistory::default()
    }));
    let channel_requests: Rc<RefCell<Vec<(u8, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let debug_snapshot: Rc<RefCell<Option<DebugSnapshot>>> = Rc::new(RefCell::new(None));

    register_globals(
        &lua,
        sample_rate,
        &commands,
        spectrum_left,
        spectrum_right,
        notes,
        playback,
        song,
        &settings,
        &log,
        &channel_requests,
        &debug_snapshot,
    )
    .map_err(|e| e.to_string())?;
    let cursor = Rc::new(RefCell::new(CursorRequest::default()));
    let controls = Rc::new(RefCell::new(Controls::load(script_path)));
    let typing: Rc<RefCell<TypingSpan>> = Rc::default();
    register_input(&lua, input, &cursor, &controls, &typing).map_err(|e| e.to_string())?;
    let playback_requests: Rc<RefCell<Vec<PlaybackRequest>>> = Rc::default();
    let store = Rc::new(RefCell::new(ScriptStore::load(script_path)));
    let sprites: Rc<RefCell<Vec<Sprite>>> = Rc::default();
    register_sprites(&lua, &sprites, &commands).map_err(|e| e.to_string())?;
    register_song_and_state(&lua, note_list, &playback_requests, &store, song).map_err(|e| e.to_string())?;
    let options: Rc<Cell<ScriptOptions>> = Rc::default();
    {
        let options = Rc::clone(&options);
        let log = Rc::clone(&log);
        lua.globals()
            .set(
                "script_options",
                lua.create_function(move |_, table: Table| {
                    let mut set = options.get();
                    for pair in table.pairs::<String, Value>() {
                        let (key, value) = pair?;
                        match (key.as_str(), value) {
                            ("start_paused", Value::Boolean(on)) => set.start_paused = on,
                            ("app_log", Value::Boolean(on)) => set.app_log = on,
                            ("record_prepare", Value::Boolean(on)) => set.record_prepare = on,
                            ("record_auto", Value::Boolean(on)) => set.record_auto = on,
                            (name @ ("start_paused" | "app_log" | "record_prepare" | "record_auto"), other) => {
                                return Err(mlua::Error::runtime(format!(
                                    "script_options: {name} is true or false, not a {}",
                                    other.type_name()
                                )));
                            }
                            (other, _) => {
                                return Err(mlua::Error::runtime(format!(
                                    "script_options: no option `{other}` (there are {SCRIPT_OPTIONS})"
                                )));
                            }
                        }
                    }
                    options.set(set);
                    log.borrow_mut().script_wants_app_log = set.app_log;
                    Ok(())
                })
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
    }
    register_timing_audio_shapes(&lua, note_list, playback, features, &commands).map_err(|e| e.to_string())?;
    register_live_notes(&lua, notes, playback, note_list, live).map_err(|e| e.to_string())?;

    let host_globals: HashSet<String> = lua
        .globals()
        .pairs::<Value, Value>()
        .filter_map(Result::ok)
        .filter_map(|(key, _)| match key {
            Value::String(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect();

    deadline.set(Some(Instant::now() + watchdog_limit));
    let ran = lua.load(source).set_name("visualizer").exec();
    deadline.set(None);
    ran.map_err(|e| e.to_string())?;

    let render: Function = lua.globals().get("render").map_err(|_| {
        "script must define a `render(width, height, left, right)` function".to_string()
    })?;

    Ok(Compiled {
        source: source.to_string(),
        lua,
        render,
        commands,
        options,
        deadline,
        stopped: Cell::new(false),
        settings,
        log,
        channel_requests,
        debug_snapshot,
        cursor,
        playback_requests,
        store,
        sprites,
        controls,
        typing,
        frame: Cell::new(0),
        time: Cell::new(0.0),
        host_globals,
    })
}

/// Live synth commands waiting for the app, and the next sequence
/// playback's tag.
#[derive(Default)]
struct LiveQueue {
    commands: Vec<LiveCommand>,
    next_tag: u64,
}

impl LiveQueue {
    /// Silence everything: a new script (or a restart) starts quiet.
    fn stop_all(&mut self) {
        self.commands.retain(|c| !matches!(c, LiveCommand::Play(_)));
        self.commands.push(LiveCommand::StopAll);
    }
}

/// A note of a registered sequence, in beats.
struct SequenceNote {
    at: f64,
    length: f64,
    key: i64,
    velocity: Option<i64>,
    channel: Option<i64>,
}

const DEFAULT_VELOCITY: i64 = 100;
const DEFAULT_NOTE_SECONDS: f64 = 0.5;
const MAX_SEQUENCES: usize = 10_000;
const MAX_SEQUENCE_NOTES: usize = 100_000;

/// A sequence from the script: a list whose entries are a key number, or
/// a table {key, at, length, velocity, channel} (no key: a rest). `at`
/// defaults to where the previous entry ended, `length` to 1 beat.
fn parse_sequence(list: &Table) -> mlua::Result<Vec<SequenceNote>> {
    let mut notes = Vec::new();
    let mut cursor = 0.0f64;
    for (i, entry) in list.sequence_values::<Value>().enumerate() {
        let index = i + 1;
        let bad = |what: &str| mlua::Error::runtime(format!("sequence_register: entry {index} {what}"));
        let (key, at, length, velocity, channel) = match entry? {
            Value::Integer(key) => (Some(key), None, None, None, None),
            Value::Number(key) => (Some(key.round() as i64), None, None, None, None),
            Value::Table(t) => {
                let key = t.get::<Option<f64>>("key").map_err(|_| bad("has a key that isn't a number"))?;
                (
                    key.map(|k| k.round() as i64),
                    t.get::<Option<f64>>("at").map_err(|_| bad("has an `at` that isn't a number"))?,
                    t.get::<Option<f64>>("length").map_err(|_| bad("has a length that isn't a number"))?,
                    t.get::<Option<i64>>("velocity").map_err(|_| bad("has a velocity that isn't a number"))?,
                    t.get::<Option<i64>>("channel").map_err(|_| bad("has a channel that isn't a number"))?,
                )
            }
            other => return Err(bad(&format!("is a {}; use a key number or a table", other.type_name()))),
        };
        let at = at.unwrap_or(cursor);
        let length = length.unwrap_or(1.0);
        if !at.is_finite() || at < 0.0 || !length.is_finite() || length <= 0.0 {
            return Err(bad("needs `at` 0 or more and a length above 0"));
        }
        cursor = at + length;
        if let Some(key) = key {
            notes.push(SequenceNote { at, length, key, velocity, channel });
        }
        if notes.len() > MAX_SEQUENCE_NOTES {
            return Err(mlua::Error::runtime(format!(
                "sequence_register: at most {MAX_SEQUENCE_NOTES} notes per sequence"
            )));
        }
    }
    Ok(notes)
}

/// `play_note`, `note_on`, `note_off`, `stop_notes`, `notes_playable` and
/// sequences: notes a script plays on the live synth (`live.rs`), only
/// while a MIDI is loaded with a soundfont.
fn register_live_notes(
    lua: &Lua,
    notes: &Rc<RefCell<NotesSnapshot>>,
    playback: &Rc<RefCell<EngineView>>,
    note_list: &Rc<RefCell<Arc<NoteList>>>,
    live: &Rc<RefCell<LiveQueue>>,
) -> mlua::Result<()> {
    let globals = lua.globals();

    let playable = {
        let view = Rc::clone(playback);
        move || {
            let view = view.borrow();
            view.has_midi && view.has_soundfont
        }
    };
    // The channel asked for if the song uses it, else the song's lowest.
    let channel_for = {
        let notes = Rc::clone(notes);
        move |asked: Option<i64>| -> u8 {
            let notes = notes.borrow();
            let detected = &notes.detected_channels;
            match asked {
                Some(c) if (0..16).contains(&c) && detected.contains(&(c as u8)) => c as u8,
                _ => detected.first().copied().unwrap_or(0),
            }
        }
    };
    let key_of = |key: f64| -> u8 { key.round().clamp(0.0, 127.0) as u8 };
    let velocity_of = |v: Option<i64>| -> u8 { v.unwrap_or(DEFAULT_VELOCITY).clamp(1, 127) as u8 };

    {
        let playable = playable.clone();
        globals.set("notes_playable", lua.create_function(move |_, ()| Ok(playable()))?)?;
    }

    {
        let (playable, channel_for, live) = (playable.clone(), channel_for.clone(), Rc::clone(live));
        globals.set(
            "play_note",
            lua.create_function(move |_, (key, options): (f64, Option<Table>)| {
                if !playable() {
                    return Ok(false);
                }
                let (mut channel, mut velocity, mut duration, mut delay) = (None, None, None, None);
                if let Some(o) = options {
                    channel = o.get::<Option<i64>>("channel")?;
                    velocity = o.get::<Option<i64>>("velocity")?;
                    duration = o.get::<Option<f64>>("duration")?;
                    delay = o.get::<Option<f64>>("delay")?;
                }
                let duration = duration.unwrap_or(DEFAULT_NOTE_SECONDS);
                if !duration.is_finite() || duration <= 0.0 {
                    return Err(mlua::Error::runtime("play_note: duration must be above 0 seconds"));
                }
                live.borrow_mut().commands.push(LiveCommand::Play(vec![LiveNote {
                    delay: delay.unwrap_or(0.0).max(0.0),
                    length: Some(duration),
                    channel: channel_for(channel),
                    key: key_of(key),
                    velocity: velocity_of(velocity),
                    tag: 0,
                }]));
                Ok(true)
            })?,
        )?;
    }

    {
        let (playable, channel_for, live) = (playable.clone(), channel_for.clone(), Rc::clone(live));
        globals.set(
            "note_on",
            lua.create_function(move |_, (key, options): (f64, Option<Table>)| {
                if !playable() {
                    return Ok(false);
                }
                let (mut channel, mut velocity) = (None, None);
                if let Some(o) = options {
                    channel = o.get::<Option<i64>>("channel")?;
                    velocity = o.get::<Option<i64>>("velocity")?;
                }
                live.borrow_mut().commands.push(LiveCommand::Play(vec![LiveNote {
                    delay: 0.0,
                    length: None,
                    channel: channel_for(channel),
                    key: key_of(key),
                    velocity: velocity_of(velocity),
                    tag: 0,
                }]));
                Ok(true)
            })?,
        )?;
    }

    {
        let (channel_for, live) = (channel_for.clone(), Rc::clone(live));
        globals.set(
            "note_off",
            lua.create_function(move |_, (key, channel): (f64, Option<i64>)| {
                live.borrow_mut().commands.push(LiveCommand::Release { channel: channel_for(channel), key: key_of(key) });
                Ok(())
            })?,
        )?;
    }

    {
        let live = Rc::clone(live);
        globals.set(
            "stop_notes",
            lua.create_function(move |_, ()| {
                live.borrow_mut().stop_all();
                Ok(())
            })?,
        )?;
    }

    let sequences: Rc<RefCell<Vec<Vec<SequenceNote>>>> = Rc::default();
    {
        let sequences = Rc::clone(&sequences);
        globals.set(
            "sequence_register",
            lua.create_function(move |_, list: Table| {
                let notes = parse_sequence(&list)?;
                let mut sequences = sequences.borrow_mut();
                if sequences.len() >= MAX_SEQUENCES {
                    return Err(mlua::Error::runtime(format!(
                        "sequence_register: at most {MAX_SEQUENCES} sequences; register them once, outside render()"
                    )));
                }
                sequences.push(notes);
                Ok(sequences.len())
            })?,
        )?;
    }

    {
        let (playable, channel_for, live) = (playable.clone(), channel_for.clone(), Rc::clone(live));
        let (view, song) = (Rc::clone(playback), Rc::clone(note_list));
        let sequences = Rc::clone(&sequences);
        globals.set(
            "sequence_play",
            lua.create_function(move |_, (id, options): (usize, Option<Table>)| {
                let sequences = sequences.borrow();
                let sequence = id
                    .checked_sub(1)
                    .and_then(|i| sequences.get(i))
                    .ok_or_else(|| mlua::Error::runtime(format!("sequence_play: no sequence with id {id}")))?;
                if !playable() {
                    return Ok(None);
                }
                let (mut channel, mut transpose, mut tempo, mut delay) = (None, 0i64, None, None);
                if let Some(o) = options {
                    channel = o.get::<Option<i64>>("channel")?;
                    transpose = o.get::<Option<i64>>("transpose")?.unwrap_or(0);
                    tempo = o.get::<Option<f64>>("tempo")?;
                    delay = o.get::<Option<f64>>("delay")?;
                }
                // Default: the song's tempo right now, as heard (times the
                // playback speed), so the sequence plays in time with it.
                let bpm = match tempo {
                    Some(bpm) => bpm,
                    None => {
                        let view = view.borrow();
                        let song_bpm = song.borrow().timing().map(|t| t.tempo_at(view.position)).unwrap_or(120.0);
                        song_bpm * view.speed
                    }
                };
                if !bpm.is_finite() || bpm <= 0.0 {
                    return Err(mlua::Error::runtime("sequence_play: tempo must be above 0"));
                }
                let beat = 60.0 / bpm;
                let delay = delay.unwrap_or(0.0).max(0.0);
                let mut queue = live.borrow_mut();
                queue.next_tag += 1;
                let tag = queue.next_tag;
                let notes = sequence
                    .iter()
                    .filter_map(|n| {
                        let key = n.key + transpose;
                        (0..128).contains(&key).then(|| LiveNote {
                            delay: delay + n.at * beat,
                            length: Some(n.length * beat),
                            channel: channel_for(n.channel.or(channel)),
                            key: key as u8,
                            velocity: velocity_of(n.velocity),
                            tag,
                        })
                    })
                    .collect();
                queue.commands.push(LiveCommand::Play(notes));
                Ok(Some(tag))
            })?,
        )?;
    }

    {
        let live = Rc::clone(live);
        globals.set(
            "sequence_stop",
            lua.create_function(move |_, handle: u64| {
                live.borrow_mut().commands.push(LiveCommand::Stop(handle));
                Ok(())
            })?,
        )?;
    }
    Ok(())
}

/// `notes_between`, `set_paused`, `seek`, the playlist and transport
/// controls, `store_get`, `store_set`.
fn register_song_and_state(
    lua: &Lua,
    note_list: &Rc<RefCell<Arc<NoteList>>>,
    playback_requests: &Rc<RefCell<Vec<PlaybackRequest>>>,
    store: &Rc<RefCell<ScriptStore>>,
    song: &Rc<RefCell<SongIdentity>>,
) -> mlua::Result<()> {
    let globals = lua.globals();

    let notes = Rc::clone(note_list);
    globals.set(
        "notes_between",
        lua.create_function(move |lua, (t0, t1): (f64, f64)| {
            let notes = notes.borrow();
            let array = lua.create_table()?;
            for (n, (index, note)) in notes.between(t0, t1).enumerate() {
                let entry = lua.create_table_with_capacity(0, 6)?;
                entry.raw_set("id", index + 1)?;
                entry.raw_set("channel", note.channel)?;
                entry.raw_set("key", note.key)?;
                entry.raw_set("velocity", note.velocity)?;
                entry.raw_set("start", note.start)?;
                entry.raw_set("stop", note.stop)?;
                array.raw_set(n + 1, entry)?;
            }
            Ok(array)
        })?,
    )?;

    let requests = Rc::clone(playback_requests);
    globals.set(
        "set_paused",
        lua.create_function(move |_, paused: bool| {
            requests.borrow_mut().push(PlaybackRequest::Pause(paused));
            Ok(())
        })?,
    )?;
    let requests = Rc::clone(playback_requests);
    globals.set(
        "seek",
        lua.create_function(move |_, seconds: f64| {
            if !seconds.is_finite() {
                return Err(mlua::Error::runtime("seek() needs a number of seconds"));
            }
            requests.borrow_mut().push(PlaybackRequest::Seek(seconds.max(0.0)));
            Ok(())
        })?,
    )?;

    // The current playlist, read-only, and moving between its songs.
    let view = Rc::clone(song);
    globals.set(
        "playlist",
        lua.create_function(move |lua, ()| {
            let song = view.borrow();
            let Some(list) = &song.transport.playlist else { return Ok(Value::Nil) };
            let t = lua.create_table_with_capacity(0, 4)?;
            t.raw_set("name", list.name.as_str())?;
            t.raw_set("playing", list.playing)?;
            t.raw_set("current", list.current.map(|i| i + 1))?;
            let entries = lua.create_table_with_capacity(list.entries.len(), 0)?;
            for (n, entry) in list.entries.iter().enumerate() {
                let e = lua.create_table_with_capacity(0, 3)?;
                e.raw_set("name", entry.name.as_str())?;
                e.raw_set("length", entry.length)?;
                e.raw_set("missing", entry.missing)?;
                entries.raw_set(n + 1, e)?;
            }
            t.raw_set("entries", entries)?;
            Ok(Value::Table(t))
        })?,
    )?;
    let (requests, view) = (Rc::clone(playback_requests), Rc::clone(song));
    globals.set(
        "play_track",
        lua.create_function(move |_, index: i64| {
            let count = view.borrow().transport.playlist.as_ref().map_or(0, |l| l.entries.len());
            if index < 1 || index as usize > count {
                return Err(mlua::Error::runtime(format!(
                    "play_track({index}): the playlist has {count} song{}",
                    if count == 1 { "" } else { "s" }
                )));
            }
            requests.borrow_mut().push(PlaybackRequest::PlayTrack(index as usize - 1));
            Ok(())
        })?,
    )?;
    for (name, request) in [("next_track", PlaybackRequest::NextTrack), ("previous_track", PlaybackRequest::PreviousTrack)] {
        let requests = Rc::clone(playback_requests);
        globals.set(
            name,
            lua.create_function(move |_, ()| {
                requests.borrow_mut().push(request);
                Ok(())
            })?,
        )?;
    }
    let requests = Rc::clone(playback_requests);
    globals.set(
        "set_speed",
        lua.create_function(move |_, speed: f64| {
            if !speed.is_finite() || speed <= 0.0 {
                return Err(mlua::Error::runtime("set_speed() needs a speed above 0 (1.0 is normal)"));
            }
            requests.borrow_mut().push(PlaybackRequest::Speed(speed));
            Ok(())
        })?,
    )?;
    let requests = Rc::clone(playback_requests);
    globals.set(
        "set_loop",
        lua.create_function(move |_, mode: String| {
            let mode = match mode.to_ascii_lowercase().as_str() {
                "off" => LoopMode::Off,
                "one" => LoopMode::One,
                "all" => LoopMode::All,
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "set_loop(\"{other}\"): use \"off\", \"one\" or \"all\""
                    )));
                }
            };
            requests.borrow_mut().push(PlaybackRequest::Loop(mode));
            Ok(())
        })?,
    )?;
    let requests = Rc::clone(playback_requests);
    globals.set(
        "set_shuffle",
        lua.create_function(move |_, on: bool| {
            requests.borrow_mut().push(PlaybackRequest::Shuffle(on));
            Ok(())
        })?,
    )?;

    // Recording: what the recording asks of the script, and its answers.
    let view = Rc::clone(song);
    globals.set(
        "recording",
        lua.create_function(move |lua, ()| {
            let Some(take) = view.borrow().take else { return Ok(Value::Nil) };
            let t = lua.create_table_with_capacity(0, 3)?;
            t.raw_set("phase", take.phase.name())?;
            t.raw_set("kind", take.kind.name())?;
            t.raw_set("mode", if take.auto { "auto" } else { "manual" })?;
            Ok(Value::Table(t))
        })?,
    )?;
    let requests = Rc::clone(playback_requests);
    globals.set(
        "recording_ready",
        lua.create_function(move |_, ()| {
            requests.borrow_mut().push(PlaybackRequest::RecordingReady);
            Ok(())
        })?,
    )?;
    let requests = Rc::clone(playback_requests);
    globals.set(
        "recording_done",
        lua.create_function(move |_, ()| {
            requests.borrow_mut().push(PlaybackRequest::RecordingDone);
            Ok(())
        })?,
    )?;

    let data = Rc::clone(store);
    globals.set(
        "store_get",
        lua.create_function(move |lua, key: String| match data.borrow().values.get(&key) {
            Some(value) => json_to_lua(lua, value),
            None => Ok(Value::Nil),
        })?,
    )?;
    let data = Rc::clone(store);
    globals.set(
        "store_set",
        lua.create_function(move |_, (key, value): (String, Value)| {
            let mut store = data.borrow_mut();
            let changed = if value.is_nil() {
                store.values.remove(&key).is_some()
            } else {
                let json = lua_to_json(&value, 0)?;
                let changed = store.values.get(&key) != Some(&json);
                if changed {
                    store.values.insert(key, json);
                }
                changed
            };
            store.dirty |= changed;
            Ok(())
        })?,
    )?;
    Ok(())
}

/// `beat`, `time_at_beat`, `bar`, `tempo`, `time_signature`,
/// `level_left`, `level_right`, `onset`, `circle`, `triangle`, `polygon`.
/// `sprite_register`, `sprite`, `sprite_size`.
fn register_sprites(
    lua: &Lua,
    sprites: &Rc<RefCell<Vec<Sprite>>>,
    commands: &Rc<RefCell<Vec<DrawCommand>>>,
) -> mlua::Result<()> {
    let globals = lua.globals();

    let list = Rc::clone(sprites);
    globals.set(
        "sprite_register",
        lua.create_function(move |_, data: Table| {
            let mut list = list.borrow_mut();
            if list.len() >= MAX_SPRITES {
                return Err(mlua::Error::runtime(format!(
                    "sprite_register: at most {MAX_SPRITES} sprites per script (register them once, outside render())"
                )));
            }
            list.push(parse_sprite(&data)?);
            Ok(list.len())
        })?,
    )?;

    let list = Rc::clone(sprites);
    let cmds = Rc::clone(commands);
    globals.set(
        "sprite",
        lua.create_function(move |_, (id, x, y, options): (usize, f32, f32, Value)| {
            let list = list.borrow();
            let Some(sprite) = id.checked_sub(1).and_then(|i| list.get(i)) else {
                return Err(mlua::Error::runtime(format!("sprite: no sprite with id {id}")));
            };
            let (mut scale, mut flip_x, mut flip_y) = (1.0f32, false, false);
            let mut src = (0, 0, sprite.width, sprite.height);
            let mut colors: Option<Rc<[u32]>> = None;
            match options {
                Value::Nil => {}
                Value::Integer(n) => scale = n as f32,
                Value::Number(n) => scale = n as f32,
                Value::Table(t) => {
                    scale = t.get::<Option<f32>>("scale")?.unwrap_or(1.0);
                    flip_x = t.get::<Option<bool>>("flip_x")?.unwrap_or(false);
                    flip_y = t.get::<Option<bool>>("flip_y")?.unwrap_or(false);
                    if let Some(part) = t.get::<Option<Table>>("src")? {
                        src = sprite_part(&part, sprite)?;
                    }
                    colors = sprite_colors(&t, sprite)?;
                }
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "sprite: the 4th argument is a scale or an options table, not a {}",
                        other.type_name()
                    )));
                }
            }
            if !(scale.is_finite() && scale >= 0.0) {
                return Err(mlua::Error::runtime("sprite: scale must be a number, 0 or more"));
            }
            let size = ((src.2 as f32 * scale).round(), (src.3 as f32 * scale).round());
            cmds.borrow_mut().push(DrawCommand::Sprite { index: id - 1, x, y, scale, flip_x, flip_y, src, colors });
            Ok(size)
        })?,
    )?;

    let list = Rc::clone(sprites);
    globals.set(
        "sprite_size",
        lua.create_function(move |_, id: usize| {
            let list = list.borrow();
            match id.checked_sub(1).and_then(|i| list.get(i)) {
                Some(sprite) => Ok((sprite.width, sprite.height)),
                None => Err(mlua::Error::runtime(format!("sprite_size: no sprite with id {id}"))),
            }
        })?,
    )?;
    Ok(())
}

/// The colors for one draw of `sprite`, from an options table's `palette`
/// (`{[index] = color, ...}`, replacing just those entries) and `tint` (a
/// color every entry is multiplied by), or `None` for its own colors.
fn sprite_colors(options: &Table, sprite: &Sprite) -> mlua::Result<Option<Rc<[u32]>>> {
    let swaps: Option<Table> = options
        .get::<Option<Table>>("palette")
        .map_err(|_| mlua::Error::runtime("sprite: palette is a table of {[index] = color}"))?;
    let tint: Option<Table> = options
        .get::<Option<Table>>("tint")
        .map_err(|_| mlua::Error::runtime("sprite: tint is a color table"))?;
    if swaps.is_none() && tint.is_none() {
        return Ok(None);
    }
    let mut colors = sprite.palette.clone();
    if let Some(swaps) = swaps {
        for pair in swaps.pairs::<Value, Value>() {
            let (index, color) = pair?;
            let index = match index {
                Value::Integer(i) => i,
                Value::Number(n) if n.fract() == 0.0 => n as i64,
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "sprite: palette keys are palette indices, not a {}",
                        other.type_name()
                    )));
                }
            };
            if index < 1 || index as usize >= colors.len() {
                return Err(mlua::Error::runtime(format!(
                    "sprite: palette[{index}], but the sprite's palette has {} colors",
                    colors.len() - 1
                )));
            }
            let Value::Table(color) = color else {
                return Err(mlua::Error::runtime(format!("sprite: palette[{index}] isn't a color table")));
            };
            colors[index as usize] = table_to_color(&color)?;
        }
    }
    if let Some(tint) = tint {
        let tint = table_to_color(&tint)?;
        for color in colors.iter_mut().skip(1) {
            *color = tint_color(*color, tint);
        }
    }
    Ok(Some(colors.into()))
}

/// The part of `sprite` an options table's `src` names: `{x, y, w, h}` or
/// `{x = .., y = .., w = .., h = ..}`, in sprite pixels from the top-left
/// (0, 0), clipped to the sprite.
fn sprite_part(part: &Table, sprite: &Sprite) -> mlua::Result<(usize, usize, usize, usize)> {
    let field = |name: &str, index: usize| -> mlua::Result<f64> {
        let named: Option<f64> = part.get(name)?;
        let value = match named {
            Some(v) => Some(v),
            None => part.raw_get::<Option<f64>>(index)?,
        };
        value.ok_or_else(|| mlua::Error::runtime(format!("sprite: src needs {name} (src = {{x, y, w, h}})")))
    };
    let (x, y, w, h) = (field("x", 1)?, field("y", 2)?, field("w", 3)?, field("h", 4)?);
    let x0 = (x.max(0.0) as usize).min(sprite.width);
    let y0 = (y.max(0.0) as usize).min(sprite.height);
    let x1 = ((x + w).max(0.0) as usize).min(sprite.width);
    let y1 = ((y + h).max(0.0) as usize).min(sprite.height);
    Ok((x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)))
}

/// A sprite from `{image = {{1, 0, 2, ...}, ...}, palette = {{r, g, b, a}, ...}}`:
/// `image` is rows of palette indices (1-based like Lua; 0 is
/// transparent), rows may differ in length (short rows are transparent at
/// the end), and the width is the longest row.
fn parse_sprite(data: &Table) -> mlua::Result<Sprite> {
    let image: Table = data
        .get::<Option<Table>>("image")?
        .ok_or_else(|| mlua::Error::runtime("sprite_register: needs an `image` table (rows of palette indices)"))?;
    let palette_table: Table = data
        .get::<Option<Table>>("palette")?
        .ok_or_else(|| mlua::Error::runtime("sprite_register: needs a `palette` table (a list of colors)"))?;
    let mut palette = Vec::with_capacity(palette_table.raw_len() + 1);
    palette.push(0); // index 0: transparent
    for i in 1..=palette_table.raw_len() {
        let color: Table = palette_table
            .raw_get(i)
            .map_err(|_| mlua::Error::runtime(format!("sprite_register: palette[{i}] isn't a color table")))?;
        palette.push(table_to_color(&color)?);
    }

    let height = image.raw_len();
    let mut rows: Vec<Vec<u32>> = Vec::with_capacity(height); // palette indices
    for y in 1..=height {
        let row: Table = image
            .raw_get(y)
            .map_err(|_| mlua::Error::runtime(format!("sprite_register: image[{y}] isn't a row table")))?;
        let mut pixels = Vec::with_capacity(row.raw_len());
        for x in 1..=row.raw_len() {
            let index: i64 = row.raw_get(x).map_err(|_| {
                mlua::Error::runtime(format!("sprite_register: image[{y}][{x}] isn't a palette index"))
            })?;
            if !(0..palette.len() as i64).contains(&index) {
                return Err(mlua::Error::runtime(format!(
                    "sprite_register: image[{y}][{x}] is {index}, but the palette has {} colors (0 is transparent)",
                    palette.len() - 1
                )));
            }
            pixels.push(index as u32);
        }
        rows.push(pixels);
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut indices = Vec::with_capacity(width * height);
    for mut row in rows {
        row.resize(width, 0);
        indices.extend(row);
    }
    let pixels = indices.iter().map(|&i| palette[i as usize]).collect();
    Ok(Sprite { width, height, pixels, indices, palette })
}

fn register_timing_audio_shapes(
    lua: &Lua,
    note_list: &Rc<RefCell<Arc<NoteList>>>,
    playback: &Rc<RefCell<EngineView>>,
    features: &Rc<RefCell<AudioFeatures>>,
    commands: &Rc<RefCell<Vec<DrawCommand>>>,
) -> mlua::Result<()> {
    let globals = lua.globals();

    // Timing: each takes an optional song time (default: now) and returns
    // nil when the song has no beats (plain audio, SMPTE-timed MIDI).
    macro_rules! timing_fn {
        ($name:literal, |$timing:ident, $seconds:ident| $body:expr) => {{
            let notes = Rc::clone(note_list);
            let view = Rc::clone(playback);
            globals.set(
                $name,
                lua.create_function(move |_, seconds: Option<f64>| {
                    let notes = notes.borrow();
                    let $seconds = seconds.unwrap_or(view.borrow().position);
                    Ok(notes.timing().map(|$timing| $body))
                })?,
            )?;
        }};
    }
    timing_fn!("beat", |timing, seconds| timing.beat_at(seconds));
    timing_fn!("tempo", |timing, seconds| timing.tempo_at(seconds));
    let notes = Rc::clone(note_list);
    let view = Rc::clone(playback);
    globals.set(
        "bar",
        lua.create_function(move |_, seconds: Option<f64>| {
            let notes = notes.borrow();
            let seconds = seconds.unwrap_or(view.borrow().position);
            Ok(match notes.timing() {
                Some(timing) => {
                    let (bar, beat) = timing.bar_at(seconds);
                    (Some(bar), Some(beat))
                }
                None => (None, None),
            })
        })?,
    )?;
    let notes = Rc::clone(note_list);
    let view = Rc::clone(playback);
    globals.set(
        "time_signature",
        lua.create_function(move |_, seconds: Option<f64>| {
            let notes = notes.borrow();
            let seconds = seconds.unwrap_or(view.borrow().position);
            Ok(match notes.timing() {
                Some(timing) => {
                    let (numerator, denominator) = timing.signature_at(seconds);
                    (Some(numerator), Some(denominator))
                }
                None => (None, None),
            })
        })?,
    )?;
    let notes = Rc::clone(note_list);
    globals.set(
        "time_at_beat",
        lua.create_function(move |_, beat: f64| Ok(notes.borrow().timing().map(|t| t.seconds_at_beat(beat))))?,
    )?;

    // Audio features.
    let state = Rc::clone(features);
    globals.set("level_left", lua.create_function(move |_, ()| Ok(state.borrow().level_left))?)?;
    let state = Rc::clone(features);
    globals.set("level_right", lua.create_function(move |_, ()| Ok(state.borrow().level_right))?)?;
    let state = Rc::clone(features);
    globals.set(
        "onset",
        lua.create_function(move |_, ()| {
            let mut features = state.borrow_mut();
            features.onset_wanted = true;
            Ok((features.onset, features.onset_strength))
        })?,
    )?;

    // Shapes.
    let cmds = Rc::clone(commands);
    globals.set(
        "circle",
        lua.create_function(move |_, (x, y, radius, color): (f32, f32, f32, Table)| {
            let color = table_to_color(&color)?;
            cmds.borrow_mut().push(DrawCommand::Circle { x, y, radius, color });
            Ok(())
        })?,
    )?;
    let cmds = Rc::clone(commands);
    globals.set(
        "triangle",
        lua.create_function(
            move |_, (x1, y1, x2, y2, x3, y3, color): (f32, f32, f32, f32, f32, f32, Table)| {
                let color = table_to_color(&color)?;
                cmds.borrow_mut().push(DrawCommand::Polygon { points: vec![(x1, y1), (x2, y2), (x3, y3)], color });
                Ok(())
            },
        )?,
    )?;
    let cmds = Rc::clone(commands);
    globals.set(
        "polygon",
        lua.create_function(move |_, (points, color): (Table, Table)| {
            let color = table_to_color(&color)?;
            let points = table_to_points(&points)?;
            cmds.borrow_mut().push(DrawCommand::Polygon { points, color });
            Ok(())
        })?,
    )?;
    Ok(())
}

/// Polygon points from Lua: either a flat list `{x1, y1, x2, y2, ...}` or
/// a list of points `{{x, y}, ...}` / `{{x = .., y = ..}, ...}`.
fn table_to_points(table: &Table) -> mlua::Result<Vec<(f32, f32)>> {
    let len = table.raw_len();
    if len > MAX_POLYGON_POINTS * 2 {
        return Err(mlua::Error::runtime(format!("polygon: at most {MAX_POLYGON_POINTS} points")));
    }
    let first: Value = table.raw_get(1)?;
    let mut points = Vec::new();
    if let Value::Table(_) = first {
        for i in 1..=len {
            let point: Table = table.raw_get(i)?;
            let x: Option<f32> = point.get("x")?;
            let y: Option<f32> = point.get("y")?;
            let (x, y) = match (x, y) {
                (Some(x), Some(y)) => (x, y),
                _ => (point.raw_get(1)?, point.raw_get(2)?),
            };
            points.push((x, y));
        }
    } else {
        if !len.is_multiple_of(2) {
            return Err(mlua::Error::runtime("polygon: a flat point list needs an even count (x, y pairs)"));
        }
        for i in (1..=len).step_by(2) {
            points.push((table.raw_get(i)?, table.raw_get(i + 1)?));
        }
    }
    Ok(points)
}

/// Deepest table nesting `store_set` accepts.
const MAX_STORE_DEPTH: usize = 16;

/// A Lua value as JSON, for the store: booleans, numbers, strings and tables
/// (a table with keys 1..n is an array, anything else an object with its
/// keys as strings). Functions and the like can't be stored.
fn lua_to_json(value: &Value, depth: usize) -> mlua::Result<serde_json::Value> {
    use serde_json::Value as Json;
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(i) => Json::from(*i),
        Value::Number(n) => serde_json::Number::from_f64(*n)
            .map(Json::Number)
            .ok_or_else(|| mlua::Error::runtime("store_set can't save inf or nan"))?,
        Value::String(s) => Json::String(s.to_str()?.to_string()),
        Value::Table(table) => {
            if depth >= MAX_STORE_DEPTH {
                return Err(mlua::Error::runtime("store_set: tables nested too deeply"));
            }
            let len = table.raw_len();
            let pairs: Vec<(Value, Value)> = table.pairs::<Value, Value>().collect::<mlua::Result<_>>()?;
            if len > 0 && pairs.len() == len {
                let mut array = Vec::with_capacity(len);
                for i in 1..=len {
                    array.push(lua_to_json(&table.raw_get(i)?, depth + 1)?);
                }
                Json::Array(array)
            } else {
                let mut object = serde_json::Map::new();
                for (k, v) in pairs {
                    let key = match k {
                        Value::String(s) => s.to_str()?.to_string(),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        _ => return Err(mlua::Error::runtime("store_set: table keys must be strings or numbers")),
                    };
                    object.insert(key, lua_to_json(&v, depth + 1)?);
                }
                Json::Object(object)
            }
        }
        other => {
            return Err(mlua::Error::runtime(format!("store_set can't save a {}", other.type_name())));
        }
    })
}

fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as Json;
    Ok(match value {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                table.raw_set(i + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
        Json::Object(map) => {
            let table = lua.create_table_with_capacity(0, map.len())?;
            for (k, v) in map {
                table.raw_set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Value::Table(table)
        }
    })
}

/// The input and cursor functions: `mouse`, `mouse_delta`, `scroll`,
/// `mouse_down/pressed/released`, `key_down/pressed/released`, `has_focus`,
/// `display_mode`, `set_cursor_visible`, `set_cursor_locked`.
fn register_input(
    lua: &Lua,
    input: &Rc<RefCell<VisualizerInput>>,
    cursor: &Rc<RefCell<CursorRequest>>,
    controls: &Rc<RefCell<Controls>>,
    typing: &Rc<RefCell<TypingSpan>>,
) -> mlua::Result<()> {
    let globals = lua.globals();

    // Actions: named, rebindable controls (see the Controls list in Script
    // Settings). Blocked (always "up") while a typing span is active.
    let actions = Rc::clone(controls);
    globals.set(
        "input_register",
        lua.create_function(move |_, (name, default, labels): (String, Value, Option<Table>)| {
            let labels = item_labels("input_register", labels)?;
            let mut controls = actions.borrow_mut();
            if let Some(i) = controls.actions.iter().position(|a| a.name == name) {
                if let Some(labels) = labels {
                    controls.actions[i].labels = labels;
                }
                return Ok(i + 1);
            }
            let names: Vec<String> = match default {
                Value::String(s) => vec![s.to_str()?.to_string()],
                Value::Table(t) => t.sequence_values::<String>().collect::<mlua::Result<_>>()?,
                Value::Nil => Vec::new(),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "input_register: the default is a key name or a list of them, not a {}",
                        other.type_name()
                    )));
                }
            };
            let mut defaults = Vec::new();
            for key_name in &names {
                match parse_binding(key_name)? {
                    Some(binding) if !defaults.contains(&binding) => defaults.push(binding),
                    Some(_) => {}
                    None => {
                        return Err(mlua::Error::runtime(format!(
                            "input_register: '{key_name}' is reserved by the app and can't be bound"
                        )));
                    }
                }
            }
            let bindings = controls.saved_bindings(&name).unwrap_or_else(|| defaults.clone());
            controls.actions.push(Action { name, defaults, bindings, labels: labels.unwrap_or_default() });
            Ok(controls.actions.len())
        })?,
    )?;
    let actions = Rc::clone(controls);
    let state = Rc::clone(input);
    let span = Rc::clone(typing);
    globals.set(
        "input",
        lua.create_function(move |_, id: usize| {
            let controls = actions.borrow();
            let action = id
                .checked_sub(1)
                .and_then(|i| controls.actions.get(i))
                .ok_or_else(|| mlua::Error::runtime(format!("input: no action with id {id}")))?;
            if span.borrow().active {
                return Ok("up");
            }
            Ok(action_state(&state.borrow(), &action.bindings))
        })?,
    )?;
    let actions = Rc::clone(controls);
    let state = Rc::clone(input);
    let span = Rc::clone(typing);
    globals.set(
        "input_down",
        lua.create_function(move |_, id: usize| {
            let controls = actions.borrow();
            let action = id
                .checked_sub(1)
                .and_then(|i| controls.actions.get(i))
                .ok_or_else(|| mlua::Error::runtime(format!("input_down: no action with id {id}")))?;
            if span.borrow().active {
                return Ok(false);
            }
            Ok(matches!(action_state(&state.borrow(), &action.bindings), "pressed" | "held"))
        })?,
    )?;
    let actions = Rc::clone(controls);
    let state = Rc::clone(input);
    let span = Rc::clone(typing);
    globals.set(
        "input_value",
        lua.create_function(move |_, id: usize| {
            let controls = actions.borrow();
            let action = id
                .checked_sub(1)
                .and_then(|i| controls.actions.get(i))
                .ok_or_else(|| mlua::Error::runtime(format!("input_value: no action with id {id}")))?;
            if span.borrow().active {
                return Ok(0.0);
            }
            Ok(action_value(&state.borrow(), &action.bindings))
        })?,
    )?;

    // Controllers, read directly: analogue axes and who's connected.
    let state = Rc::clone(input);
    globals.set(
        "pad_axis",
        lua.create_function(move |_, name: String| {
            let index = AXIS_NAMES.iter().position(|n| n.eq_ignore_ascii_case(&name)).ok_or_else(|| {
                mlua::Error::runtime(format!("pad_axis: no axis '{name}' (there's {})", AXIS_NAMES.join(", ")))
            })?;
            Ok(state.borrow().pads.axes[index])
        })?,
    )?;
    let state = Rc::clone(input);
    globals.set(
        "gamepads",
        lua.create_function(move |lua, ()| {
            let state = state.borrow();
            let names = lua.create_table_with_capacity(state.pads.connected.len(), 0)?;
            for (i, name) in state.pads.connected.iter().enumerate() {
                names.raw_set(i + 1, name.as_str())?;
            }
            Ok(names)
        })?,
    )?;

    // Text.
    let state = Rc::clone(input);
    globals.set("text_typed", lua.create_function(move |_, ()| Ok(state.borrow().typed()))?)?;
    let span = Rc::clone(typing);
    globals.set(
        "typing_begin",
        lua.create_function(move |_, (text, options): (Option<String>, Option<Table>)| {
            let (mut max_length, mut multiline) = (None, false);
            if let Some(options) = options {
                max_length = options.get::<Option<usize>>("max_length")?;
                multiline = options.get::<Option<bool>>("multiline")?.unwrap_or(false);
            }
            span.borrow_mut().begin(text.as_deref().unwrap_or(""), max_length, multiline);
            Ok(())
        })?,
    )?;
    let span = Rc::clone(typing);
    globals.set(
        "typing_state",
        lua.create_function(move |lua, ()| {
            let span = span.borrow();
            let (line, column) = span.line_and_column();
            let t = lua.create_table_with_capacity(0, 8)?;
            t.raw_set("active", span.active)?;
            t.raw_set("done", span.done)?;
            t.raw_set("cancelled", span.cancelled)?;
            t.raw_set("text", span.text())?;
            t.raw_set("cursor", span.cursor())?;
            t.raw_set("line", line)?;
            t.raw_set("column", column)?;
            t.raw_set("key", span.last_key.clone())?;
            Ok(t)
        })?,
    )?;
    let span = Rc::clone(typing);
    globals.set(
        "typing_end",
        lua.create_function(move |_, ()| {
            let mut span = span.borrow_mut();
            span.end();
            Ok(span.text())
        })?,
    )?;

    let state = Rc::clone(input);
    globals.set(
        "mouse",
        lua.create_function(move |_, ()| {
            let pointer = state.borrow().pointer;
            Ok((pointer.map(|p| p.0), pointer.map(|p| p.1)))
        })?,
    )?;
    let state = Rc::clone(input);
    globals.set("mouse_delta", lua.create_function(move |_, ()| Ok(state.borrow().pointer_delta))?)?;
    let state = Rc::clone(input);
    globals.set("scroll", lua.create_function(move |_, ()| Ok(state.borrow().scroll))?)?;

    for (name, pick) in [
        ("mouse_down", 0usize),
        ("mouse_pressed", 1),
        ("mouse_released", 2),
    ] {
        let state = Rc::clone(input);
        globals.set(
            name,
            lua.create_function(move |_, button: Option<String>| {
                let index = parse_mouse_button(button.as_deref())?;
                let input = state.borrow();
                let buttons = match pick {
                    0 => &input.buttons_down,
                    1 => &input.buttons_pressed,
                    _ => &input.buttons_released,
                };
                Ok(buttons[index])
            })?,
        )?;
    }

    // The original per-key functions: superseded by actions (input_register)
    // and out of the docs, kept so existing scripts keep working.
    for (name, pick) in [("key_down", 0usize), ("key_pressed", 1), ("key_released", 2)] {
        let state = Rc::clone(input);
        let span = Rc::clone(typing);
        globals.set(
            name,
            lua.create_function(move |_, key: String| {
                let Some(key) = parse_key(&key)? else {
                    return Ok(false); // reserved by the app, never reported
                };
                if span.borrow().active {
                    return Ok(false);
                }
                let input = state.borrow();
                let keys = match pick {
                    0 => &input.keys_down,
                    1 => &input.keys_pressed,
                    _ => &input.keys_released,
                };
                Ok(keys.contains(&key))
            })?,
        )?;
    }

    let state = Rc::clone(input);
    globals.set("has_focus", lua.create_function(move |_, ()| Ok(state.borrow().focused))?)?;
    let state = Rc::clone(input);
    globals.set("display_mode", lua.create_function(move |_, ()| Ok(state.borrow().mode.name()))?)?;

    let request = Rc::clone(cursor);
    globals.set(
        "set_cursor_visible",
        lua.create_function(move |_, visible: bool| {
            request.borrow_mut().hidden = !visible;
            Ok(())
        })?,
    )?;
    let request = Rc::clone(cursor);
    globals.set(
        "set_cursor_locked",
        lua.create_function(move |_, locked: bool| {
            request.borrow_mut().confined = locked;
            Ok(())
        })?,
    )?;
    Ok(())
}

/// "left" (also the default), "right" or "middle", as an index into
/// `VisualizerInput`'s button arrays.
fn parse_mouse_button(name: Option<&str>) -> mlua::Result<usize> {
    match name.map(str::to_ascii_lowercase).as_deref() {
        None | Some("left") => Ok(0),
        Some("right") => Ok(1),
        Some("middle") => Ok(2),
        Some(other) => Err(mlua::Error::runtime(format!(
            "unknown mouse button '{other}' (use \"left\", \"right\" or \"middle\")"
        ))),
    }
}

/// A key by name, case-insensitively ("a", "space", "up", "enter", "f5",
/// ...). `Ok(None)` for the app's reserved keys (Escape, F5, F11), which
/// scripts never see.
fn parse_key(name: &str) -> mlua::Result<Option<egui::Key>> {
    let key = egui::Key::ALL
        .iter()
        .copied()
        .find(|k| k.name().eq_ignore_ascii_case(name))
        .or_else(|| egui::Key::from_name(name))
        .ok_or_else(|| {
            mlua::Error::runtime(format!(
                "unknown key '{name}' (try e.g. \"a\", \"space\", \"up\", \"enter\")"
            ))
        })?;
    Ok((!RESERVED_KEYS.contains(&key)).then_some(key))
}

#[allow(clippy::too_many_arguments)]
fn register_globals(
    lua: &Lua,
    sample_rate: u32,
    commands: &Rc<RefCell<Vec<DrawCommand>>>,
    spectrum_left: &Rc<RefCell<SpectrumAnalyzer>>,
    spectrum_right: &Rc<RefCell<SpectrumAnalyzer>>,
    notes: &Rc<RefCell<NotesSnapshot>>,
    playback: &Rc<RefCell<EngineView>>,
    song: &Rc<RefCell<SongIdentity>>,
    settings: &Rc<RefCell<SettingsStore>>,
    log: &Rc<RefCell<LogHistory>>,
    channel_requests: &Rc<RefCell<Vec<(u8, bool)>>>,
    debug_snapshot: &Rc<RefCell<Option<DebugSnapshot>>>,
) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set("SAMPLE_RATE", sample_rate)?;
    globals.set("NOTE_LOOKAHEAD", NOTE_LOOKAHEAD_SECS)?;
    globals.set("DT", 0.0)?;
    globals.set("FRAME", 0)?;
    globals.set("TIME", 0.0)?;

    let cmds = Rc::clone(commands);
    globals.set(
        "clear",
        lua.create_function(move |_, color: Table| {
            cmds.borrow_mut().push(DrawCommand::Clear(table_to_color(&color)?));
            Ok(())
        })?,
    )?;

    let cmds = Rc::clone(commands);
    globals.set(
        "line",
        lua.create_function(
            move |_, (x0, y0, x1, y1, color): (f32, f32, f32, f32, Table)| {
                let color = table_to_color(&color)?;
                cmds.borrow_mut()
                    .push(DrawCommand::Line { x0, y0, x1, y1, color });
                Ok(())
            },
        )?,
    )?;

    let cmds = Rc::clone(commands);
    globals.set(
        "rect",
        lua.create_function(
            move |_, (x0, y0, x1, y1, color): (f32, f32, f32, f32, Table)| {
                let color = table_to_color(&color)?;
                cmds.borrow_mut()
                    .push(DrawCommand::Rect { x0, y0, x1, y1, color });
                Ok(())
            },
        )?,
    )?;

    // Pixel text (Monogram, see pixel_font.rs): drawn as rects, so it
    // blends and clips exactly like rect() does.
    globals.set("FONT_HEIGHT", pixel_font::CELL_HEIGHT)?;
    let cmds = Rc::clone(commands);
    globals.set(
        "text",
        lua.create_function(
            move |_, (x, y, text, color, height): (f32, f32, String, Table, Option<f32>)| {
                let color = table_to_color(&color)?;
                let height = text_height(height)?;
                let mut cmds = cmds.borrow_mut();
                pixel_font::layout(&text, x, y, height, |x0, y0, x1, y1| {
                    cmds.push(DrawCommand::Rect { x0, y0, x1, y1, color });
                });
                Ok(pixel_font::measure(&text, height))
            },
        )?,
    )?;
    globals.set(
        "text_size",
        lua.create_function(|_, (text, height): (String, Option<f32>)| {
            Ok(pixel_font::measure(&text, text_height(height)?))
        })?,
    )?;

    let cmds = Rc::clone(commands);
    globals.set(
        "pixel",
        lua.create_function(move |_, (x, y, color): (i32, i32, Table)| {
            let color = table_to_color(&color)?;
            cmds.borrow_mut().push(DrawCommand::Pixel { x, y, color });
            Ok(())
        })?,
    )?;

    let active = Rc::clone(notes);
    globals.set(
        "active_notes",
        lua.create_function(move |lua, ()| {
            active_notes_to_table(lua, &active.borrow().active)
        })?,
    )?;

    let upcoming = Rc::clone(notes);
    globals.set(
        "upcoming_notes",
        lua.create_function(move |lua, ()| {
            upcoming_notes_to_table(lua, &upcoming.borrow().upcoming)
        })?,
    )?;

    let channels = Rc::clone(notes);
    globals.set(
        "midi_channels",
        lua.create_function(move |lua, ()| {
            let detected = &channels.borrow().detected_channels;
            let array = lua.create_table_with_capacity(detected.len(), 0)?;
            for (i, &c) in detected.iter().enumerate() {
                array.raw_set(i + 1, c)?;
            }
            Ok(array)
        })?,
    )?;

    let channels = Rc::clone(notes);
    globals.set(
        "channel_enabled",
        lua.create_function(move |_, channel: i64| {
            let notes = channels.borrow();
            if notes.detected_channels.is_empty() {
                return Ok(true); // nothing detected yet, don't filter anything
            }
            let index = channel.clamp(0, 15) as usize;
            Ok(notes.enabled_channels.get(index).copied().unwrap_or(true))
        })?,
    )?;

    let requests = Rc::clone(channel_requests);
    globals.set(
        "set_channel_enabled",
        lua.create_function(move |_, (channel, enabled): (i64, bool)| {
            requests.borrow_mut().push((channel.clamp(0, 15) as u8, enabled));
            Ok(())
        })?,
    )?;

    let left = Rc::clone(spectrum_left);
    globals.set(
        "fft_left",
        lua.create_function(move |lua, samples: Table| {
            let samples = table_to_vec(&samples)?;
            let magnitudes = left.borrow_mut().analyze(&samples);
            vec_to_table(lua, &magnitudes)
        })?,
    )?;

    let right = Rc::clone(spectrum_right);
    globals.set(
        "fft_right",
        lua.create_function(move |lua, samples: Table| {
            let samples = table_to_vec(&samples)?;
            let magnitudes = right.borrow_mut().analyze(&samples);
            vec_to_table(lua, &magnitudes)
        })?,
    )?;

    let view = Rc::clone(playback);
    let song = Rc::clone(song);
    globals.set(
        "playback",
        lua.create_function(move |lua, ()| {
            let view = view.borrow();
            let song = song.borrow();
            let t = lua.create_table_with_capacity(0, 13)?;
            t.raw_set("position", view.position)?;
            t.raw_set("length", view.length)?;
            t.raw_set("speed", view.speed)?;
            t.raw_set("paused", view.paused)?;
            t.raw_set("finished", view.finished)?;
            t.raw_set("loop_enabled", view.loop_enabled)?;
            t.raw_set("generation", view.generation)?;
            t.raw_set("song_name", view.track_name.clone())?;
            t.raw_set("song_path", song.path.clone())?;
            t.raw_set("song_id", song.id.clone())?;
            t.raw_set("song_loads", song.loads)?;
            t.raw_set("recording", song.recording)?;
            t.raw_set(
                "loop_mode",
                match song.transport.loop_mode {
                    LoopMode::Off => "off",
                    LoopMode::One => "one",
                    LoopMode::All => "all",
                },
            )?;
            t.raw_set("shuffle", song.transport.shuffle)?;
            Ok(t)
        })?,
    )?;

    let history = Rc::clone(log);
    globals.set(
        "log",
        lua.create_function(move |_, message: String| {
            history.borrow_mut().push_from_script(message, false);
            Ok(())
        })?,
    )?;

    let history = Rc::clone(log);
    globals.set(
        "log_app",
        lua.create_function(move |_, message: String| {
            history.borrow_mut().push_from_script(message, true);
            Ok(())
        })?,
    )?;

    // This does NOT grant the script Lua's own `debug` library
    // (StdLib::DEBUG is still excluded above), it's a host function that
    // happens to use the debug *C* API on the host side to read the calling
    // frame's locals, same trust boundary as every other host function here.
    let snapshot = Rc::clone(debug_snapshot);
    globals.set(
        "debug_locals",
        lua.create_function(move |lua, label: Option<String>| {
            let vars = capture_caller_locals(lua);
            *snapshot.borrow_mut() = Some(DebugSnapshot { label, vars });
            Ok(())
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "setting_bool",
        lua.create_function(move |_, (key, default, labels): (String, bool, Option<Table>)| {
            let labels = item_labels("setting_bool", labels)?;
            let mut store = store.borrow_mut();
            let value = store.get_or_register_bool(&key, default);
            store.label(&key, labels);
            Ok(value)
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "setting_int",
        lua.create_function(move |_, (key, default, min, max, labels): (String, i64, i64, i64, Option<Table>)| {
            let labels = item_labels("setting_int", labels)?;
            let mut store = store.borrow_mut();
            let value = store.get_or_register_int(&key, default, min, max);
            store.label(&key, labels);
            Ok(value)
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "setting_float",
        lua.create_function(move |_, (key, default, min, max, labels): (String, f64, f64, f64, Option<Table>)| {
            let labels = item_labels("setting_float", labels)?;
            let mut store = store.borrow_mut();
            let value = store.get_or_register_float(&key, default, min, max);
            store.label(&key, labels);
            Ok(value)
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "setting_color",
        lua.create_function(move |lua, (key, default, labels): (String, Table, Option<Table>)| {
            let labels = item_labels("setting_color", labels)?;
            let default = table_to_rgb(&default)?;
            let mut store = store.borrow_mut();
            let value = store.get_or_register_color(&key, default);
            store.label(&key, labels);
            rgb_to_table(lua, value)
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "setting_string",
        lua.create_function(move |_, (key, default, labels): (String, String, Option<Table>)| {
            let labels = item_labels("setting_string", labels)?;
            let mut store = store.borrow_mut();
            let value = store.get_or_register_string(&key, &default);
            store.label(&key, labels);
            Ok(value)
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "settings_preset",
        lua.create_function(move |_, (name, values): (String, Table)| {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(mlua::Error::runtime("settings_preset: give it a name"));
            }
            let mut map = serde_json::Map::new();
            for pair in values.pairs::<String, Value>() {
                let (key, value) = pair?;
                let json = match value {
                    Value::Boolean(b) => serde_json::Value::Bool(b),
                    Value::Integer(i) => serde_json::Value::from(i),
                    Value::Number(n) => serde_json::Value::from(n),
                    Value::String(s) => serde_json::Value::String(s.to_str()?.to_string()),
                    Value::Table(t) if t.contains_key("r")? => {
                        let [r, g, b] = table_to_rgb(&t)?;
                        serde_json::json!({ "r": r, "g": g, "b": b })
                    }
                    Value::Table(t) => serde_json::Value::from(table_to_strings(&t)?),
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "settings_preset: {key} can't be a {}",
                            other.type_name()
                        )));
                    }
                };
                map.insert(key, json);
            }
            let mut store = store.borrow_mut();
            let preset = SettingsPreset { name, values: map };
            match store.presets.iter_mut().find(|p| p.name == preset.name) {
                Some(existing) => *existing = preset,
                None => store.presets.push(preset),
            }
            Ok(())
        })?,
    )?;

    let store = Rc::clone(settings);
    globals.set(
        "setting_selection",
        lua.create_function(
            move |lua, (key, options, defaults, max_selections, labels): (String, Table, Table, i64, Option<Table>)| {
                let labels = item_labels("setting_selection", labels)?;
                let options = table_to_strings(&options)?;
                let defaults = table_to_strings(&defaults)?;
                let max_selections = max_selections.max(1) as usize;
                let mut store = store.borrow_mut();
                let value = store.get_or_register_selection(&key, options, defaults, max_selections);
                store.label(&key, labels);
                strings_to_table(lua, &value)
            },
        )?,
    )?;

    Ok(())
}

/// A color table `{r, g, b}` or `{r, g, b, a}` packed as `0xAARRGGBB`. `a` is
/// 0.0..1.0 and defaults to 1.0 (fully opaque, overwrites).
fn table_to_color(t: &Table) -> mlua::Result<u32> {
    let r: f32 = t.get("r")?;
    let g: f32 = t.get("g")?;
    let b: f32 = t.get("b")?;
    let a: Option<f32> = t.get("a")?;
    let byte = |v: f32| v.clamp(0.0, 255.0) as u32;
    let alpha = (a.unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u32;
    Ok((alpha << 24) | (byte(r) << 16) | (byte(g) << 8) | byte(b))
}

/// A plain `{r, g, b}` table (no alpha) as used by `setting_color`.
fn table_to_rgb(t: &Table) -> mlua::Result<[u8; 3]> {
    let r: f32 = t.get("r")?;
    let g: f32 = t.get("g")?;
    let b: f32 = t.get("b")?;
    let byte = |v: f32| v.clamp(0.0, 255.0) as u8;
    Ok([byte(r), byte(g), byte(b)])
}

fn rgb_to_table(lua: &Lua, [r, g, b]: [u8; 3]) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(0, 3)?;
    t.raw_set("r", r)?;
    t.raw_set("g", g)?;
    t.raw_set("b", b)?;
    Ok(t)
}

fn table_to_strings(t: &Table) -> mlua::Result<Vec<String>> {
    let len = t.raw_len();
    let mut out = Vec::with_capacity(len);
    for i in 1..=len {
        out.push(t.get(i)?);
    }
    Ok(out)
}

fn strings_to_table(lua: &Lua, values: &[String]) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(values.len(), 0)?;
    for (i, v) in values.iter().enumerate() {
        t.raw_set(i + 1, v.clone())?;
    }
    Ok(t)
}

fn active_notes_to_table(lua: &Lua, notes: &[ActiveNote]) -> mlua::Result<Table> {
    let array = lua.create_table_with_capacity(notes.len(), 0)?;
    for (i, note) in notes.iter().enumerate() {
        let entry = lua.create_table_with_capacity(0, 4)?;
        entry.raw_set("channel", note.channel)?;
        entry.raw_set("key", note.key)?;
        entry.raw_set("velocity", note.velocity)?;
        entry.raw_set("source", if note.from_script { "script" } else { "song" })?;
        array.raw_set(i + 1, entry)?;
    }
    Ok(array)
}

fn upcoming_notes_to_table(lua: &Lua, notes: &[NoteChange]) -> mlua::Result<Table> {
    let array = lua.create_table_with_capacity(notes.len(), 0)?;
    for (i, note) in notes.iter().enumerate() {
        let entry = lua.create_table_with_capacity(0, 5)?;
        entry.raw_set("channel", note.channel)?;
        entry.raw_set("key", note.key)?;
        entry.raw_set("velocity", note.velocity)?;
        entry.raw_set("on", note.on)?;
        entry.raw_set("seconds_until", note.seconds_until)?;
        array.raw_set(i + 1, entry)?;
    }
    Ok(array)
}

fn table_to_vec(t: &Table) -> mlua::Result<Vec<f32>> {
    let len = t.raw_len();
    let mut out = Vec::with_capacity(len);
    for i in 1..=len {
        out.push(t.get(i)?);
    }
    Ok(out)
}

fn vec_to_table(lua: &Lua, values: &[f32]) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(values.len(), 0)?;
    for (i, &v) in values.iter().enumerate() {
        table.raw_set(i + 1, v)?;
    }
    Ok(table)
}

/// A text line height from a script: `FONT_HEIGHT` when not given; must be
/// a positive, finite number of pixels (capped, so a typo can't ask for
/// gigantic text).
fn text_height(height: Option<f32>) -> mlua::Result<f32> {
    let height = height.unwrap_or(pixel_font::CELL_HEIGHT as f32);
    if !(height.is_finite() && height > 0.0) {
        return Err(mlua::Error::runtime("text height must be a positive number of pixels"));
    }
    Ok(height.min(4096.0))
}

fn rasterize(buffer: &mut [u32], width: usize, height: usize, cmd: DrawCommand, sprites: &[Sprite]) {
    match cmd {
        // `clear` always overwrites (the alpha channel is dropped).
        DrawCommand::Clear(color) => buffer.fill(color & 0x00FF_FFFF),
        DrawCommand::Pixel { x, y, color } => set_pixel(buffer, width, height, x, y, color),
        DrawCommand::Line { x0, y0, x1, y1, color } => {
            draw_line(buffer, width, height, (x0, y0, x1, y1), color);
        }
        DrawCommand::Rect { x0, y0, x1, y1, color } => {
            fill_rect(buffer, width, height, (x0, y0, x1, y1), color);
        }
        DrawCommand::Circle { x, y, radius, color } => fill_circle(buffer, width, height, (x, y, radius), color),
        DrawCommand::Polygon { points, color } => fill_polygon(buffer, width, height, &points, color),
        DrawCommand::Sprite { index, x, y, scale, flip_x, flip_y, src, colors } => {
            if let Some(sprite) = sprites.get(index) {
                draw_sprite(buffer, width, height, sprite, colors.as_deref(), src, (x, y), scale, (flip_x, flip_y));
            }
        }
    }
}

/// Draw the `src` part of `sprite` (x, y, width, height in sprite pixels)
/// with its top-left at `(x, y)`, `scale` times its size,
/// nearest-neighbor: every buffer pixel takes the sprite pixel under its
/// center. Transparent pixels are skipped, opaque ones copied, translucent
/// ones blended. `colors`, when given, recolors it: one color per palette
/// index.
#[allow(clippy::too_many_arguments)]
fn draw_sprite(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    sprite: &Sprite,
    colors: Option<&[u32]>,
    (src_x, src_y, src_w, src_h): (usize, usize, usize, usize),
    (x, y): (f32, f32),
    scale: f32,
    (flip_x, flip_y): (bool, bool),
) {
    let out_w = (src_w as f32 * scale).round();
    let out_h = (src_h as f32 * scale).round();
    if out_w < 1.0 || out_h < 1.0 || src_w == 0 || src_h == 0 {
        return;
    }
    let left = x.round() as i64;
    let top = y.round() as i64;
    let (out_w, out_h) = (out_w as i64, out_h as i64);
    let x_start = left.max(0);
    let x_end = (left + out_w).min(width as i64);
    let y_start = top.max(0);
    let y_end = (top + out_h).min(height as i64);
    if x_start >= x_end || y_start >= y_end {
        return;
    }
    // Which sprite column each visible buffer column shows, worked out once.
    let source = |offset: i64, out: i64, size: usize, flip: bool| -> usize {
        let i = (((offset as f64 + 0.5) * size as f64 / out as f64) as usize).min(size - 1);
        if flip { size - 1 - i } else { i }
    };
    let columns: Vec<usize> =
        (x_start..x_end).map(|bx| src_x + source(bx - left, out_w, src_w, flip_x)).collect();
    for by in y_start..y_end {
        let sy = src_y + source(by - top, out_h, src_h, flip_y);
        let row = sy * sprite.width..(sy + 1) * sprite.width;
        let dst_row = &mut buffer[by as usize * width + x_start as usize..by as usize * width + x_end as usize];
        let put = |dst: &mut u32, color: u32| match color >> 24 {
            0 => {}
            255 => *dst = color & 0x00FF_FFFF,
            _ => *dst = blend(*dst, color),
        };
        match colors {
            None => {
                let src_row = &sprite.pixels[row];
                for (dst, &sx) in dst_row.iter_mut().zip(&columns) {
                    put(dst, src_row[sx]);
                }
            }
            Some(colors) => {
                let src_row = &sprite.indices[row];
                for (dst, &sx) in dst_row.iter_mut().zip(&columns) {
                    put(dst, colors[src_row[sx] as usize]);
                }
            }
        }
    }
}

/// A filled circle: every pixel whose center is within `radius` of
/// `(cx, cy)`. Each row is one span, so a translucent circle blends each
/// pixel exactly once.
fn fill_circle(buffer: &mut [u32], width: usize, height: usize, (cx, cy, radius): (f32, f32, f32), color: u32) {
    if radius.is_nan() || radius <= 0.0 {
        return;
    }
    let top = (cy - radius - 0.5).floor().max(0.0) as usize;
    let bottom = ((cy + radius + 0.5).ceil().max(0.0) as usize).min(height);
    for row in top..bottom {
        let dy = row as f32 + 0.5 - cy;
        let span = radius * radius - dy * dy;
        if span < 0.0 {
            continue;
        }
        let half = span.sqrt();
        fill_row_span(buffer, width, row, cx - half, cx + half, color);
    }
}

/// A filled polygon (even-odd rule, so self-intersecting shapes get
/// holes): every pixel whose center is inside it, one span per crossing
/// pair per row.
fn fill_polygon(buffer: &mut [u32], width: usize, height: usize, points: &[(f32, f32)], color: u32) {
    if points.len() < 3 {
        return;
    }
    let min_y = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_y = points.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
    let top = (min_y - 0.5).floor().max(0.0) as usize;
    let bottom = ((max_y + 0.5).ceil().max(0.0) as usize).min(height);
    let mut crossings: Vec<f32> = Vec::new();
    for row in top..bottom {
        let y = row as f32 + 0.5;
        crossings.clear();
        for i in 0..points.len() {
            let (ax, ay) = points[i];
            let (bx, by) = points[(i + 1) % points.len()];
            if (ay <= y && y < by) || (by <= y && y < ay) {
                crossings.push(ax + (y - ay) * (bx - ax) / (by - ay));
            }
        }
        crossings.sort_by(f32::total_cmp);
        #[allow(clippy::chunks_exact_to_as_chunks)] // like the rest of the code
        for pair in crossings.chunks_exact(2) {
            fill_row_span(buffer, width, row, pair[0], pair[1], color);
        }
    }
}

/// Fill the pixels in `row` whose centers lie in `[x0, x1)`.
fn fill_row_span(buffer: &mut [u32], width: usize, row: usize, x0: f32, x1: f32, color: u32) {
    let first = (x0 - 0.5).ceil();
    let end = (x1 - 0.5).ceil();
    if end <= first {
        return;
    }
    fill_rect(buffer, width, row + 1, (first, row as f32, end, row as f32 + 1.0), color);
}

/// Alpha-composite `src` (`0xAARRGGBB`) over `dst` (`0x00RRGGBB`), returning
/// `0x00RRGGBB`. `src` alpha 255 overwrites, 0 leaves `dst` untouched.
fn blend(dst: u32, src: u32) -> u32 {
    let a = src >> 24;
    if a >= 255 {
        return src & 0x00FF_FFFF;
    }
    if a == 0 {
        return dst;
    }
    let inv = 255 - a;
    let channel = |shift: u32| {
        let s = (src >> shift) & 0xFF;
        let d = (dst >> shift) & 0xFF;
        (s * a + d * inv + 127) / 255
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Fill an axis-aligned rectangle, clipped to the buffer.
fn fill_rect(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    color: u32,
) {
    let to_x = |v: f32| v.round().clamp(0.0, width as f32) as usize;
    let to_y = |v: f32| v.round().clamp(0.0, height as f32) as usize;
    let (xa, xb) = (to_x(x0.min(x1)), to_x(x0.max(x1)));
    let (ya, yb) = (to_y(y0.min(y1)), to_y(y0.max(y1)));
    let alpha = color >> 24;
    if alpha == 0 {
        return;
    }
    for y in ya..yb {
        let row = y * width;
        if alpha >= 255 {
            buffer[row + xa..row + xb].fill(color & 0x00FF_FFFF);
        } else {
            for pixel in &mut buffer[row + xa..row + xb] {
                *pixel = blend(*pixel, color);
            }
        }
    }
}

fn set_pixel(buffer: &mut [u32], width: usize, height: usize, x: i32, y: i32, color: u32) {
    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
        return;
    }
    let index = y as usize * width + x as usize;
    buffer[index] = blend(buffer[index], color);
}

/// Bresenham's line algorithm, in integer pixel space.
fn draw_line(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    color: u32,
) {
    let (mut x0, mut y0) = (x0.round() as i32, y0.round() as i32);
    let (x1, y1) = (x1.round() as i32, y1.round() as i32);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        set_pixel(buffer, width, height, x0, y0, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(freq: f32, sample_rate: f32, len: usize, phase: &mut f32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                let sample = phase.sin();
                *phase += 2.0 * std::f32::consts::PI * freq / sample_rate;
                sample
            })
            .collect()
    }

    fn sample_notes() -> NotesSnapshot {
        NotesSnapshot {
            active: vec![
                ActiveNote { channel: 0, key: 60, velocity: 100, from_script: false },
                ActiveNote { channel: 1, key: 64, velocity: 80, from_script: false },
                ActiveNote { channel: 0, key: 21, velocity: 40, from_script: false },
                ActiveNote { channel: 9, key: 108, velocity: 127, from_script: true },
            ],
            upcoming: vec![
                NoteChange { channel: 0, key: 67, velocity: 90, on: true, seconds_until: 0.4 },
                NoteChange { channel: 0, key: 67, velocity: 0, on: false, seconds_until: 1.1 },
                NoteChange { channel: 2, key: 48, velocity: 70, on: true, seconds_until: 2.3 },
                // A note-on with no matching note-off inside the window.
                NoteChange { channel: 3, key: 84, velocity: 55, on: true, seconds_until: 3.7 },
            ],
            detected_channels: vec![0, 1, 2, 3, 9],
            // Channels 1 and 3 disabled, to exercise the two-pass path.
            enabled_channels: [
                true, false, true, false, true, true, true, true, true, true, true, true, true,
                true, true, true,
            ],
        }
    }

    fn sample_playback() -> EngineView {
        EngineView {
            track_name: Some("test".to_string()),
            soundfont_name: None,
            has_midi: true,
            has_audio_file: false,
            has_soundfont: true,
            position: 12.5,
            length: 180.0,
            speed: 1.0,
            paused: false,
            finished: false,
            loop_enabled: false,
            generation: 0,
            loads: 1,
        }
    }

    /// `debug_locals()` should capture real locals (and parameters, which
    /// `lua_getlocal` treats the same way) in scope at its call site,
    /// recurse into table values, and skip Lua's own internal temporaries.
    #[test]
    fn debug_locals_captures_scope_at_the_call_site() {
        let source = r#"
function render(width, height, left, right)
    local x = 42
    local label = "hello"
    local t = { a = 1, b = 2 }
    debug_locals("test point")
end
"#;
        let mut visualizer = LuaVisualizer::new(source.to_string(), None, 44_100);
        assert!(visualizer.error().is_none(), "failed to compile: {:?}", visualizer.error());
        assert!(visualizer.debug_snapshot().is_none(), "no snapshot before the first render");

        let mut buffer = vec![0u32; 16 * 16];
        visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());

        let snapshot = visualizer.debug_snapshot().expect("debug_locals should have captured a snapshot");
        assert_eq!(snapshot.label.as_deref(), Some("test point"));

        let find = |name: &str| snapshot.vars.iter().find(|v| v.name == name);
        assert_eq!(find("x").map(|v| v.value.as_str()), Some("42"));
        assert_eq!(find("label").map(|v| v.value.as_str()), Some("\"hello\""));
        let t = find("t").expect("table local `t` should be captured");
        assert_eq!(t.children.len(), 2, "t = {{a=1, b=2}} should have 2 entries");
    }

    /// Feed a bundled default script several frames of synthetic audio + note
    /// data and make sure it compiles and never hits a Lua runtime error, a
    /// script shipped as a default should never itself be the thing showing
    /// an error banner. `name` is just for a clearer panic message when this
    /// runs over every bundled script in a loop.
    fn assert_script_runs_cleanly(name: &str, source: &str, notes: &NotesSnapshot) {
        let mut visualizer = LuaVisualizer::new(source.to_string(), None, 44_100);
        assert!(
            visualizer.error().is_none(),
            "{name}: failed to compile: {:?}",
            visualizer.error()
        );

        let mut buffer = vec![0u32; 64 * 32];
        let mut phase = 0.0f32;
        let playback = sample_playback();
        for _ in 0..8 {
            let mono = sine_wave(440.0, 44_100.0, 512, &mut phase);
            let samples: Vec<StereoFrame> = mono.iter().map(|&s| (s, s)).collect();
            visualizer.render(&mut buffer, 64, 32, &samples, notes, &playback, &VisualizerInput::default());
            assert!(
                visualizer.error().is_none(),
                "{name}: runtime error: {:?}",
                visualizer.error()
            );
        }
    }

    /// Every bundled script, dropped or not, against both the empty and
    /// the populated note paths, with no per-script test function to
    /// remember to add: `build.rs` already means a new file under
    /// `assets/visualizers/{dropped,undropped}` needs no Rust-side edit to
    /// be picked up as a script; this is the same guarantee for its tests.
    #[test]
    fn bundled_scripts_run_cleanly() {
        for &(_, name, source) in BUNDLED_SCRIPTS {
            assert_script_runs_cleanly(name, source, &NotesSnapshot::default());
            assert_script_runs_cleanly(name, source, &sample_notes());
        }
    }

    #[test]
    fn blank_script_template_runs_cleanly() {
        assert_script_runs_cleanly("blank template", BLANK_SCRIPT_TEMPLATE, &NotesSnapshot::default());
    }

    /// Looks up a bundled script's source by file name, for tests that
    /// exercise one specific script's behavior rather than every script
    /// generically.
    fn find_script(file_name: &str) -> &'static str {
        BUNDLED_SCRIPTS
            .iter()
            .find(|&&(_, name, _)| name == file_name)
            .map(|&(_, _, source)| source)
            .unwrap_or_else(|| panic!("no bundled script named {file_name}"))
    }

    /// A bad edit's compile error must stay visible through `error()` even
    /// after the still-running (older) script renders several more
    /// successful frames. This was the actual reported bug: the error
    /// disappeared within one frame because a successful render from the
    /// fallback script was clearing it.
    #[test]
    fn input_functions_see_the_frames_input() {
        let script = r#"
function render(w, h, l, r)
    local x, y = mouse()
    log("mouse " .. tostring(x) .. " " .. tostring(y))
    log("left " .. tostring(mouse_down()) .. " right " .. tostring(mouse_pressed("right")))
    log("a " .. tostring(key_down("A")) .. " space " .. tostring(key_pressed("space")) .. " esc " .. tostring(key_down("escape")))
    log("focus " .. tostring(has_focus()) .. " mode " .. display_mode())
    set_cursor_visible(false)
    set_cursor_locked(true)
end"#;
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        assert_eq!(visualizer.cursor_request(), CursorRequest::default());
        let input = VisualizerInput {
            mode: crate::visualizer::DisplayMode::Dedicated,
            focused: true,
            pointer: Some((3.0, 4.0)),
            buttons_down: [true, false, false],
            buttons_pressed: [false, true, false],
            keys_down: vec![egui::Key::A, egui::Key::Escape],
            keys_pressed: vec![egui::Key::Space],
            ..Default::default()
        };
        let mut buffer = vec![0u32; 16 * 16];
        visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &sample_playback(), &input);
        assert_eq!(visualizer.error(), None);

        let messages: Vec<String> = visualizer.log_entries().into_iter().map(|e| e.message).collect();
        assert_eq!(
            messages,
            [
                "mouse 3.0 4.0",
                "left true right true",
                "a true space true esc false",
                "focus true mode dedicated",
            ]
        );
        assert_eq!(visualizer.cursor_request(), CursorRequest { hidden: true, confined: true });
    }

    #[test]
    fn restart_runs_the_running_script_fresh_even_after_a_broken_edit() {
        let script = "local frames = 0
function render(w, h, l, r) frames = frames + 1; log('frame ' .. frames) end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut buffer = vec![0u32; 4];
        let mut frame = |v: &mut LuaVisualizer| {
            v.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        };
        frame(&mut visualizer);
        frame(&mut visualizer);
        assert_eq!(visualizer.log_entries().last().unwrap().message, "frame 2");

        // A broken edit leaves the old script running; restarting restarts
        // that one, and the edit's error stays reported.
        visualizer.set_source("function render(".to_string());
        assert!(visualizer.restart());
        frame(&mut visualizer);
        assert_eq!(visualizer.log_entries().last().unwrap().message, "frame 1");
        assert!(visualizer.error().is_some());
    }

    #[test]
    fn reserved_keys_never_reach_scripts() {
        let script = "function render(w, h, l, r) log(tostring(key_down('escape')) .. tostring(key_down('f5')) .. tostring(key_down('F11')) .. tostring(key_down('f6'))) end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let input = VisualizerInput {
            focused: true,
            keys_down: vec![egui::Key::Escape, egui::Key::F5, egui::Key::F11, egui::Key::F6],
            ..Default::default()
        };
        let mut buffer = vec![0u32; 4];
        visualizer.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), &sample_playback(), &input);
        assert_eq!(visualizer.log_entries().last().unwrap().message, "falsefalsefalsetrue");
    }

    #[test]
    fn error_line_finds_where_compile_and_runtime_errors_point() {
        let mut visualizer = LuaVisualizer::new("-- one\n-- two\nfunction render(\n".to_string(), None, 44_100);
        let compile = visualizer.error().unwrap().to_string();
        assert_eq!(error_line(&compile), Some(4), "{compile}");

        visualizer.set_source("function render(w, h, l, r)\n  local x = 1\n  nope()\nend".to_string());
        let mut buffer = vec![0u32; 4];
        visualizer.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        let runtime = visualizer.error().unwrap().to_string();
        assert_eq!(error_line(&runtime), Some(3), "{runtime}");

        assert_eq!(error_line("failed to save C:/x.lua: access denied"), None);
    }

    #[test]
    fn rename_moves_the_script_and_its_settings() {
        let dir = std::env::temp_dir().join(format!("synththing-rename-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("untitled-1.lua");
        fs::write(&old, "function render() end").unwrap();
        fs::write(sidecar_path(&old), "{}").unwrap();
        fs::write(dir.join("taken.lua"), "").unwrap();

        assert!(rename_script(&old, "bad/name").is_err());
        assert!(rename_script(&old, "con").is_err());
        assert!(rename_script(&old, "  ").is_err());
        assert!(rename_script(&old, "taken").is_err());

        let new = rename_script(&old, " my visualizer ").unwrap();
        assert_eq!(new, dir.join("my visualizer.lua"));
        assert!(new.exists() && !old.exists());
        assert!(sidecar_path(&new).exists() && !sidecar_path(&old).exists());
        // Case-only rename of the same file is allowed.
        assert!(rename_script(&new, "My Visualizer").is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    fn frame(visualizer: &mut LuaVisualizer, playback: &EngineView) {
        let mut buffer = vec![0u32; 4];
        visualizer.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), playback, &VisualizerInput::default());
    }

    fn last_log(visualizer: &LuaVisualizer) -> String {
        visualizer.log_entries().last().map(|e| e.message.clone()).unwrap_or_default()
    }

    #[test]
    fn notes_between_returns_whole_notes() {
        // One track, 480 ticks/quarter at the default 120 bpm: middle C from
        // 0 to 0.5 s on channel 0.
        let mut smf = b"MThd\0\0\0\x06\0\0\0\x01\x01\xE0MTrk".to_vec();
        let track = [0x00, 0x90, 60, 100, 0x83, 0x60, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00];
        smf.extend((track.len() as u32).to_be_bytes());
        smf.extend(track);
        let script = "function render(w, h, l, r)
            local notes = notes_between(0.25, 1.0)
            local n = notes[1]
            log(#notes .. ' ' .. n.id .. ' ' .. n.channel .. ' ' .. n.key .. ' ' .. n.velocity .. ' ' .. n.start .. ' ' .. n.stop)
            log(#notes_between(0.5, 1.0))
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        visualizer.set_note_list(Arc::new(NoteList::from_smf(&smf).unwrap()));
        frame(&mut visualizer, &sample_playback());
        assert_eq!(visualizer.error(), None);
        let messages: Vec<String> = visualizer.log_entries().into_iter().map(|e| e.message).collect();
        assert_eq!(messages, ["1 1 0 60 100 0.0 0.5", "0"]);
    }

    #[test]
    fn frame_time_generation_and_playback_requests() {
        let script = "function render(w, h, l, r)
            log(FRAME .. ' ' .. tostring(TIME >= 0) .. ' ' .. playback().generation)
            if FRAME == 2 then set_paused(true); seek(12.5) end
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut playback = sample_playback();
        playback.generation = 7;
        frame(&mut visualizer, &playback);
        assert_eq!(last_log(&visualizer), "1 true 7");
        assert!(visualizer.take_playback_requests().is_empty());
        frame(&mut visualizer, &playback);
        assert_eq!(last_log(&visualizer), "2 true 7");
        assert_eq!(
            visualizer.take_playback_requests(),
            [PlaybackRequest::Pause(true), PlaybackRequest::Seek(12.5)]
        );
        // Restart starts the count over.
        visualizer.restart();
        frame(&mut visualizer, &playback);
        assert_eq!(last_log(&visualizer), "1 true 7");
    }

    #[test]
    fn scripts_play_notes_and_sequences_on_the_songs_channels() {
        let script = "
            local riff = sequence_register({ 60, { key = 64, length = 0.5 }, {}, { key = 67, at = 4, channel = 5, velocity = 30 } })
            function render()
                if FRAME == 1 then
                    log(tostring(notes_playable()))
                    play_note(60.4, { velocity = 200, duration = 0.25 })   -- no channel: the song's lowest
                    note_on(62, { channel = 5 })
                    note_off(62, 5)
                    log(sequence_play(riff, { tempo = 120, transpose = 2, channel = 9 }))
                elseif FRAME == 2 then
                    sequence_stop(1)
                    stop_notes()
                end
            end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        assert_eq!(visualizer.take_live_commands(), [LiveCommand::StopAll], "a new script starts quiet");
        let notes = NotesSnapshot { detected_channels: vec![2, 5], ..NotesSnapshot::default() };
        let mut buffer = vec![0u32; 4];
        let mut run = |visualizer: &mut LuaVisualizer| {
            visualizer.render(&mut buffer, 2, 2, &[], &notes, &sample_playback(), &VisualizerInput::default());
        };
        run(&mut visualizer);
        assert_eq!(visualizer.error(), None);
        let note = |delay: f64, length: Option<f64>, channel: u8, key: u8, velocity: u8, tag: u64| LiveNote {
            delay,
            length,
            channel,
            key,
            velocity,
            tag,
        };
        // 120 bpm: half a second a beat. Channel 9 isn't in the song, so the
        // sequence falls back to channel 2; the last note asks for 5, which is.
        assert_eq!(
            visualizer.take_live_commands(),
            [
                LiveCommand::Play(vec![note(0.0, Some(0.25), 2, 60, 127, 0)]),
                LiveCommand::Play(vec![note(0.0, None, 5, 62, 100, 0)]),
                LiveCommand::Release { channel: 5, key: 62 },
                LiveCommand::Play(vec![
                    note(0.0, Some(0.5), 2, 62, 100, 1),
                    note(0.5, Some(0.25), 2, 66, 100, 1),
                    note(2.0, Some(0.5), 5, 69, 30, 1),
                ]),
            ]
        );
        assert_eq!(last_log(&visualizer), "1");
        run(&mut visualizer);
        assert_eq!(visualizer.take_live_commands(), [LiveCommand::Stop(1), LiveCommand::StopAll]);

        // No soundfont (or a plain audio file): nothing to play on.
        let mut silent = LuaVisualizer::new("function render() log(tostring(play_note(60))) end".into(), None, 44_100);
        silent.take_live_commands();
        let mut buffer = vec![0u32; 4];
        let view = EngineView { has_soundfont: false, ..sample_playback() };
        silent.render(&mut buffer, 2, 2, &[], &notes, &view, &VisualizerInput::default());
        assert_eq!(last_log(&silent), "false");
        assert!(silent.take_live_commands().is_empty());
    }

    #[test]
    fn active_notes_say_whether_the_script_played_them() {
        let script = "function render()
            local sources = {}
            for _, n in ipairs(active_notes()) do sources[#sources + 1] = n.key .. '=' .. n.source end
            log(table.concat(sources, ' '))
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut buffer = vec![0u32; 4];
        visualizer.render(&mut buffer, 2, 2, &[], &sample_notes(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(last_log(&visualizer), "60=song 64=song 21=song 108=script");
    }

    /// The editor's checks on every bundled script: they should be clean,
    /// so a warning in the editor means something.
    #[test]
    fn bundled_scripts_pass_the_editor_checks() {
        let known = crate::lua_analysis::known_globals(host_global_names());
        let mut problems = Vec::new();
        for &(_, name, source) in BUNDLED_SCRIPTS {
            if name == "editor_test.lua" {
                continue; // has problems on purpose; checked below
            }
            for d in crate::lua_analysis::analyze(source, &known).diagnostics {
                problems.push(format!("{name}:{}: {}", d.line, d.message));
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));

        // The editor test script has exactly the three it says it has.
        let &(_, _, test) = BUNDLED_SCRIPTS.iter().find(|(_, n, _)| *n == "editor_test.lua").unwrap();
        let found: Vec<String> =
            crate::lua_analysis::analyze(test, &known).diagnostics.iter().map(|d| d.message.clone()).collect();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].contains("`spare` is never used"), "{found:?}");
        assert!(found[1].contains("Did you mean `playback`"), "{found:?}");
        assert!(found[2].contains("global `stray`"), "{found:?}");
    }

    #[test]
    fn keyboard_example_plays_along() {
        use egui::Key;
        let source = include_str!("../assets/visualizers/visualizers/keyboard.lua");
        let mut visualizer = LuaVisualizer::new(source.to_string(), None, 44_100);
        visualizer.take_live_commands();
        let notes = NotesSnapshot { detected_channels: vec![3, 4], ..NotesSnapshot::default() };
        let mut buffer = vec![0u32; 320 * 200];
        let mut run = |visualizer: &mut LuaVisualizer, input: VisualizerInput| {
            visualizer.render(&mut buffer, 320, 200, &[], &notes, &sample_playback(), &input);
            assert_eq!(visualizer.error(), None);
            visualizer.take_live_commands()
        };
        let held = |key: u8| LiveCommand::Play(vec![LiveNote { delay: 0.0, length: None, channel: 3, key, velocity: 100, tag: 0 }]);
        let release = |key: u8| LiveCommand::Release { channel: 3, key };

        // A plays the span's first note, C4, until it's let go.
        assert_eq!(run(&mut visualizer, keys(&[Key::A], &[Key::A], &[])), [held(60)]);
        assert_eq!(run(&mut visualizer, keys(&[Key::A], &[], &[])), []);
        assert_eq!(run(&mut visualizer, keys(&[], &[], &[Key::A])), [release(60)]);
        // Right moves the span up a step: A is now D4, L is E5.
        run(&mut visualizer, keys(&[Key::ArrowRight], &[Key::ArrowRight], &[]));
        assert_eq!(run(&mut visualizer, keys(&[Key::A, Key::L], &[Key::A, Key::L], &[])), [held(62), held(76)]);
        // Up changes the key to C# major, one semitone up; held notes keep
        // their own key until released.
        assert_eq!(run(&mut visualizer, keys(&[Key::ArrowUp], &[Key::ArrowUp], &[])), [release(62), release(76)]);
        assert_eq!(run(&mut visualizer, keys(&[Key::A], &[Key::A], &[])), [held(63)]);
        // Losing focus lets go.
        let unfocused = VisualizerInput { focused: false, ..VisualizerInput::default() };
        assert_eq!(run(&mut visualizer, unfocused), [release(63)]);
    }

    #[test]
    fn scripts_see_the_playlist_and_move_through_it() {
        let script = "function render()
            local list = playlist()
            if not list then log('none') return end
            local p = playback()
            log(list.name .. '|' .. tostring(list.current) .. '|' .. #list.entries .. '|' .. list.entries[2].name
                .. '|' .. tostring(list.entries[2].length) .. '|' .. p.loop_mode .. '|' .. tostring(p.shuffle))
            if FRAME == 2 then
                play_track(2); next_track(); previous_track(); set_speed(1.5); set_loop('all'); set_shuffle(true)
            end
            if FRAME == 3 then play_track(3) end
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "none");
        let entry = |name: &str, length| PlaylistEntryView { name: name.into(), length, missing: false };
        visualizer.set_transport(Transport {
            loop_mode: LoopMode::One,
            shuffle: false,
            playlist: Some(PlaylistView {
                name: "Mix".into(),
                playing: true,
                current: Some(0),
                entries: vec![entry("intro", Some(61.5)), entry("outro", None)],
            }),
        });
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "Mix|1|2|outro|nil|one|false");
        assert_eq!(
            visualizer.take_playback_requests(),
            [
                PlaybackRequest::PlayTrack(1),
                PlaybackRequest::NextTrack,
                PlaybackRequest::PreviousTrack,
                PlaybackRequest::Speed(1.5),
                PlaybackRequest::Loop(LoopMode::All),
                PlaybackRequest::Shuffle(true),
            ]
        );
        // Only songs in the list.
        frame(&mut visualizer, &sample_playback());
        assert!(visualizer.error().unwrap_or_default().contains("the playlist has 2 songs"), "{:?}", visualizer.error());
        assert!(visualizer.take_playback_requests().is_empty());
    }

    #[test]
    fn bundled_scripts_are_added_once_each() {
        let dir = std::env::temp_dir().join(format!("synththing-seed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let added = |dir: &Path| -> Vec<String> {
            let mut names: Vec<String> = list_scripts(dir).iter().map(|p| nice_name(p)).collect();
            names.sort();
            names
        };
        // A fresh install: every visualizer and game, no templates or examples.
        seed_bundled(&dir).unwrap();
        let fresh = added(&dir);
        assert!(fresh.contains(&"highway".to_string()) && fresh.contains(&"waveform".to_string()), "{fresh:?}");
        assert!(!fresh.contains(&"player".to_string()) && !fresh.contains(&"editor_test".to_string()), "{fresh:?}");
        // One the user deleted stays deleted.
        fs::remove_file(dir.join("snake.lua")).unwrap();
        seed_bundled(&dir).unwrap();
        assert!(!added(&dir).contains(&"snake".to_string()));
        // An install from 0.1.8 (empty marker) gets only what's new since.
        fs::remove_dir_all(&dir).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".seeded"), "").unwrap();
        seed_bundled(&dir).unwrap();
        assert_eq!(added(&dir), ["highway", "note_runner"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The gamepad example with a controller in use: buttons held and
    /// pressed, sticks and triggers pushed.
    #[test]
    fn gamepad_example_draws_a_controller_in_use() {
        let mut visualizer = LuaVisualizer::new(find_script("gamepad_demo.lua").to_string(), None, 44_100);
        let mut input = keys(&[], &[], &[]);
        input.pads.down = vec![PadInput::A, PadInput::LeftStickLeft, PadInput::RightTrigger];
        input.pads.pressed = vec![PadInput::A];
        input.pads.axes = [-0.8, 0.2, 0.0, -0.5, 0.0, 0.9];
        input.pads.connected = vec!["Test Pad".into()];
        let mut buffer = vec![0u32; 960 * 600];
        for _ in 0..3 {
            visualizer.render(&mut buffer, 960, 600, &[], &NotesSnapshot::default(), &sample_playback(), &input);
            assert_eq!(visualizer.error(), None);
        }
        assert_eq!(visualizer.actions().len(), 21);
        assert!(visualizer.actions().iter().all(|a| a.defaults.iter().any(|b| matches!(b, Binding::Pad(_)))));
    }

    #[test]
    fn the_watchdog_stops_runaway_scripts() {
        // An endless loop in render(): stopped, and not run again.
        let script = "function render() log('frame ' .. FRAME) if FRAME == 2 then while true do end end end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        frame(&mut visualizer, &sample_playback());
        let started = Instant::now();
        frame(&mut visualizer, &sample_playback());
        let took = started.elapsed();
        assert!(took >= WATCHDOG_LIMIT && took < WATCHDOG_LIMIT + Duration::from_secs(2), "{took:?}");
        assert!(visualizer.error().unwrap_or_default().contains("watchdog"), "{:?}", visualizer.error());
        let started = Instant::now();
        frame(&mut visualizer, &sample_playback());
        assert!(started.elapsed() < Duration::from_millis(100), "a stopped script isn't run again");
        assert!(last_log(&visualizer).contains("watchdog"));
        // Restarting runs it afresh.
        visualizer.restart();
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "frame 1");
        // An endless loop at the top level is a compile error, not a hang.
        visualizer.set_source("while true do end function render() end".into());
        assert!(visualizer.error().unwrap_or_default().contains("watchdog"), "{:?}", visualizer.error());
    }

    #[test]
    fn coroutines_work_and_the_watchdog_reaches_into_them() {
        let script = "local co = coroutine.create(function() for i = 1, 3 do coroutine.yield(i) end end)
            local stuck = coroutine.create(function() while true do end end)
            function render()
                local _, n = coroutine.resume(co)
                log('got ' .. tostring(n))
                if FRAME == 2 then
                    local ok, err = coroutine.resume(stuck)
                    log(tostring(ok) .. ' ' .. tostring(err))
                end
            end";
        let mut visualizer = LuaVisualizer::with_watchdog(script.to_string(), None, 44_100, Duration::from_millis(300));
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "got 1");
        frame(&mut visualizer, &sample_playback());
        let log = last_log(&visualizer);
        assert!(log.starts_with("false") && log.contains("watchdog"), "{log}");
    }

    #[test]
    fn the_stop_flag_stops_a_busy_script() {
        let script = "function render() log('frame ' .. FRAME) if FRAME == 2 then while true do end end end";
        let mut visualizer = LuaVisualizer::with_watchdog(script.to_string(), None, 44_100, Duration::from_secs(60));
        frame(&mut visualizer, &sample_playback());
        let stop = visualizer.stop_flag();
        let revision = visualizer.log_revision();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            stop.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        frame(&mut visualizer, &sample_playback());
        stopper.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "stopped long before the watchdog");
        assert!(visualizer.error().unwrap_or_default().contains(STOPPED_TAG), "{:?}", visualizer.error());
        assert_ne!(visualizer.log_revision(), revision);
        visualizer.stop_flag().store(false, Ordering::Relaxed);
        frame(&mut visualizer, &sample_playback());
        assert!(visualizer.error().is_some(), "stays stopped until restarted");
        assert!(visualizer.restart());
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "frame 1");
        assert!(visualizer.error().is_none());
    }

    #[test]
    fn script_logs_can_go_to_the_app_log() {
        let mut quiet = LuaVisualizer::new("function render() log('x') end".into(), None, 44_100);
        frame(&mut quiet, &sample_playback());
        let history = quiet.compiled.as_ref().unwrap().log.borrow();
        assert!(!history.to_app_log.get() && !history.script_wants_app_log);
        drop(history);
        // The preference outlives a recompile.
        quiet.set_app_log(true);
        quiet.set_source("function render() log('y') end".into());
        assert!(quiet.compiled.as_ref().unwrap().log.borrow().to_app_log.get());

        let script = "script_options({ app_log = true }) function render() log('z') end";
        let mut asks = LuaVisualizer::new(script.into(), None, 44_100);
        assert!(asks.options().app_log);
        assert!(asks.compiled.as_ref().unwrap().log.borrow().script_wants_app_log);
        let mut history = LogHistory::default();
        assert!(history.push(LogLevel::Info, "a".into()));
        assert!(!history.push(LogLevel::Info, "a".into()), "a repeat isn't new");
        frame(&mut asks, &sample_playback());
        assert_eq!(last_log(&asks), "z");
        // log_app: one message, whatever the settings; it shows in the
        // script's own log too.
        let mut one = LuaVisualizer::new("function render() log_app('once') end".into(), None, 44_100);
        frame(&mut one, &sample_playback());
        assert_eq!(last_log(&one), "once");
        let bad = LuaVisualizer::new("script_options({ app_log = 1 }) function render() end".into(), None, 44_100);
        assert!(bad.error().unwrap_or_default().contains("app_log is true or false"), "{:?}", bad.error());
    }

    #[test]
    fn state_snapshot_finds_top_level_locals_and_own_globals() {
        let script = "local score = 3
            local hidden = { a = 1 }
            local function bump() hidden.a = hidden.a + 1 end
            level = 'one'
            local unused = 9
            function render() score = score + 1; bump() end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        render_with(&mut visualizer, &VisualizerInput::default());
        let closed = visualizer.state_snapshot(&StateRequest::default()).unwrap();
        let names = |vars: &[StateVar]| vars.iter().map(|v| format!("{}={}", v.name, v.value)).collect::<Vec<_>>();
        // `hidden` through bump(), held by render(); `unused` isn't held by anything.
        assert_eq!(names(&closed.locals), ["hidden=table (1 entries)", "score=4"]);
        assert!(closed.locals[0].expandable && closed.locals[0].children.is_empty(), "closed until asked");
        assert_eq!(names(&closed.globals), ["level=\"one\""], "no host functions, no render");
        let mut request = StateRequest::default();
        request.open.insert(vec!["locals".into(), "hidden".into()], STATE_PAGE);
        let open = visualizer.state_snapshot(&request).unwrap();
        assert_eq!(names(&open.locals[0].children), ["a=2"]);
        assert_eq!(open.locals[0].children[0].path, ["locals", "hidden", "\"a\""]);

        // A big table: only as many entries as asked, in order, the rest counted.
        let big = "local t = {} for i = 1, 300 do t[i] = { i } end
            function render() local _ = t end";
        let mut big = LuaVisualizer::new(big.to_string(), None, 44_100);
        render_with(&mut big, &VisualizerInput::default());
        let mut request = StateRequest::default();
        request.open.insert(vec!["locals".into(), "t".into()], STATE_PAGE);
        let state = big.state_snapshot(&request).unwrap();
        let t = &state.locals[0];
        assert_eq!(t.value, "table (300 entries)");
        assert_eq!((t.children.len(), t.more), (STATE_PAGE, 300 - STATE_PAGE));
        assert_eq!((t.children[0].name.as_str(), t.children[9].name.as_str()), ("1", "10"));
        assert!(t.children[0].children.is_empty(), "entries stay closed");
    }

    #[test]
    fn settings_and_controls_take_a_group_and_info() {
        let script = "local speed = setting_float('speed', 1, 0, 2, { group = 'Play', info = 'How fast' })
            setting_bool('grid', true)
            local jump = input_register('jump', 'space', { group = 'Movement' })
            input_register('jump', 'space')
            function render() setting_float('speed', 1, 0, 2) end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        render_with(&mut visualizer, &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        let settings = visualizer.settings();
        assert_eq!(settings[0].labels, ItemLabels { group: Some("Play".into()), info: Some("How fast".into()) });
        assert_eq!(settings[1].labels, ItemLabels::default());
        // A later call without them keeps what the first gave.
        assert_eq!(visualizer.actions()[0].labels.group.as_deref(), Some("Movement"));
        for (bad, message) in [
            ("setting_bool('x', true, { colour = 'red' })", "no option `colour`"),
            ("input_register('x', 'space', { group = 3 })", "group is text"),
        ] {
            let broken = LuaVisualizer::new(format!("{bad} function render() end"), None, 44_100);
            assert!(broken.error().unwrap_or_default().contains(message), "{bad}: {:?}", broken.error());
        }
    }

    #[test]
    fn script_options_and_song_loads() {
        let script = "script_options({ start_paused = true })
            function render() log(playback().song_loads) end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        assert_eq!(visualizer.error(), None);
        assert!(visualizer.options().start_paused);
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "0");
        // The same song twice still counts twice.
        visualizer.set_song(Some(Path::new("a.mid")), Some("x".into()));
        visualizer.set_song(Some(Path::new("a.mid")), Some("x".into()));
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "2");
        // playback().recording follows set_recording.
        visualizer.set_source("function render() log(tostring(playback().recording)) end".into());
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "false");
        visualizer.set_recording(true, None);
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "true");
        // recording(): nil outside a recording, else what it asks.
        visualizer.set_source(
            "script_options({ record_prepare = true, record_auto = true })
            function render()
                local r = recording()
                log(r and (r.phase .. ' ' .. r.kind .. ' ' .. r.mode) or 'none')
                if r and r.phase == 'preparing' then recording_ready() else recording_done() end
            end"
            .into(),
        );
        assert!(visualizer.options().record_prepare && visualizer.options().record_auto);
        visualizer.set_recording(false, None);
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "none");
        let take = RecordingTake { phase: RecordPhase::Preparing, kind: RecordKind::Playlist, auto: true };
        visualizer.set_recording(false, Some(take));
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "preparing playlist auto");
        assert!(visualizer.take_playback_requests().contains(&PlaybackRequest::RecordingReady));
        visualizer.set_recording(true, Some(RecordingTake { phase: RecordPhase::Recording, kind: RecordKind::Song, auto: false }));
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "recording song manual");
        assert!(visualizer.take_playback_requests().contains(&PlaybackRequest::RecordingDone));
        // Another script without the option: off again.
        visualizer.set_source("function render() end".into());
        assert!(!visualizer.options().start_paused);
        // Mistakes say what's wrong.
        visualizer.set_source("script_options({ start_paused = 1 }) function render() end".into());
        assert!(visualizer.error().unwrap_or_default().contains("true or false"), "{:?}", visualizer.error());
        visualizer.set_source("script_options({ autoplay = true }) function render() end".into());
        assert!(visualizer.error().unwrap_or_default().contains("no option `autoplay`"), "{:?}", visualizer.error());
    }

    #[test]
    fn store_survives_the_script_being_reloaded() {
        let dir = std::env::temp_dir().join(format!("synththing-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("game.lua");
        let script = "function render(w, h, l, r)
            local best = store_get('best') or 0
            store_set('best', best + 1)
            store_set('table', { 1, 2, { name = 'x' } })
            local t = store_get('table')
            log(best .. ' ' .. #t .. ' ' .. t[3].name .. ' ' .. tostring(store_get('missing')))
            store_set('gone', nil)
        end";
        fs::write(&path, script).unwrap();

        let mut first = LuaVisualizer::new(script.to_string(), Some(path.clone()), 44_100);
        frame(&mut first, &sample_playback());
        frame(&mut first, &sample_playback());
        assert_eq!(last_log(&first), "1 3 x nil");
        drop(first); // saves

        let mut second = LuaVisualizer::new(script.to_string(), Some(path.clone()), 44_100);
        frame(&mut second, &sample_playback());
        assert_eq!(second.error(), None);
        assert_eq!(last_log(&second), "2 3 x nil");
        assert!(store_path(&path).exists());

        // Functions can't be stored.
        second.set_source("function render() store_set('f', print) end".to_string());
        frame(&mut second, &sample_playback());
        assert!(second.error().unwrap_or_default().contains("can't save"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_draws_inside_the_size_it_returns() {
        let script = "function render(w, h, l, r)
            clear({ r = 0, g = 0, b = 0 })
            local tw, th = text(0, 0, 'Hi', { r = 255, g = 255, b = 255 }, FONT_HEIGHT)
            local mw, mh = text_size('Hi', 24)
            log(tw .. ' ' .. th .. ' ' .. mw .. ' ' .. mh)
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let (w, h) = (32, 16);
        let mut buffer = vec![0u32; w * h];
        visualizer.render(&mut buffer, w, h, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        assert_eq!(last_log(&visualizer), "11.0 12.0 22.0 24.0");
        let lit: Vec<(usize, usize)> =
            (0..w * h).filter(|&i| buffer[i] == 0x00FF_FFFF).map(|i| (i % w, i / w)).collect();
        assert!(!lit.is_empty());
        assert!(lit.iter().all(|&(x, y)| x < 11 && y < 12), "{lit:?}");
    }

    #[test]
    fn shapes_fill_the_right_pixels_once() {
        let script = "function render(w, h, l, r)
            clear({ r = 0, g = 0, b = 0 })
            circle(8, 8, 4, { r = 255, g = 0, b = 0 })
            triangle(20, 2, 30, 2, 20, 12, { r = 0, g = 255, b = 0 })
            polygon({ 2, 20, 12, 20, 12, 30, 2, 30 }, { r = 0, g = 0, b = 255, a = 0.5 })
            polygon({ { x = 20, y = 20 }, { x = 30, y = 20 }, { 30, 30 } }, { r = 255, g = 255, b = 255 })
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let w = 32;
        let mut buffer = vec![0u32; w * w];
        visualizer.render(&mut buffer, w, w, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        let at = |x: usize, y: usize| buffer[y * w + x];
        // Circle: center filled, corners of its box not.
        assert_eq!(at(8, 8), 0xFF0000);
        assert_eq!(at(5, 5), 0xFF0000);
        assert_eq!(at(4, 4), 0);
        // Triangle: inside its right angle, not past the hypotenuse.
        assert_eq!(at(21, 3), 0x00FF00);
        assert_eq!(at(29, 11), 0);
        // Translucent square: blended exactly once everywhere (no seams).
        let blue = at(2, 20);
        assert!(blue != 0 && (2..12).all(|x| (20..30).all(|y| at(x, y) == blue)));
        assert_eq!(at(12, 25), 0);
        // Point-list polygon (mixed {x=,y=} and {x, y} forms).
        assert_eq!(at(29, 21), 0xFFFFFF);
        assert_eq!(at(21, 29), 0);
    }

    #[test]
    fn timing_functions_follow_the_song() {
        // One note, default tempo (120 bpm) and time signature (4/4).
        let mut smf = b"MThd\0\0\0\x06\0\0\0\x01\x01\xE0MTrk".to_vec();
        let track = [0x00, 0x90, 60, 100, 0x83, 0x60, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00];
        smf.extend((track.len() as u32).to_be_bytes());
        smf.extend(track);
        let script = "function render(w, h, l, r)
            local bar, into = bar(2.75)
            local num, den = time_signature()
            log(beat() .. ' ' .. beat(1.0) .. ' ' .. time_at_beat(3) .. ' ' .. tempo() .. ' ' .. bar .. ' ' .. into .. ' ' .. num .. '/' .. den)
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        visualizer.set_note_list(Arc::new(NoteList::from_smf(&smf).unwrap()));
        let mut playback = sample_playback();
        playback.position = 0.25;
        frame(&mut visualizer, &playback);
        assert_eq!(visualizer.error(), None);
        assert_eq!(last_log(&visualizer), "0.5 2.0 1.5 120.0 2 1.5 4/4");

        // No song timing (plain audio): nil, not an error.
        let mut visualizer = LuaVisualizer::new(
            "function render() log(tostring(beat()) .. ' ' .. tostring(tempo()) .. ' ' .. tostring(bar())) end".to_string(),
            None,
            44_100,
        );
        frame(&mut visualizer, &playback);
        assert_eq!(last_log(&visualizer), "nil nil nil");
    }

    #[test]
    fn audio_levels_and_onsets_reach_scripts() {
        let script = "function render(w, h, l, r)
            local hit, strength = onset()
            log(string.format('%.2f %.2f %s', level_left(), level_right(), tostring(hit)))
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut buffer = vec![0u32; 4];
        let quiet = vec![(0.0, 0.0); 735];
        let loud: Vec<StereoFrame> = (0..735).map(|i| ((i as f32 * 0.3).sin() * 0.8, 0.25)).collect();
        let mut hits = 0;
        for n in 0..40 {
            let samples = if n < 30 { &quiet } else { &loud };
            visualizer.render(&mut buffer, 2, 2, samples, &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
            hits += usize::from(last_log(&visualizer).ends_with("true"));
        }
        assert_eq!(visualizer.error(), None);
        // A 0.8-amplitude sine is ~0.566 RMS; a constant 0.25 is 0.25.
        let log = last_log(&visualizer);
        let left: f32 = log.split(' ').next().unwrap().parse().unwrap();
        assert!((left - 0.566).abs() < 0.01 && log.contains(" 0.25 "), "{log}");
        assert_eq!(hits, 1, "one onset when the sound starts");
    }

    #[test]
    fn sprites_register_once_and_draw_scaled_flipped_and_transparent() {
        let script = "
            local red = { r = 255, g = 0, b = 0 }
            local green = { r = 0, g = 255, b = 0, a = 0.5 }
            local id = sprite_register({
                image = { { 1, 0 }, { 2 } },   -- second row is short: transparent at the end
                palette = { red, green },
            })
            function render(w, h, l, r)
                clear({ r = 0, g = 0, b = 255 })
                local sw, sh = sprite_size(id)
                local dw, dh = sprite(id, 0, 0, 2)
                sprite(id, 6, 0, { scale = 1, flip_x = true })
                log(id .. ' ' .. sw .. 'x' .. sh .. ' drawn ' .. dw .. 'x' .. dh)
            end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let w = 8;
        let mut buffer = vec![0u32; w * 4];
        visualizer.render(&mut buffer, w, 4, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        assert_eq!(last_log(&visualizer), "1 2x2 drawn 4.0x4.0");
        let at = |x: usize, y: usize| buffer[y * w + x];
        // Scaled 2x: red block top-left, transparent top-right (blue shows).
        assert_eq!([at(0, 0), at(1, 1)], [0xFF0000, 0xFF0000]);
        assert_eq!(at(2, 0), 0x0000FF);
        // Bottom-left: half-transparent green over blue.
        assert!(at(0, 2) != 0x0000FF && at(0, 2) & 0x00FF00 != 0);
        assert_eq!(at(3, 3), 0x0000FF);
        // Flipped copy at x 6: red now on the right.
        assert_eq!((at(6, 0), at(7, 0)), (0x0000FF, 0xFF0000));

        for bad in [
            "sprite_register({ image = { { 3 } }, palette = { { r = 1, g = 1, b = 1 } } })",
            "sprite_register({ palette = {} })",
            "function render() sprite(5, 0, 0) end",
        ] {
            let mut visualizer = LuaVisualizer::new(format!("{bad}\nfunction render() end"), None, 44_100);
            frame(&mut visualizer, &sample_playback());
            let mut v2 = LuaVisualizer::new(bad.to_string(), None, 44_100);
            frame(&mut v2, &sample_playback());
            assert!(visualizer.error().is_some() || v2.error().is_some(), "{bad}");
        }
    }

    #[test]
    fn sprite_sheets_draw_just_the_named_part() {
        // A 4x2 sheet: two 2x2 frames side by side, red then green.
        let script = "
            local sheet = sprite_register({
                palette = { { r = 255, g = 0, b = 0 }, { r = 0, g = 255, b = 0 } },
                image = { { 1, 1, 2, 2 }, { 1, 1, 2, 2 } },
            })
            function render(w, h, l, r)
                local dw, dh = sprite(sheet, 0, 0, { src = { 2, 0, 2, 2 }, scale = 2 })
                sprite(sheet, 4, 0, { src = { x = 0, y = 0, w = 2, h = 2 } })
                sprite(sheet, 6, 0, { src = { 3, 1, 9, 9 } }) -- clipped to the sheet: 1x1 green
                log(dw .. 'x' .. dh)
            end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let w = 8;
        let mut buffer = vec![0u32; w * 4];
        visualizer.render(&mut buffer, w, 4, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        assert_eq!(last_log(&visualizer), "4.0x4.0");
        let at = |x: usize, y: usize| buffer[y * w + x];
        assert!((0..4).all(|x| (0..4).all(|y| at(x, y) == 0x00FF00)), "second frame, doubled");
        assert_eq!((at(4, 0), at(5, 1)), (0xFF0000, 0xFF0000));
        assert_eq!((at(6, 0), at(7, 0), at(6, 1)), (0x00FF00, 0, 0));
    }

    #[test]
    fn sprites_can_be_recolored_per_draw() {
        // A 2x1 sprite: a red pixel and a white one. Drawn plain, with its
        // red swapped for blue, tinted, and both, all in the same frame.
        let script = "
            local s = sprite_register({
                palette = { { r = 255, g = 0, b = 0 }, { r = 255, g = 255, b = 255 } },
                image = { { 1, 2 } },
            })
            function render(w, h, l, r)
                sprite(s, 0, 0)
                sprite(s, 2, 0, { palette = { [1] = { r = 0, g = 0, b = 255 } } })
                sprite(s, 4, 0, { tint = { r = 255, g = 128, b = 0 } })
                sprite(s, 6, 0, { palette = { [2] = { r = 0, g = 255, b = 0 } }, tint = { r = 0, g = 255, b = 255 } })
            end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut buffer = vec![0u32; 8];
        visualizer.render(&mut buffer, 8, 1, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        assert_eq!(
            buffer,
            [0xFF0000, 0xFFFFFF, 0x0000FF, 0xFFFFFF, 0xFF0000, 0xFF8000, 0x000000, 0x00FF00]
        );

        for (bad, message) in [
            ("{ palette = { [3] = { r = 0, g = 0, b = 0 } } }", "palette has 2 colors"),
            ("{ palette = { red = { r = 0, g = 0, b = 0 } } }", "palette indices"),
            ("{ tint = 5 }", "tint is a color table"),
        ] {
            let script = format!(
                "local s = sprite_register({{ palette = {{ {{ r = 1, g = 1, b = 1 }}, {{ r = 2, g = 2, b = 2 }} }}, image = {{ {{ 1 }} }} }})
                function render() sprite(s, 0, 0, {bad}) end"
            );
            let mut visualizer = LuaVisualizer::new(script, None, 44_100);
            let mut buffer = vec![0u32; 4];
            visualizer.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
            let error = visualizer.error().unwrap_or_default();
            assert!(error.contains(message), "{bad}: {error}");
        }
    }

    #[test]
    fn playback_names_the_song() {
        let script = "function render()
            local p = playback()
            log(tostring(p.song_name) .. '|' .. tostring(p.song_path) .. '|' .. tostring(p.song_id))
        end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        frame(&mut visualizer, &sample_playback());
        assert_eq!(last_log(&visualizer), "test|nil|nil");
        visualizer.set_song(Some(Path::new("C:/music/song.mid")), Some(crate::loader::song_id(b"MThd")));
        frame(&mut visualizer, &sample_playback());
        // The id is the start of the file's SHA-256, so the same bytes
        // always give the same id, wherever the file is.
        assert_eq!(last_log(&visualizer), "test|C:/music/song.mid|70fad3c7454a1f31");
    }

    fn render_with(visualizer: &mut LuaVisualizer, input: &VisualizerInput) {
        let mut buffer = vec![0u32; 4];
        visualizer.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), &sample_playback(), input);
    }

    fn keys(down: &[egui::Key], pressed: &[egui::Key], released: &[egui::Key]) -> VisualizerInput {
        VisualizerInput {
            focused: true,
            keys_down: down.to_vec(),
            keys_pressed: pressed.to_vec(),
            keys_released: released.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn actions_report_their_state_and_keep_saved_bindings() {
        use egui::Key;
        let dir = std::env::temp_dir().join(format!("synththing-actions-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("game.lua");
        let script = "
            local jump = input_register('jump', 'space')
            local left = input_register('left', { 'a', 'left' })
            local again = input_register('jump', 'x') -- same name: same action
            function render()
                log(input(jump) .. ' ' .. input(left) .. ' ' .. tostring(input_down(left)) .. ' ' .. tostring(jump == again))
            end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), Some(path.clone()), 44_100);
        render_with(&mut visualizer, &keys(&[Key::Space, Key::A], &[Key::Space], &[]));
        assert_eq!(visualizer.error(), None);
        assert_eq!(last_log(&visualizer), "pressed held true true");
        render_with(&mut visualizer, &keys(&[Key::ArrowLeft], &[Key::ArrowLeft], &[Key::Space]));
        assert_eq!(last_log(&visualizer), "released pressed true true");
        render_with(&mut visualizer, &keys(&[], &[], &[]));
        assert_eq!(last_log(&visualizer), "up up false true");

        // Rebind jump to J; a reload of the script keeps it.
        let actions = visualizer.actions();
        assert_eq!(actions[1].bindings, [Binding::Key(Key::A), Binding::Key(Key::ArrowLeft)]);
        visualizer.set_action_bindings(0, vec![Binding::Key(Key::J), Binding::Pad(PadInput::Y)]);
        let mut reloaded = LuaVisualizer::new(script.to_string(), Some(path.clone()), 44_100);
        assert_eq!(reloaded.actions()[0].bindings, [Binding::Key(Key::J), Binding::Pad(PadInput::Y)]);
        assert_eq!(reloaded.actions()[0].defaults, [Binding::Key(Key::Space)]);
        render_with(&mut reloaded, &keys(&[Key::Space], &[Key::Space], &[]));
        assert_eq!(last_log(&reloaded).split(' ').next(), Some("up"), "space no longer jumps");
        // The controller button it's bound to now does.
        let mut pad = keys(&[], &[], &[]);
        pad.pads.down = vec![PadInput::Y];
        pad.pads.pressed = vec![PadInput::Y];
        render_with(&mut reloaded, &pad);
        assert_eq!(last_log(&reloaded).split(' ').next(), Some("pressed"));

        // Defaults can name controller inputs, and analogue axes read directly.
        let script = "local fire = input_register('fire', { 'space', 'pad_a', 'pad_south' })
            function render() log(#gamepads() .. ' ' .. input(fire) .. ' ' .. pad_axis('lstick_x')) end";
        let mut pads = LuaVisualizer::new(script.to_string(), None, 44_100);
        assert_eq!(pads.actions()[0].defaults, [Binding::Key(Key::Space), Binding::Pad(PadInput::A)], "aliases don't double up");
        let mut input = keys(&[], &[], &[]);
        input.pads.down = vec![PadInput::A];
        input.pads.axes = [-0.5, 0.0, 0.0, 0.0, 0.0, 0.0];
        input.pads.connected = vec!["Xbox Controller".into()];
        render_with(&mut pads, &input);
        assert_eq!(last_log(&pads), "1 held -0.5");

        // Mouse buttons as bindings: states and values like keys.
        let script = "local fire = input_register('fire', { 'space', 'mouse_left', 'MOUSE_RIGHT' })
            function render() log(input(fire) .. ' ' .. input_value(fire)) end";
        let mut mouse = LuaVisualizer::new(script.to_string(), None, 44_100);
        assert_eq!(
            mouse.actions()[0].defaults,
            [Binding::Key(Key::Space), Binding::Mouse(0), Binding::Mouse(1)],
            "case doesn't matter"
        );
        assert_eq!(Binding::Mouse(1).name(), "mouse_right");
        let mut input = keys(&[], &[], &[]);
        input.buttons_down = [false, true, false];
        input.buttons_pressed = [false, true, false];
        render_with(&mut mouse, &input);
        assert_eq!(last_log(&mouse), "pressed 1.0");
        let mut bad_mouse = LuaVisualizer::new("input_register('x', 'mouse_4') function render() end".into(), None, 44_100);
        render_with(&mut bad_mouse, &VisualizerInput::default());
        assert!(bad_mouse.error().unwrap_or_default().contains("unknown mouse button"), "{:?}", bad_mouse.error());

        // input_value: how far, from any binding; keys count fully.
        let script = "local left = input_register('left', { 'a', 'pad_lstick_left' })
            local right = input_register('right', { 'd', 'pad_lstick_right' })
            local gas = input_register('gas', { 'pad_rt', 'w' })
            function render() log(string.format('%.2f %.2f %.2f', input_value(left), input_value(right), input_value(gas))) end";
        let mut steer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut input = keys(&[], &[], &[]);
        input.pads.axes = [-0.4, 0.0, 0.0, 0.0, 0.0, 0.25];
        render_with(&mut steer, &input);
        assert_eq!(last_log(&steer), "0.40 0.00 0.25");
        let mut input = keys(&[Key::D], &[], &[]);
        input.pads.axes = [-0.4, 0.0, 0.0, 0.0, 0.0, 0.0];
        input.pads.down = vec![PadInput::RightTrigger];
        render_with(&mut steer, &input);
        assert_eq!(last_log(&steer), "0.40 1.00 1.00", "a key counts fully; a button-only trigger counts fully");
        steer.set_source("function render() input_value(9) end".into());
        render_with(&mut steer, &VisualizerInput::default());
        assert!(steer.error().unwrap_or_default().contains("input_value: no action with id 9"), "{:?}", steer.error());
        let mut bad_pad = LuaVisualizer::new("input_register('x', 'pad_banana') function render() end".into(), None, 44_100);
        assert!(bad_pad.error().unwrap_or_default().contains("unknown controller input"), "{:?}", bad_pad.error());
        bad_pad.set_source("function render() pad_axis('wheel') end".into());
        render_with(&mut bad_pad, &VisualizerInput::default());
        assert!(bad_pad.error().unwrap_or_default().contains("no axis 'wheel'"), "{:?}", bad_pad.error());

        // Reserved keys can't be bound.
        let mut bad = LuaVisualizer::new("input_register('quit', 'escape') function render() end".to_string(), None, 44_100);
        render_with(&mut bad, &VisualizerInput::default());
        assert!(bad.error().unwrap_or_default().contains("reserved"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_spans_edit_text_and_block_controls() {
        use crate::typing::TextEvent;
        use egui::{Key, Modifiers};
        let script = "
            local jump = input_register('jump', 'space')
            typing_begin('hi')
            function render()
                local s = typing_state()
                log(string.format('%s|%d|%s|%s|%s|%s|%s', s.text, s.cursor, tostring(s.active), tostring(s.done),
                    input(jump), tostring(key_down('space')), text_typed()))
            end";
        let mut visualizer = LuaVisualizer::new(script.to_string(), None, 44_100);
        let mut input = keys(&[Key::Space], &[Key::Space], &[]);
        input.text_events = vec![
            TextEvent::Text(" there".into()),
            TextEvent::Key(Key::ArrowLeft, Modifiers::NONE),
            TextEvent::Key(Key::Backspace, Modifiers::NONE),
        ];
        render_with(&mut visualizer, &input);
        assert_eq!(visualizer.error(), None);
        // Typing: controls are blocked even though space is down.
        assert_eq!(last_log(&visualizer), "hi thee|6|true|false|up|false| there");

        let mut enter = keys(&[], &[], &[]);
        enter.text_events = vec![TextEvent::Key(Key::Enter, Modifiers::NONE)];
        render_with(&mut visualizer, &enter);
        assert_eq!(last_log(&visualizer), "hi thee|6|false|true|up|false|");
        // Span over: controls work again.
        render_with(&mut visualizer, &keys(&[Key::Space], &[Key::Space], &[]));
        assert!(last_log(&visualizer).ends_with("pressed|true|"), "{}", last_log(&visualizer));
    }

    #[test]
    fn terminal_example_runs_lines_and_recalls_history() {
        use crate::typing::TextEvent;
        use egui::{Key, Modifiers};
        let script = format!(
            "{}\nlocal inner = render\nfunction render(...) inner(...) local s = typing_state() log(s.text .. '|' .. s.cursor) end",
            include_str!("../assets/visualizers/templates/terminal.lua")
        );
        let mut visualizer = LuaVisualizer::new(script, None, 44_100);
        let typed = |events: Vec<TextEvent>| VisualizerInput { text_events: events, ..keys(&[], &[], &[]) };
        render_with(&mut visualizer, &keys(&[], &[], &[]));
        render_with(&mut visualizer, &typed(vec![TextEvent::Text("echo hi".into())]));
        assert_eq!(last_log(&visualizer), "echo hi|7");
        render_with(&mut visualizer, &typed(vec![TextEvent::Key(Key::Enter, Modifiers::NONE)]));
        assert_eq!(last_log(&visualizer), "|0");
        render_with(&mut visualizer, &typed(vec![TextEvent::Key(Key::ArrowUp, Modifiers::NONE)]));
        assert_eq!(last_log(&visualizer), "echo hi|7");
        render_with(&mut visualizer, &typed(vec![TextEvent::Key(Key::ArrowDown, Modifiers::NONE)]));
        assert_eq!(last_log(&visualizer), "|0");
        render_with(&mut visualizer, &typed(vec![TextEvent::Text("help".into())]));
        render_with(&mut visualizer, &typed(vec![TextEvent::Key(Key::Enter, Modifiers::NONE)]));
        assert_eq!(visualizer.error(), None);
    }

    #[test]
    fn slow_scripts_drop_to_30_fps_after_three_seconds() {
        let mut perf = Perf::default();
        let t0 = Instant::now();
        // Fast frames: never drops.
        for i in 0..300 {
            assert!(perf.record(t0 + Duration::from_millis(i * 16), 5.0).is_none());
        }
        // Slow for a while, then fast for long enough that the last-second
        // average is back under budget: the run is broken, still 60.
        let slow_start = t0 + Duration::from_secs(10);
        let mut t = slow_start;
        while t < slow_start + Duration::from_millis(1500) {
            assert!(perf.record(t, 25.0).is_none());
            t += Duration::from_millis(25);
        }
        for _ in 0..120 {
            assert!(perf.record(t, 2.0).is_none());
            t += Duration::from_millis(16);
        }
        assert!(!perf.half_rate);
        assert!(perf.slow_since.is_none());
        // Slow for 3 s straight: drops, once.
        let mut messages = 0;
        let start = t;
        while t < start + Duration::from_millis(4200) {
            messages += usize::from(perf.record(t, 25.0).is_some());
            t += Duration::from_millis(25);
        }
        assert!(perf.half_rate);
        assert_eq!(messages, 1);
        let summary = perf.summary();
        assert!(summary.half_rate && (summary.avg_ms - 25.0).abs() < 0.01 && summary.overall_max_ms == 25.0);
    }

    #[test]
    fn frame_rate_fallback_survives_restart_but_not_new_code() {
        let mut visualizer = LuaVisualizer::new("function render() end".to_string(), None, 44_100);
        visualizer.perf.half_rate = true;
        visualizer.restart();
        assert!(visualizer.perf_summary().half_rate, "Restart keeps the fallback");
        visualizer.set_source("function render() end -- edited".to_string());
        assert!(!visualizer.perf_summary().half_rate, "new code is judged afresh");
        visualizer.perf.half_rate = true;
        visualizer.retry_full_frame_rate();
        assert!(!visualizer.perf_summary().half_rate);
    }

    #[test]
    fn unknown_keys_and_buttons_are_script_errors() {
        for call in ["key_down('nope')", "mouse_down('thumb')"] {
            let script = format!("function render(w, h, l, r) {call} end");
            let mut visualizer = LuaVisualizer::new(script, None, 44_100);
            let mut buffer = vec![0u32; 4];
            visualizer.render(&mut buffer, 2, 2, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
            let error = visualizer.error().unwrap_or_default().to_string();
            assert!(error.contains("unknown"), "{call}: {error}");
        }
    }

    #[test]
    fn compile_error_persists_across_successful_renders() {
        let mut visualizer = LuaVisualizer::new(BLANK_SCRIPT_TEMPLATE.to_string(), None, 44_100);
        assert!(visualizer.error().is_none());

        visualizer.set_source("this is not valid lua(".to_string());
        assert!(visualizer.error().is_some(), "a bad edit should report an error");

        let mut buffer = vec![0u32; 16 * 16];
        let playback = sample_playback();
        for _ in 0..5 {
            visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &playback, &VisualizerInput::default());
            assert!(
                visualizer.error().is_some(),
                "a successful render of the fallback script must not clear the standing compile error"
            );
        }

        // Fixing it should clear the standing error.
        visualizer.set_source(BLANK_SCRIPT_TEMPLATE.to_string());
        assert!(visualizer.error().is_none());
    }

    /// A compile error should also land in the (still-running) script's own
    /// log, not just the transient inline banner.
    #[test]
    fn compile_error_is_logged_against_the_running_script() {
        let mut visualizer = LuaVisualizer::new(BLANK_SCRIPT_TEMPLATE.to_string(), None, 44_100);
        visualizer.set_source("this is not valid lua(".to_string());
        let entries = visualizer.log_entries();
        assert!(
            entries.iter().any(|e| e.level == LogLevel::Error && e.message.contains("compile error")),
            "expected a compile-error entry in the log, got: {entries:?}"
        );
    }

    /// A script's registered settings should reflect its declared defaults
    /// the first time it runs, with no sidecar file present.
    #[test]
    fn settings_demo_registers_expected_defaults() {
        let mut visualizer = LuaVisualizer::new(find_script("settings_demo.lua").to_string(), None, 44_100);
        let mut buffer = vec![0u32; 16 * 16];
        visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        let settings = visualizer.settings();
        assert!(!settings.is_empty(), "settings_demo.lua should register settings");
        assert!(settings.iter().any(|d| matches!(d.kind, SettingKind::Bool)));
        assert!(settings.iter().any(|d| matches!(d.kind, SettingKind::Int { .. })));
        assert!(settings.iter().any(|d| matches!(d.kind, SettingKind::Float { .. })));
        assert!(settings.iter().any(|d| matches!(d.kind, SettingKind::Color)));
        assert!(settings.iter().any(|d| matches!(d.kind, SettingKind::String)));
        assert!(settings.iter().any(|d| matches!(d.kind, SettingKind::Selection { .. })));
    }

    /// Changing a setting through `set_setting` should be visible to the
    /// script on the very next frame, with no recompile.
    #[test]
    fn set_setting_applies_without_recompile() {
        let mut visualizer = LuaVisualizer::new(find_script("settings_demo.lua").to_string(), None, 44_100);
        let mut buffer = vec![0u32; 16 * 16];
        let playback = sample_playback();
        visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &playback, &VisualizerInput::default());

        let bool_key = visualizer
            .settings()
            .iter()
            .find(|d| matches!(d.kind, SettingKind::Bool))
            .map(|d| d.key.clone())
            .expect("settings_demo.lua should have a bool setting");

        visualizer.set_setting(&bool_key, SettingValue::Bool(true));
        visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &playback, &VisualizerInput::default());
        let value = visualizer
            .settings()
            .into_iter()
            .find(|d| d.key == bool_key)
            .map(|d| d.value)
            .unwrap();
        assert!(matches!(value, SettingValue::Bool(true)));
    }

    /// `settings_preset` offers presets (the same name again replaces one),
    /// each setting remembers its default, presets' values fit the settings
    /// they're for, and a preset written from changed settings reads back.
    #[test]
    fn settings_presets() {
        let source = "-- A test.\n-- Two lines of comment.\n\n\
                      settings_preset('Fast', { speed = 9, tint = { r = 1, g = 2, b = 3 } })\n\
                      settings_preset('Fast', { speed = 3 })\n\
                      settings_preset('Shapes', { shapes = { 'star', 'nope' }, ['odd key'] = true })\n\
                      function render()\n\
                        setting_int('speed', 2, 1, 5)\n\
                        setting_color('tint', { r = 10, g = 20, b = 30 })\n\
                        setting_selection('shapes', { 'circle', 'star' }, { 'circle' }, 1)\n\
                        setting_bool('odd key', false)\n\
                      end\n";
        let mut visualizer = LuaVisualizer::new(source.to_string(), None, 44_100);
        let mut buffer = vec![0u32; 16 * 16];
        visualizer.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        assert_eq!(visualizer.error(), None);
        let presets = visualizer.presets();
        assert_eq!(presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Fast", "Shapes"]);
        assert_eq!(presets[0].values.len(), 1, "replaced");
        let settings = visualizer.settings();
        let speed = settings.iter().find(|d| d.key == "speed").unwrap();
        assert_eq!(speed.default, SettingValue::Int(2));
        // Made to fit: 9 is past the max; an option that isn't one is dropped.
        assert_eq!(setting_from_json(&speed.kind, &serde_json::json!(9)), Some(SettingValue::Int(5)));
        let shapes = settings.iter().find(|d| d.key == "shapes").unwrap();
        assert_eq!(
            setting_from_json(&shapes.kind, &presets[1].values["shapes"]),
            Some(SettingValue::Selection(vec!["star".into()]))
        );

        // Changed settings, as a preset in the script, under its comment.
        visualizer.set_setting("speed", SettingValue::Int(4));
        visualizer.set_setting("odd key", SettingValue::Bool(true));
        let line = preset_line("Mine", &visualizer.settings());
        assert_eq!(line, "settings_preset(\"Mine\", { speed = 4, [\"odd key\"] = true })");
        let with = with_preset_line(source, "Mine", &line);
        assert!(with.starts_with("-- A test.\n-- Two lines of comment.\n\nsettings_preset(\"Mine\""), "{with}");
        // Again, changed: replaced in place.
        let again = with_preset_line(&with, "Mine", "settings_preset(\"Mine\", { speed = 1 })");
        assert_eq!(again.matches("settings_preset(\"Mine\"").count(), 1);
        let mut reread = LuaVisualizer::new(with, None, 44_100);
        reread.render(&mut buffer, 16, 16, &[], &NotesSnapshot::default(), &sample_playback(), &VisualizerInput::default());
        let mine = reread.presets().into_iter().find(|p| p.name == "Mine").unwrap();
        assert_eq!((mine.values["speed"].as_i64(), mine.values["odd key"].as_bool()), (Some(4), Some(true)));
    }

    /// Not a correctness test, prints how long a `render` call takes for each
    /// bundled script at the app's actual panel size (800x320), with a
    /// realistic worst-case sample count (two audio chunks landing between two
    /// 60fps frames) and a busy note load, so there's a real number behind
    /// "does this keep up at 60fps" rather than a guess. Run with
    /// `cargo test --bin synththing -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints timing, not a pass/fail check"]
    fn bundled_script_render_timing() {
        // ~32 notes held, ~64 upcoming changes, a dense passage.
        let notes = {
            let mut snapshot = NotesSnapshot::default();
            for i in 0..32u8 {
                snapshot.active.push(ActiveNote {
                    channel: i % 6,
                    key: 30 + i * 2,
                    velocity: 90,
                    from_script: false,
                });
            }
            for i in 0..64u8 {
                snapshot.upcoming.push(NoteChange {
                    channel: i % 6,
                    key: 24 + i,
                    velocity: if i % 2 == 0 { 90 } else { 0 },
                    on: i % 2 == 0,
                    seconds_until: 0.1 + i as f32 * 0.05,
                });
            }
            snapshot.detected_channels = vec![0, 1, 2, 3, 4, 5];
            // Half the channels disabled → the keyboard's translucent second
            // pass does per-pixel blending, the slower path.
            snapshot.enabled_channels = [
                true, false, true, false, true, false, true, true, true, true, true, true, true,
                true, true, true,
            ];
            snapshot
        };
        let playback = sample_playback();

        for &(_, name, source) in BUNDLED_SCRIPTS {
            let mut visualizer = LuaVisualizer::new(source.to_string(), None, 44_100);
            assert!(visualizer.error().is_none());

            let mut buffer = vec![0u32; 800 * 320];
            let mut phase = 0.0f32;
            let mut frame = |v: &mut LuaVisualizer| {
                let mono = sine_wave(440.0, 44_100.0, 2048, &mut phase);
                let samples: Vec<StereoFrame> = mono.iter().map(|&s| (s, s)).collect();
                v.render(&mut buffer, 800, 320, &samples, &notes, &playback, &VisualizerInput::default());
            };

            for _ in 0..8 {
                frame(&mut visualizer);
            }
            let frames = 120;
            let start = std::time::Instant::now();
            for _ in 0..frames {
                frame(&mut visualizer);
            }
            println!(
                "{name}: {:.3}ms/frame over {frames} frames (60fps budget: 16.7ms)",
                start.elapsed().as_secs_f64() * 1000.0 / frames as f64
            );
            assert!(visualizer.error().is_none());
        }
    }
}
