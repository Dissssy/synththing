//! The Lua API scripts see: every function and global the app gives them,
//! the tables they take and return, and the `script_options` keys, each
//! with its types, the version it first shipped in and what it does. The
//! one place these are written down; everything else is made from it:
//!
//! - the editor's completions, hover help and signature help
//!   (`lua_completion`, `lua_analysis`),
//! - the oldest app a script runs on (`library::min_app_version`),
//! - the LuaLS definitions written next to the user's scripts, so any
//!   editor with the Lua language server (VS Code's Lua extension, Neovim,
//!   Zed...) knows the API too ([`luals_definitions`]).
//!
//! A test checks this against what a real script VM has, both ways: every
//! global the app registers is here, and everything here is registered.
//!
//! `since` is the release a name first shipped in. Something new gets
//! `"next"` until it ships: cargo-release writes the version in when it
//! makes the release (`release.toml`), like the changelog's Unreleased.
//! Types are LuaLS's (`number`, `integer`, `string`, `boolean`, `any`,
//! `table`, a class below, `X[]` for a list, `A|B` for either).

use std::collections::HashMap;
use std::sync::OnceLock;

/// A function or global the app gives scripts.
pub struct Item {
    pub name: &'static str,
    /// Where it's grouped in the reference (`## Section` there).
    pub section: &'static str,
    pub since: &'static str,
    pub kind: Kind,
    /// One line: what it does or is.
    pub summary: &'static str,
    /// Kept so old scripts work, but not offered or documented: what to
    /// use instead.
    pub deprecated: Option<&'static str>,
}

pub enum Kind {
    Function { params: &'static [Param], returns: &'static [Param] },
    Constant { ty: &'static str },
    /// One the script defines and the app calls (`render`).
    Callback { params: &'static [Param] },
}

/// A parameter, a return value or a table field.
pub struct Param {
    pub name: &'static str,
    pub ty: &'static str,
    pub optional: bool,
    pub doc: &'static str,
}

/// A table shape: what a function takes (options) or gives back.
pub struct Class {
    pub name: &'static str,
    pub summary: &'static str,
    pub fields: &'static [Param],
}

/// A `script_options` key (what it does is its field's doc in the
/// `ScriptOptions` class).
pub struct ScriptOption {
    pub key: &'static str,
    pub since: &'static str,
}

const fn p(name: &'static str, ty: &'static str) -> Param {
    Param { name, ty, optional: false, doc: "" }
}

const fn opt(name: &'static str, ty: &'static str) -> Param {
    Param { name, ty, optional: true, doc: "" }
}

const fn d(name: &'static str, ty: &'static str, doc: &'static str) -> Param {
    Param { name, ty, optional: false, doc }
}

const fn od(name: &'static str, ty: &'static str, doc: &'static str) -> Param {
    Param { name, ty, optional: true, doc }
}

const fn f(
    name: &'static str,
    section: &'static str,
    since: &'static str,
    params: &'static [Param],
    returns: &'static [Param],
    summary: &'static str,
) -> Item {
    Item { name, section, since, kind: Kind::Function { params, returns }, summary, deprecated: None }
}

/// A function kept for old scripts, superseded by `instead`.
const fn old(name: &'static str, section: &'static str, since: &'static str, params: &'static [Param], returns: &'static [Param], summary: &'static str, instead: &'static str) -> Item {
    Item { name, section, since, kind: Kind::Function { params, returns }, summary, deprecated: Some(instead) }
}

const fn c(name: &'static str, section: &'static str, since: &'static str, ty: &'static str, summary: &'static str) -> Item {
    Item { name, section, since, kind: Kind::Constant { ty }, summary, deprecated: None }
}

const DRAWING: &str = "Drawing";
const TEXT: &str = "Text";
const SPRITES: &str = "Sprites";
const AUDIO: &str = "Audio data";
const MIDI: &str = "MIDI data";
const PLAYBACK: &str = "Playback & timing";
const RECORDING: &str = "Recording";
const PLAYLIST: &str = "Playlist";
const MUSICAL: &str = "Musical timing";
const INPUT: &str = "Input";
const CONTROLS: &str = "Controls";
const TYPING: &str = "Typing";
const NOTES: &str = "Playing notes";
const SEQUENCES: &str = "Sequences";
const LOGGING: &str = "Logging";
const DEBUGGING: &str = "Debugging";
const SETTINGS: &str = "Settings";
const SAVED: &str = "Saved data";
const SHARED: &str = "Shared data";
const ASSETS: &str = "Assets";
const GETTING_STARTED: &str = "Getting started";

const COLOR: &str = "Color";
const SIZE: &[Param] = &[p("width", "number"), p("height", "number")];
const BUTTON: &[Param] = &[od("button", "\"left\"|\"right\"|\"middle\"", "\"left\" if not given")];
const LABELS: Param = od("labels", "SettingLabels", "where it's listed in the Settings window, and its hover text");

pub const ITEMS: &[Item] = &[
    // Drawing
    f("clear", DRAWING, "0.1.1", &[p("color", COLOR)], &[], "fill the whole frame (or the clip) with a color"),
    f("line", DRAWING, "0.1.1", &[p("x0", "number"), p("y0", "number"), p("x1", "number"), p("y1", "number"), p("color", COLOR), od("width", "number", "pixels, 1 if not given")], &[], "draw a line; wider than 1 is a band with round ends"),
    f("rect", DRAWING, "0.1.1", &[p("x0", "number"), p("y0", "number"), p("x1", "number"), p("y1", "number"), p("color", COLOR)], &[], "fill an axis-aligned rectangle"),
    f("rect_outline", DRAWING, "0.7.0", &[p("x0", "number"), p("y0", "number"), p("x1", "number"), p("y1", "number"), p("color", COLOR), opt("width", "number")], &[], "just a rectangle's edge, inside it"),
    f("circle", DRAWING, "0.1.6", &[p("x", "number"), p("y", "number"), p("radius", "number"), p("color", COLOR)], &[], "a filled circle"),
    f("circle_outline", DRAWING, "0.7.0", &[p("x", "number"), p("y", "number"), p("radius", "number"), p("color", COLOR), opt("width", "number")], &[], "just a circle's edge, inside the radius"),
    f("triangle", DRAWING, "0.1.6", &[p("x1", "number"), p("y1", "number"), p("x2", "number"), p("y2", "number"), p("x3", "number"), p("y3", "number"), p("color", COLOR)], &[], "a filled triangle"),
    f("polygon", DRAWING, "0.1.6", &[d("points", "number[]|number[][]", "{x1, y1, x2, y2, ...} or {{x, y}, ...}"), p("color", COLOR)], &[], "a filled polygon (even-odd: where it crosses itself is a hole)"),
    f("pixel", DRAWING, "0.1.1", &[p("x", "integer"), p("y", "integer"), p("color", COLOR)], &[], "set one pixel"),
    f("set_clear_color", DRAWING, "0.6.0", &[p("color", COLOR)], &[], "what each frame starts from, blended over the last one; a < 1 leaves trails, a = 0 keeps it"),
    f("shift_frame", DRAWING, "0.7.0", &[p("dx", "integer"), opt("dy", "integer")], &[], "move what's drawn so far (the kept frame) by whole pixels, inside the clip"),
    f("translate", DRAWING, "0.6.0", &[p("dx", "number"), p("dy", "number")], &[], "move everything drawn after this; adds up, push_view/pop_view to undo"),
    f("clip", DRAWING, "0.6.0", &[opt("x0", "number"), opt("y0", "number"), opt("x1", "number"), opt("y1", "number")], &[], "draw only inside this rectangle (where it is after translate); clip() for anywhere"),
    f("push_view", DRAWING, "0.6.0", &[], &[], "save the current translate and clip"),
    f("pop_view", DRAWING, "0.6.0", &[], &[], "go back to the translate and clip push_view saved"),
    f("hsv", DRAWING, "0.6.0", &[d("hue", "number", "degrees, wraps"), d("saturation", "number", "0 to 1"), d("value", "number", "0 to 1"), od("a", "number", "0 to 1")], &[p("color", COLOR)], "a color from hue, saturation and value"),
    f("mix", DRAWING, "0.6.0", &[p("color1", COLOR), p("color2", COLOR), d("t", "number", "0 is color1, 1 is color2")], &[p("color", COLOR)], "a color between two, alpha included"),
    f("approach", PLAYBACK, "0.6.0", &[p("current", "number"), p("target", "number"), d("rate", "number", "per second"), od("rate_down", "number", "per second, going down")], &[p("value", "number")], "smoothing by the second, the same at any frame rate"),
    // Text
    f("text", TEXT, "0.1.5", &[p("x", "number"), p("y", "number"), p("text", "string"), p("color", COLOR), od("height", "number", "line height in pixels, FONT_HEIGHT if not given")], SIZE, "draw pixel text (Monogram; Japanese, Korean and Chinese from Noto), and get the size drawn"),
    f("text_size", TEXT, "0.1.5", &[p("text", "string"), opt("height", "number")], SIZE, "measure text without drawing it"),
    c("FONT_HEIGHT", TEXT, "0.1.5", "integer", "12, the pixel font's own line height; whole multiples stay crisp"),
    // Sprites
    f("sprite_register", SPRITES, "0.1.8", &[p("sprite", "SpriteImage")], &[p("id", "integer")], "register a sprite once, outside render(); palette index 0 is transparent"),
    f("sprite", SPRITES, "0.1.8", &[p("id", "integer"), p("x", "number"), p("y", "number"), od("options", "number|SpriteOptions", "a scale, or options")], SIZE, "draw a sprite, or part of one, optionally recolored; the size drawn"),
    f("sprite_size", SPRITES, "0.1.8", &[p("id", "integer")], SIZE, "a sprite's size in its own pixels"),
    f("blit", DRAWING, "1.6.0", &[d("pixels", "string|integer[]", "palette indices 0-255, a string of bytes or a list"), p("width", "integer"), p("height", "integer"), p("palette", "Color[]"), opt("x", "number"), opt("y", "number"), opt("w", "number"), opt("h", "number")], &[], "a palette-indexed image in one call, stretched to w x h"),
    f("album_art", SPRITES, "1.5.0", &[od("size", "integer", "at most this many pixels across")], &[p("art", "AlbumArt|nil")], "the playing song's art as a sprite, and the color it's mostly made of"),
    // Audio data
    f("fft_left", AUDIO, "0.1.1", &[p("samples", "number[]")], &[p("spectrum", "number[]")], "the left channel's magnitude spectrum (512 entries)"),
    f("fft_right", AUDIO, "0.1.1", &[p("samples", "number[]")], &[p("spectrum", "number[]")], "the right channel's magnitude spectrum"),
    f("fft_bin", AUDIO, "0.6.0", &[p("hz", "number")], &[p("entry", "integer")], "the spectrum entry nearest hz (1 to 512)"),
    f("fft_freq", AUDIO, "0.6.0", &[p("entry", "integer")], &[p("hz", "number")], "the frequency a spectrum entry is at"),
    f("fft_band", AUDIO, "0.6.0", &[p("spectrum", "number[]"), p("lo_hz", "number"), p("hi_hz", "number")], &[p("peak", "number"), p("average", "number")], "the peak and average of the spectrum entries in that range"),
    f("level_left", AUDIO, "0.1.6", &[], &[p("level", "number")], "this frame's left loudness (RMS), 0 when silent"),
    f("level_right", AUDIO, "0.1.6", &[], &[p("level", "number")], "this frame's right loudness (RMS)"),
    f("peak_left", AUDIO, "0.6.0", &[], &[p("peak", "number")], "this frame's loudest left sample, ignoring sign (0 to 1 unless clipping)"),
    f("peak_right", AUDIO, "0.6.0", &[], &[p("peak", "number")], "this frame's loudest right sample"),
    f("onset", AUDIO, "0.1.6", &[], &[p("hit", "boolean"), p("strength", "number")], "whether a sound just started this frame, and how sharply"),
    f("history_left", AUDIO, "0.6.0", &[d("seconds", "number", "up to 4"), od("count", "integer", "spread that many evenly over them")], &[p("samples", "number[]")], "the left channel's last seconds, oldest first"),
    f("history_right", AUDIO, "0.6.0", &[p("seconds", "number"), opt("count", "integer")], &[p("samples", "number[]")], "the same for the right channel"),
    f("note_name", MIDI, "0.7.0", &[p("key", "number"), od("octave", "boolean", "false leaves the octave out")], &[p("name", "string")], "\"C#4\" (60 is middle C, C4)"),
    f("note_freq", MIDI, "0.7.0", &[p("key", "number")], &[p("hz", "number")], "a key's pitch (69, A4, is 440 Hz; fractional keys work)"),
    f("freq_note", MIDI, "0.7.0", &[p("hz", "number")], &[p("key", "number|nil")], "the key at a pitch, fractional (nil for 0 Hz or less)"),
    c("SAMPLE_RATE", PLAYBACK, "0.1.1", "integer", "the engine's sample rate, in Hz"),
    // MIDI data
    f("active_notes", MIDI, "0.1.1", &[], &[p("notes", "ActiveNote[]")], "MIDI notes held right now"),
    f("upcoming_notes", MIDI, "0.1.1", &[], &[p("notes", "UpcomingNote[]")], "note on/off changes within NOTE_LOOKAHEAD"),
    f("notes_between", MIDI, "0.1.4", &[d("t0", "number", "song seconds"), d("t1", "number", "song seconds")], &[p("notes", "SongNote[]")], "whole notes sounding in [t0, t1)"),
    f("midi_channels", MIDI, "0.1.1", &[], &[p("channels", "integer[]")], "the channels the current file uses, sorted"),
    f("channel_enabled", MIDI, "0.1.1", &[p("channel", "integer")], &[p("enabled", "boolean")], "whether a channel's toggle is on"),
    f("set_channel_enabled", MIDI, "0.1.1", &[p("channel", "integer"), p("enabled", "boolean")], &[], "mute or unmute a channel"),
    f("channel_program", MIDI, "0.6.0", &[p("channel", "integer"), od("seconds", "number", "song seconds, now if not given")], &[p("program", "integer"), p("name", "string")], "the instrument a channel plays (General MIDI name; channel 9 is Drums)"),
    f("channel_name", MIDI, "0.6.0", &[p("channel", "integer")], &[p("name", "string|nil")], "the name of the track a channel's notes are in, or nil"),
    f("channel_colors", MIDI, "0.5.1", &[d("colors", "table<integer, Color>", "channel (0-15) to color; {} for the defaults")], &[], "the colors you draw channels in, for the app's channel toggles"),
    c("NOTE_LOOKAHEAD", MIDI, "0.1.1", "number", "song seconds upcoming_notes() looks ahead"),
    // Playback & timing
    f("playback", PLAYBACK, "0.1.1", &[], &[p("playback", "Playback")], "what's playing and where, times in song seconds"),
    f("set_paused", PLAYBACK, "0.1.4", &[p("paused", "boolean")], &[], "pause or resume playback (applied after this frame)"),
    f("seek", PLAYBACK, "0.1.4", &[p("seconds", "number")], &[], "jump to a song position, seek(0) restarts (applied after this frame)"),
    f("set_speed", PLAYBACK, "0.2.0", &[d("speed", "number", "1.0 is normal")], &[], "playback speed (applied after this frame)"),
    f("script_options", PLAYBACK, "0.2.0", &[p("options", "ScriptOptions")], &[], "once, at the top: how the app treats this script"),
    f("display_mode", INPUT, "0.1.1", &[], &[p("mode", "\"window\"|\"dedicated\"|\"mini\"|\"pip\"")], "where the visualizer is showing (\"mini\" is the mini player, \"pip\" picture-in-picture)"),
    f("is_mobile", INPUT, "1.1.0", &[], &[p("mobile", "boolean")], "the app is in its mobile layout (a phone), so maybe draw something lighter"),
    c("DT", PLAYBACK, "0.1.1", "number", "seconds since the previous render() call (0.0 on the first, at most 0.25)"),
    c("TIME", PLAYBACK, "0.1.4", "number", "seconds since this script started, wall clock, uncapped"),
    c("FRAME", PLAYBACK, "0.1.4", "integer", "frames rendered since this script started (1 on the first)"),
    // Recording
    f("recording", RECORDING, "0.4.0", &[], &[p("recording", "Recording|nil")], "the recording the visualizer is part of, or nil"),
    f("recording_ready", RECORDING, "0.4.0", &[], &[], "with record_prepare: done preparing; the video starts with the next frame"),
    f("recording_done", RECORDING, "0.4.0", &[], &[], "with record_prepare: this song's take is over; the recording ends or the playlist moves on"),
    // Playlist
    f("playlist", PLAYLIST, "0.2.0", &[], &[p("playlist", "Playlist|nil")], "the current playlist, read-only, or nil"),
    f("play_track", PLAYLIST, "0.2.0", &[p("index", "integer")], &[], "play the current playlist's index-th song"),
    f("next_track", PLAYLIST, "0.2.0", &[], &[], "the Next button"),
    f("previous_track", PLAYLIST, "0.2.0", &[], &[], "the Previous button"),
    f("set_loop", PLAYLIST, "0.2.0", &[p("mode", "\"off\"|\"one\"|\"all\"")], &[], "the Loop mode"),
    f("set_shuffle", PLAYLIST, "0.2.0", &[p("on", "boolean")], &[], "shuffle the playlist or not"),
    // Musical timing
    f("beat", MUSICAL, "0.1.6", &[od("seconds", "number", "song seconds, now if not given")], &[p("beats", "number|nil")], "song position in quarter notes from 0 (nil without a MIDI score)"),
    f("time_at_beat", MUSICAL, "0.1.6", &[p("beat", "number")], &[p("seconds", "number|nil")], "when a beat falls, in song seconds"),
    f("bar", MUSICAL, "0.1.6", &[opt("seconds", "number")], &[p("bar", "integer|nil"), p("beat_in_bar", "number|nil")], "bar number from 1, beats into it from 0"),
    f("tempo", MUSICAL, "0.1.6", &[opt("seconds", "number")], &[p("bpm", "number|nil")], "quarter notes per minute"),
    f("time_signature", MUSICAL, "0.1.6", &[opt("seconds", "number")], &[p("numerator", "integer|nil"), p("denominator", "integer|nil")], "the time signature"),
    // Input
    f("has_focus", INPUT, "0.1.1", &[], &[p("focused", "boolean")], "whether keys go to this script (clicked into, or dedicated fullscreen)"),
    f("mouse", INPUT, "0.1.1", &[], &[p("x", "number|nil"), p("y", "number|nil")], "the pointer in buffer pixels, or nil while it isn't over the visualizer"),
    f("mouse_delta", INPUT, "0.1.1", &[], &[p("dx", "number"), p("dy", "number")], "pointer movement since last frame, in buffer pixels (raw motion while focused)"),
    f("scroll", INPUT, "0.1.1", &[], &[p("dx", "number"), p("dy", "number")], "scrolling since last frame (positive y is up)"),
    f("mouse_down", INPUT, "0.1.1", BUTTON, &[p("down", "boolean")], "a mouse button is held right now"),
    f("mouse_pressed", INPUT, "0.1.1", BUTTON, &[p("pressed", "boolean")], "a mouse button went down this frame"),
    f("mouse_released", INPUT, "0.1.1", BUTTON, &[p("released", "boolean")], "a mouse button went up this frame"),
    f("touches", INPUT, "1.1.0", &[], &[p("touches", "Touch[]")], "fingers on the visualizer, in buffer pixels"),
    f("set_cursor_visible", INPUT, "0.1.1", &[p("visible", "boolean")], &[], "hide the system cursor over the visualizer (draw your own)"),
    f("set_cursor_locked", INPUT, "0.1.1", &[p("locked", "boolean")], &[], "keep the cursor in the window while the visualizer has focus"),
    // Controls
    f("input_register", CONTROLS, "0.1.8", &[p("name", "string"), d("default_keys", "string|string[]", "a name or a list: keys, \"pad_a\"-style controller inputs or \"mouse_left\"-style buttons"), od("labels", "SettingLabels", "where it's listed in the Controls window, and its hover text")], &[p("id", "integer")], "a rebindable control, once at the top"),
    f("input", CONTROLS, "0.1.8", &[p("id", "integer")], &[p("state", "\"pressed\"|\"held\"|\"released\"|\"up\"")], "the control's state this frame"),
    f("input_down", CONTROLS, "0.1.8", &[p("id", "integer")], &[p("down", "boolean")], "the control is held (pressed this frame or earlier)"),
    f("input_value", CONTROLS, "0.3.1", &[p("id", "integer")], &[p("value", "number")], "how far the control is pressed, 0 to 1 (keys 0 or 1, triggers and sticks how far)"),
    f("pad_axis", CONTROLS, "0.2.1", &[p("name", "\"lstick_x\"|\"lstick_y\"|\"rstick_x\"|\"rstick_y\"|\"lt\"|\"rt\"")], &[p("value", "number")], "a stick (-1 to 1, y down) or trigger (0 to 1), read directly"),
    f("gamepads", CONTROLS, "0.2.1", &[], &[p("names", "string[]")], "the connected game controllers"),
    old("key_down", INPUT, "0.1.1", &[p("key", "string")], &[p("down", "boolean")], "a key is held", "input_register and input_down"),
    old("key_pressed", INPUT, "0.1.1", &[p("key", "string")], &[p("pressed", "boolean")], "a key went down this frame", "input_register and input"),
    old("key_released", INPUT, "0.1.1", &[p("key", "string")], &[p("released", "boolean")], "a key went up this frame", "input_register and input"),
    // Typing
    f("text_typed", TYPING, "0.1.8", &[], &[p("typed", "string")], "the characters typed this frame, while focused"),
    f("typing_begin", TYPING, "0.1.8", &[od("text", "string", "to start from"), opt("options", "TypingOptions")], &[], "start a typing span: the app edits, you draw"),
    f("typing_state", TYPING, "0.1.8", &[], &[p("state", "TypingState")], "the typing span's text, cursor and whether it's done"),
    f("typing_end", TYPING, "0.1.8", &[], &[p("text", "string")], "stop the typing span"),
    // Playing notes
    f("play_note", NOTES, "0.1.8", &[p("key", "integer"), opt("options", "PlayNoteOptions")], &[p("played", "boolean")], "play a note on the song's instrument (MIDI only)"),
    f("note_on", NOTES, "0.1.8", &[p("key", "integer"), opt("options", "NoteOnOptions")], &[p("played", "boolean")], "start a note held until note_off (MIDI only)"),
    f("note_off", NOTES, "0.1.8", &[p("key", "integer"), opt("channel", "integer")], &[], "release a note started with note_on"),
    f("stop_notes", NOTES, "0.1.8", &[], &[], "release every note the script is playing, cancel its sequences"),
    f("notes_playable", NOTES, "0.1.8", &[], &[p("playable", "boolean")], "whether play_note works now (a MIDI song with a soundfont)"),
    f("midi_send", NOTES, "1.6.0", &[d("status", "integer", "e.g. 0xC0 | channel for a program change"), opt("data1", "integer"), opt("data2", "integer")], &[p("sent", "boolean")], "a raw MIDI channel message to the script's synthesizer"),
    f("sound_register", NOTES, "1.6.0", &[d("samples", "string|number[]", "unsigned 8-bit samples (128 is silence), or numbers -1 to 1"), p("sample_rate", "integer")], &[p("id", "integer")], "a sound effect"),
    f("sound_play", NOTES, "1.6.0", &[p("id", "integer"), opt("options", "SoundOptions")], &[p("played", "boolean")], "play a registered sound effect"),
    // Sequences
    f("sequence_register", SEQUENCES, "0.1.8", &[d("notes", "(integer|SequenceNote)[]", "in beats; a bare number is a key")], &[p("id", "integer")], "a sequence of notes, once at the top"),
    f("sequence_play", SEQUENCES, "0.1.8", &[p("id", "integer"), opt("options", "SequenceOptions")], &[p("handle", "integer|nil")], "play a sequence, at the song's tempo by default; nil when notes can't play"),
    f("sequence_stop", SEQUENCES, "0.1.8", &[p("handle", "integer")], &[], "stop one sequence_play"),
    // Logging and debugging
    f("log", LOGGING, "0.1.1", &[p("message", "any")], &[], "append to this script's log (identical repeats collapse)"),
    f("log_app", LOGGING, "0.2.2", &[p("message", "any")], &[], "like log(), and also copied to the app's log (Help > Log...)"),
    f("debug_locals", DEBUGGING, "0.1.1", &[opt("label", "string")], &[], "snapshot the locals in scope right here, shown in the Debug window"),
    // Settings
    f("setting_bool", SETTINGS, "0.1.1", &[p("key", "string"), p("default", "boolean"), LABELS], &[p("value", "boolean")], "a checkbox in the Settings window"),
    f("setting_int", SETTINGS, "0.1.1", &[p("key", "string"), p("default", "integer"), p("min", "integer"), p("max", "integer"), LABELS], &[p("value", "integer")], "a whole-number slider"),
    f("setting_float", SETTINGS, "0.1.1", &[p("key", "string"), p("default", "number"), p("min", "number"), p("max", "number"), LABELS], &[p("value", "number")], "a slider"),
    f("setting_color", SETTINGS, "0.1.1", &[p("key", "string"), p("default", COLOR), od("labels", "ColorSettingLabels", "as for the others, and alpha = true to pick its opacity too")], &[p("value", COLOR)], "a color picker"),
    f("setting_string", SETTINGS, "0.1.1", &[p("key", "string"), p("default", "string"), LABELS], &[p("value", "string")], "a text box"),
    f("setting_selection", SETTINGS, "0.1.1", &[p("key", "string"), p("options", "string[]"), p("defaults", "string[]"), p("max_selections", "integer"), LABELS], &[p("selected", "string[]")], "pick up to max_selections of a list; a pick past it drops the oldest"),
    f("settings_preset", SETTINGS, "0.4.3", &[p("name", "string"), d("values", "table<string, any>", "setting key to value")], &[], "a named set of setting values, picked in the Settings window, once at the top"),
    // Saved data and assets
    f("store_get", SAVED, "0.1.4", &[p("key", "string")], &[p("value", "any")], "the script's saved data, nil if none"),
    f("store_set", SAVED, "0.1.4", &[p("key", "string"), d("value", "any", "nil deletes")], &[], "save script data across restarts"),
    f("shared_get", SHARED, "next", &[p("key", "string")], &[p("value", "any")], "a value shared with Local API programs, nil if none"),
    f("shared_set", SHARED, "next", &[p("key", "string"), d("value", "any", "nil deletes")], &[], "share a value with Local API programs, until the app closes"),
    f("read_asset", ASSETS, "1.6.0", &[d("name", "string", "a plain file name")], &[p("contents", "string|nil")], "a file's bytes from the script's <name>.lua.assets folder"),
    f("list_assets", ASSETS, "1.6.0", &[], &[p("names", "string[]")], "the files in the script's assets folder, sorted"),
    // What the script defines
    Item {
        name: "render",
        section: GETTING_STARTED,
        since: "0.1.1",
        kind: Kind::Callback {
            params: &[
                d("width", "integer", "the frame's size in pixels"),
                p("height", "integer"),
                d("left", "number[]", "the new left samples since the last frame"),
                d("right", "number[]", "the new right samples"),
            ],
        },
        summary: "define this: called once per frame to draw it",
        deprecated: None,
    },
];

pub const CLASSES: &[Class] = &[
    Class {
        name: "Color",
        summary: "r, g and b 0 to 255 (clamped, fractions fine), a 0 to 1 (1 if not given)",
        fields: &[p("r", "number"), p("g", "number"), p("b", "number"), opt("a", "number")],
    },
    Class {
        name: "Playback",
        summary: "what's playing and where (playback()), times in song seconds",
        fields: &[
            p("position", "number"),
            p("length", "number"),
            p("speed", "number"),
            p("paused", "boolean"),
            p("finished", "boolean"),
            p("loop_enabled", "boolean"),
            p("loop_mode", "\"off\"|\"one\"|\"all\""),
            p("shuffle", "boolean"),
            d("generation", "integer", "goes up each time the position jumps: a seek, a loop, a new song"),
            p("song_name", "string|nil"),
            d("title", "string|nil", "from the song's tags"),
            p("artist", "string|nil"),
            p("album", "string|nil"),
            p("song_path", "string|nil"),
            p("song_id", "string|nil"),
            d("song_loads", "integer", "goes up each time a song is loaded, the same one again too"),
            p("recording", "boolean"),
            d("streaming", "boolean", "the local API is streaming the visualizer"),
        ],
    },
    Class {
        name: "Recording",
        summary: "the recording a visualizer is part of (recording())",
        fields: &[
            p("phase", "\"preparing\"|\"recording\""),
            p("kind", "\"song\"|\"playlist\"|\"free\""),
            p("mode", "\"auto\"|\"manual\""),
        ],
    },
    Class {
        name: "Playlist",
        summary: "the current playlist (playlist())",
        fields: &[p("name", "string"), p("playing", "boolean"), d("current", "integer|nil", "the playing entry's index"), p("entries", "PlaylistEntry[]")],
    },
    Class {
        name: "PlaylistEntry",
        summary: "a song in the playlist",
        fields: &[p("name", "string"), d("length", "number|nil", "nil until it's been read"), p("missing", "boolean")],
    },
    Class {
        name: "ActiveNote",
        summary: "a note held right now (active_notes())",
        fields: &[p("channel", "integer"), p("key", "integer"), p("velocity", "integer"), p("source", "\"song\"|\"script\"")],
    },
    Class {
        name: "UpcomingNote",
        summary: "a note on or off coming up (upcoming_notes())",
        fields: &[p("channel", "integer"), p("key", "integer"), p("velocity", "integer"), p("on", "boolean"), p("seconds_until", "number")],
    },
    Class {
        name: "SongNote",
        summary: "a whole note of the song (notes_between()), times in song seconds",
        fields: &[p("id", "integer"), p("channel", "integer"), p("key", "integer"), p("velocity", "integer"), p("start", "number"), p("stop", "number")],
    },
    Class {
        name: "AlbumArt",
        summary: "the playing song's art (album_art())",
        fields: &[p("sprite", "integer"), p("width", "integer"), p("height", "integer"), d("color", "Color", "the color it's mostly made of")],
    },
    Class {
        name: "SpriteImage",
        summary: "a sprite to register (sprite_register())",
        fields: &[d("image", "integer[][]", "rows of palette indices, 0 transparent"), p("palette", "Color[]")],
    },
    Class {
        name: "SpriteOptions",
        summary: "how sprite() draws",
        fields: &[
            opt("scale", "number"),
            opt("flip_x", "boolean"),
            opt("flip_y", "boolean"),
            od("src", "number[]", "{x, y, w, h}: part of the sprite"),
            od("palette", "table<integer, Color>", "palette index to a color to draw it in"),
            od("tint", "Color", "drawn over every pixel"),
        ],
    },
    Class {
        name: "Touch",
        summary: "a finger on the visualizer (touches())",
        fields: &[
            p("id", "integer"),
            p("x", "number"),
            p("y", "number"),
            p("dx", "number"),
            p("dy", "number"),
            p("force", "number|nil"),
            p("pressed", "boolean"),
            p("released", "boolean"),
        ],
    },
    Class {
        name: "TypingOptions",
        summary: "how typing_begin() edits",
        fields: &[opt("max_length", "integer"), opt("multiline", "boolean")],
    },
    Class {
        name: "TypingState",
        summary: "the typing span (typing_state())",
        fields: &[
            p("active", "boolean"),
            p("done", "boolean"),
            p("cancelled", "boolean"),
            p("text", "string"),
            p("cursor", "integer"),
            p("line", "integer"),
            p("column", "integer"),
            p("key", "string|nil"),
        ],
    },
    Class {
        name: "PlayNoteOptions",
        summary: "how play_note() plays",
        fields: &[opt("channel", "integer"), opt("velocity", "integer"), od("duration", "number", "seconds"), od("delay", "number", "seconds")],
    },
    Class {
        name: "NoteOnOptions",
        summary: "how note_on() plays",
        fields: &[opt("channel", "integer"), opt("velocity", "integer")],
    },
    Class {
        name: "SoundOptions",
        summary: "how sound_play() plays",
        fields: &[od("volume", "number", "1 if not given"), od("pan", "number", "-1 (left) to 1 (right)"), od("speed", "number", "2 plays an octave up")],
    },
    Class {
        name: "SequenceNote",
        summary: "a note in a sequence (sequence_register()), in beats",
        fields: &[p("key", "integer"), opt("at", "number"), opt("length", "number"), opt("velocity", "integer"), opt("channel", "integer")],
    },
    Class {
        name: "SequenceOptions",
        summary: "how sequence_play() plays",
        fields: &[opt("channel", "integer"), opt("transpose", "integer"), od("tempo", "number", "bpm; the song's if not given"), od("delay", "number", "seconds")],
    },
    Class {
        name: "SettingLabels",
        summary: "where a setting or control is listed, and its hover text",
        fields: &[od("group", "string", "listed under General if not given"), od("info", "string", "a longer explanation, shown on hover")],
    },
    Class {
        name: "ColorSettingLabels",
        summary: "SettingLabels, and whether the color's opacity is picked too",
        fields: &[opt("group", "string"), opt("info", "string"), od("alpha", "boolean", "the value has an `a`")],
    },
    Class {
        name: "ScriptOptions",
        summary: "how the app treats the script (script_options())",
        fields: &[
            od("start_paused", "boolean", "songs load paused, for the script to start them"),
            od("app_log", "boolean", "log() also goes to the app's log"),
            od("record_prepare", "boolean", "recordings wait for recording_ready() and recording_done()"),
            od("record_auto", "boolean", "it can play itself: the Record window offers Auto"),
            od("transparent", "boolean", "its frames can be see-through"),
            od("assets", "boolean", "it reads data files: its assets folder is made, with a button to open it"),
        ],
    },
];

pub const SCRIPT_OPTIONS: &[ScriptOption] = &[
    ScriptOption { key: "start_paused", since: "0.2.0" },
    ScriptOption { key: "app_log", since: "0.2.2" },
    ScriptOption { key: "record_prepare", since: "0.4.0" },
    ScriptOption { key: "record_auto", since: "0.4.0" },
    ScriptOption { key: "transparent", since: "1.5.0" },
    ScriptOption { key: "assets", since: "1.6.0" },
];

/// Lua's own libraries scripts didn't always have.
pub const LIBRARIES_SINCE: &[(&str, &str)] = &[("coroutine", "0.4.0")];

pub fn item(name: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.name == name)
}

#[cfg(test)]
fn class(name: &str) -> Option<&'static Class> {
    CLASSES.iter().find(|c| c.name == name)
}

/// The version `name` (a function, global, `script_options` key or Lua
/// library) first shipped in, `"next"` if it hasn't yet.
pub fn since(name: &str) -> Option<&'static str> {
    item(name)
        .map(|i| i.since)
        .or_else(|| SCRIPT_OPTIONS.iter().find(|o| o.key == name).map(|o| o.since))
        .or_else(|| LIBRARIES_SINCE.iter().find(|(n, _)| *n == name).map(|(_, s)| *s))
}

/// A `since` as a version: `"next"` is the release after this build's
/// version (any next release has it, so the patch after is the oldest).
pub fn version(since: &str) -> Option<semver::Version> {
    if since == "next" {
        let mut version = semver::Version::parse(crate::build_info::VERSION).ok()?;
        version.patch += 1;
        version.pre = semver::Prerelease::EMPTY;
        return Some(version);
    }
    semver::Version::parse(since).ok()
}

/// Every `since`, of everything.
pub fn all_since() -> impl Iterator<Item = (&'static str, &'static str)> {
    ITEMS
        .iter()
        .map(|i| (i.name, i.since))
        .chain(SCRIPT_OPTIONS.iter().map(|o| (o.key, o.since)))
        .chain(LIBRARIES_SINCE.iter().copied())
}

/// The `script_options` keys, as a sentence's list ("a, b and c").
pub fn script_option_list() -> String {
    let keys: Vec<&str> = SCRIPT_OPTIONS.iter().map(|o| o.key).collect();
    match keys.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => keys.join(""),
    }
}

/// A parameter list as the editor shows it: `x, y, [width]`.
fn param_list(params: &[Param]) -> String {
    params.iter().map(|p| if p.optional { format!("[{}]", p.name) } else { p.name.to_string() }).collect::<Vec<_>>().join(", ")
}

/// One line for the editor's hover and Functions list:
/// `text(x, y, text, color, [height]) -> width, height: what it does`.
pub fn detail(item: &Item) -> String {
    match &item.kind {
        Kind::Function { params, returns } => {
            let returns = if returns.is_empty() {
                String::new()
            } else {
                format!(" -> {}", returns.iter().map(|r| r.name).collect::<Vec<_>>().join(", "))
            };
            format!("{}({}){returns}: {}", item.name, param_list(params), item.summary)
        }
        Kind::Callback { params } => format!("{}({}): {}", item.name, param_list(params), item.summary),
        Kind::Constant { .. } => item.summary.to_string(),
    }
}

/// (name, [`detail`]) of every item but the deprecated ones, made once:
/// what the editor's completions, hover and signature help read.
pub fn details() -> &'static [(&'static str, &'static str)] {
    static DETAILS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    DETAILS.get_or_init(|| {
        ITEMS.iter().filter(|i| i.deprecated.is_none()).map(|i| (i.name, &*Box::leak(detail(i).into_boxed_str()))).collect()
    })
}

/// The field names of a class, for completing `t.` after a call.
pub fn field_names(class_name: &str) -> Option<&'static [&'static str]> {
    static NAMES: OnceLock<HashMap<&'static str, Vec<&'static str>>> = OnceLock::new();
    NAMES
        .get_or_init(|| CLASSES.iter().map(|c| (c.name, c.fields.iter().map(|f| f.name).collect())).collect())
        .get(class_name)
        .map(Vec::as_slice)
}

/// What a host function returns first, as a type (`Playback`, `Touch[]`,
/// `AlbumArt|nil`), for completing fields of its result.
pub fn first_return(function: &str) -> Option<&'static str> {
    match &item(function)?.kind {
        Kind::Function { returns, .. } => returns.first().map(|r| r.ty),
        _ => None,
    }
}

// --- the reference ----------------------------------------------------------

/// The generated parts of docs/scripting-reference.md, made and checked
/// by a test (`reference_lists_everything_and_is_current`).
#[cfg(test)]
mod reference {
    use super::*;


    /// How docs/scripting-reference.md marks a generated list of functions:
    /// `<!-- api: rect line -->`, the list, then [`END`].
    const MARK: &str = "<!-- api:";
    /// The marker for the generated list of tables.
    pub const TYPES_MARK: &str = "<!-- api-types -->";
    const END: &str = "<!-- /api -->";

    /// `since` as the reference says it: `"next"` stays `next` until
    /// cargo-release writes the version in (release.toml), here too.
    fn since_note(since: &str) -> String {
        format!("(since {since})")
    }

    /// Lines of a code block, each `left   -- comment`, the comments lined
    /// up; continuation comments (an empty left) under them.
    fn aligned(rows: &[(String, String)]) -> String {
        let column = rows.iter().map(|(left, _)| left.chars().count()).max().unwrap_or(0) + 3;
        let mut out = String::new();
        for (left, comment) in rows {
            let pad = column - left.chars().count();
            out.push_str(&format!("{left}{}-- {comment}\n", " ".repeat(pad)));
        }
        out
    }

    /// The generated code block for `names`, in that order.
    fn api_block(names: &[&str]) -> Result<String, String> {
        let mut rows = Vec::new();
        for name in names {
            let item = item(name).ok_or_else(|| format!("{name} isn't in lua_api::ITEMS"))?;
            let left = match &item.kind {
                Kind::Function { params, returns } if !returns.is_empty() => {
                    format!("{}({}) -> {}", item.name, param_list(params), returns.iter().map(|r| r.name).collect::<Vec<_>>().join(", "))
                }
                Kind::Function { params, .. } | Kind::Callback { params } => format!("{}({})", item.name, param_list(params)),
                Kind::Constant { .. } => item.name.to_string(),
            };
            rows.push((left, format!("{} {}", item.summary, since_note(item.since))));
            let params = match &item.kind {
                Kind::Function { params, .. } | Kind::Callback { params } => *params,
                Kind::Constant { .. } => &[],
            };
            let docs: Vec<String> = params.iter().filter(|p| !p.doc.is_empty()).map(|p| format!("{}: {}", p.name, p.doc)).collect();
            if !docs.is_empty() {
                rows.push((String::new(), docs.join("; ")));
            }
        }
        Ok(format!("```lua\n{}```\n", aligned(&rows)))
    }

    /// The generated code block of every table shape.
    fn types_block() -> String {
        let mut out = String::from("```lua\n");
        for (i, class) in CLASSES.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let mut rows = vec![(class.name.to_string(), class.summary.to_string())];
            for field in class.fields {
                let optional = if field.optional { "?" } else { "" };
                rows.push((format!("    {}{optional}: {}", field.name, field.ty), field.doc.to_string()));
            }
            // Fields without a doc don't get an empty comment.
            for line in aligned(&rows).lines() {
                out.push_str(line.strip_suffix("-- ").unwrap_or(line).trim_end());
                out.push('\n');
            }
        }
        out.push_str("```\n");
        out
    }

    /// docs/scripting-reference.md with its generated parts made again from
    /// the table: each marked list of functions, and the tables. Errors for a
    /// name that isn't an item, or a marker without its end.
    pub fn update_reference(markdown: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut lines = markdown.lines();
        while let Some(line) = lines.next() {
            out.push_str(line);
            out.push('\n');
            let trimmed = line.trim();
            let block = if let Some(names) = trimmed.strip_prefix(MARK).and_then(|rest| rest.strip_suffix("-->")) {
                api_block(&names.split_whitespace().collect::<Vec<_>>())?
            } else if trimmed == TYPES_MARK {
                types_block()
            } else {
                continue;
            };
            if !lines.any(|l| l.trim() == END) {
                return Err(format!("`{trimmed}` has no `{END}` after it"));
            }
            out.push_str(&block);
            out.push_str(END);
            out.push('\n');
        }
        Ok(out)
    }

    /// The names each `<!-- api: ... -->` marker lists, with the `## ` section
    /// it's in.
    pub fn reference_markers(markdown: &str) -> Vec<(String, &str)> {
        let mut section = String::new();
        let mut found = Vec::new();
        for line in markdown.lines() {
            if let Some(title) = line.strip_prefix("## ") {
                section = title.trim().to_string();
            } else if let Some(names) = line.trim().strip_prefix(MARK).and_then(|rest| rest.strip_suffix("-->")) {
                found.extend(names.split_whitespace().map(|name| (section.clone(), name)));
            }
        }
        found
    }
}

// --- LuaLS ------------------------------------------------------------------

/// The LuaLS definitions: a `---@meta` file declaring every class,
/// function and global above with its types and docs.
pub fn luals_definitions() -> String {
    let mut out = String::from(
        "---@meta synththing\n\
         -- The synththing script API, for the Lua language server (LuaLS): what\n\
         -- the app gives scripts, with types and docs. Written by synththing for\n\
         -- the version it is, and replaced when that changes; edits are lost.\n\n",
    );
    let doc_line = |out: &mut String, text: &str| {
        for line in text.lines() {
            out.push_str("--- ");
            out.push_str(line);
            out.push('\n');
        }
    };
    let since_text = |since: &str| if since == "next" { "the next release".to_string() } else { since.to_string() };
    for class in CLASSES {
        doc_line(&mut out, class.summary);
        out.push_str(&format!("---@class {}\n", class.name));
        for field in class.fields {
            let optional = if field.optional { "?" } else { "" };
            let doc = if field.doc.is_empty() { String::new() } else { format!(" {}", field.doc) };
            out.push_str(&format!("---@field {}{optional} {}{doc}\n", field.name, field.ty));
        }
        out.push('\n');
    }
    for item in ITEMS {
        doc_line(&mut out, &format!("{}\n\nSince {} ({}).", capitalized(item.summary), since_text(item.since), item.section));
        if let Some(instead) = item.deprecated {
            out.push_str(&format!("---@deprecated Use {instead}.\n"));
        }
        match &item.kind {
            Kind::Function { params, returns } => {
                for param in *params {
                    let optional = if param.optional { "?" } else { "" };
                    let doc = if param.doc.is_empty() { String::new() } else { format!(" {}", param.doc) };
                    out.push_str(&format!("---@param {}{optional} {}{doc}\n", param.name, param.ty));
                }
                for ret in *returns {
                    let doc = if ret.doc.is_empty() { String::new() } else { format!(" {}", ret.doc) };
                    out.push_str(&format!("---@return {} {}{doc}\n", ret.ty, ret.name));
                }
                let names: Vec<&str> = params.iter().map(|p| p.name).collect();
                out.push_str(&format!("function {}({}) end\n\n", item.name, names.join(", ")));
            }
            Kind::Callback { params } => {
                for param in *params {
                    let doc = if param.doc.is_empty() { String::new() } else { format!(" {}", param.doc) };
                    out.push_str(&format!("---@param {} {}{doc}\n", param.name, param.ty));
                }
                let names: Vec<&str> = params.iter().map(|p| p.name).collect();
                out.push_str(&format!("function {}({}) end\n\n", item.name, names.join(", ")));
            }
            Kind::Constant { ty } => {
                out.push_str(&format!("---@type {ty}\n{} = nil\n\n", item.name));
            }
        }
    }
    out
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Where the definitions go in the scripts folder: a hidden folder, so
/// they aren't listed as a script.
pub const LUALS_DIR: &str = ".luals";
pub const LUALS_FILE: &str = "synththing.lua";

/// A `.luarc.json` for the scripts folder: Lua 5.4 (what scripts run
/// on), and the definitions as a library.
pub fn luarc() -> String {
    format!(
        "{{\n  \"$schema\": \"https://raw.githubusercontent.com/LuaLS/vscode-lua/master/setting/schema.json\",\n  \"runtime.version\": \"Lua 5.4\",\n  \"workspace.library\": [\"{LUALS_DIR}\"]\n}}\n"
    )
}

/// Write the definitions into `dir` (the scripts folder), when they're
/// missing or from another version, and a `.luarc.json` pointing at them
/// unless there is one (the user's own settings stay).
pub fn write_luals(dir: &std::path::Path) -> std::io::Result<()> {
    let folder = dir.join(LUALS_DIR);
    let path = folder.join(LUALS_FILE);
    let definitions = luals_definitions();
    if std::fs::read_to_string(&path).ok().as_deref() != Some(definitions.as_str()) {
        std::fs::create_dir_all(&folder)?;
        std::fs::write(&path, definitions)?;
    }
    let luarc = dir.join(".luarc.json");
    if !luarc.exists() {
        std::fs::write(luarc, self::luarc())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Everything a script VM has (but Lua's own) is here, and everything
    /// here is in it.
    #[test]
    fn matches_what_scripts_get() {
        let registered: HashSet<String> = crate::lua_visualizer::host_global_names().into_iter().collect();
        // Lua's own: the ones the editor offers, and the rest of the base library.
        let lua: HashSet<&str> = crate::lua_analysis::LUA_GLOBALS
            .iter()
            .map(|(n, _)| *n)
            .chain(["_G", "_VERSION", "collectgarbage", "dofile", "load", "loadfile", "warn"])
            .collect();
        let ours: HashSet<&str> = ITEMS.iter().map(|i| i.name).collect();
        let mut missing: Vec<&String> =
            registered.iter().filter(|name| !ours.contains(name.as_str()) && !lua.contains(name.as_str())).collect();
        missing.sort();
        assert!(missing.is_empty(), "registered but not in lua_api::ITEMS (or Lua's LUA_GLOBALS): {missing:?}");
        for item in ITEMS.iter().filter(|i| !matches!(i.kind, Kind::Callback { .. })) {
            assert!(registered.contains(item.name), "{} is in lua_api::ITEMS but scripts don't have it", item.name);
        }
        assert_eq!(ours.len(), ITEMS.len(), "a name is in ITEMS twice");
    }

    #[test]
    fn versions_and_types_are_sound() {
        let classes: HashSet<&str> = CLASSES.iter().map(|c| c.name).collect();
        let builtin = ["number", "integer", "string", "boolean", "any", "nil", "table"];
        let known = |ty: &str| {
            // `table<K, V>`, `(A|B)[]`, `"literal"`: check the named parts.
            ty.split(|c: char| "|[]()<>, ".contains(c))
                .filter(|part| !part.is_empty() && !part.starts_with('"') && part != &"table")
                .all(|part| builtin.contains(&part) || classes.contains(part))
        };
        for (name, since) in all_since() {
            assert!(since == "next" || semver::Version::parse(since).is_ok(), "{name}: {since}");
        }
        for item in ITEMS {
            let types: Vec<&str> = match &item.kind {
                Kind::Function { params, returns } => params.iter().chain(*returns).map(|p| p.ty).collect(),
                Kind::Callback { params } => params.iter().map(|p| p.ty).collect(),
                Kind::Constant { ty } => vec![ty],
            };
            for ty in types {
                assert!(known(ty), "{}: unknown type {ty}", item.name);
            }
        }
        for class in CLASSES {
            for field in class.fields {
                assert!(known(field.ty), "{}.{}: unknown type {}", class.name, field.name, field.ty);
            }
        }
        // The ScriptOptions class lists exactly the keys.
        let keys: Vec<&str> = class("ScriptOptions").unwrap().fields.iter().map(|f| f.name).collect();
        assert_eq!(keys, SCRIPT_OPTIONS.iter().map(|o| o.key).collect::<Vec<_>>());
    }

    /// cargo-release's replacement (release.toml) turns each entry's
    /// `"next"` into the version, and nothing else.
    #[test]
    fn release_fills_in_next() {
        let config = include_str!("../release.toml");
        let line = config.lines().find(|l| l.contains("file = \"src/lua_api.rs\"")).expect("release.toml replaces in lua_api.rs");
        let search = line.split("search = \"").nth(1).and_then(|rest| rest.split("\", replace").next()).unwrap().replace("\\\"", "\"");
        let search = regex::Regex::new(&search).unwrap();
        let entries = "    f(\"a\", DRAWING, \"next\", &[], &[], \"\"),\n    ScriptOption { key: \"b\", since: \"next\" },\n";
        assert_eq!(search.find_iter(entries).count(), 2);
        let next_items = all_since().filter(|(_, since)| *since == "next").count();
        assert_eq!(search.find_iter(include_str!("lua_api.rs")).count(), next_items, "only entries match");
    }

    /// The reference's generated parts are current, and every function and
    /// global is listed once, in its own section. With
    /// SYNTHTHING_UPDATE_REFERENCE=1 set, this writes them instead.
    #[test]
    fn reference_lists_everything_and_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/scripting-reference.md");
        let markdown = std::fs::read_to_string(&path).unwrap();
        let updated = reference::update_reference(&markdown).unwrap();
        if std::env::var_os("SYNTHTHING_UPDATE_REFERENCE").is_some() {
            std::fs::write(&path, &updated).unwrap();
        } else {
            assert!(
                updated == markdown,
                "docs/scripting-reference.md is out of date with src/lua_api.rs: run `SYNTHTHING_UPDATE_REFERENCE=1 cargo test --lib reference_lists`"
            );
        }
        let markers = reference::reference_markers(&updated);
        for item in ITEMS.iter().filter(|i| i.deprecated.is_none()) {
            let places: Vec<&String> = markers.iter().filter(|(_, n)| *n == item.name).map(|(s, _)| s).collect();
            assert_eq!(places, [item.section], "{} should be listed once, under ## {}", item.name, item.section);
        }
        assert!(updated.contains(reference::TYPES_MARK), "the reference lists the tables");
    }

    #[test]
    fn details_and_definitions() {
        let text = details().iter().find(|(n, _)| *n == "text").unwrap().1;
        assert!(text.starts_with("text(x, y, text, color, [height]) -> width, height: "), "{text}");
        assert_eq!(first_return("playback"), Some("Playback"));
        assert!(field_names("Playback").unwrap().contains(&"title"));
        let definitions = luals_definitions();
        assert!(definitions.starts_with("---@meta"));
        assert!(definitions.contains("---@class Color\n"));
        assert!(definitions.contains("---@param height? number"));
        assert!(definitions.contains("function text(x, y, text, color, height) end"));
        assert!(definitions.contains("---@type number\nDT = nil"));
        assert_eq!(script_option_list(), "start_paused, app_log, record_prepare, record_auto, transparent and assets");
    }
}
