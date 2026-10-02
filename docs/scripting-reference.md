# synththing scripting reference

<!--
This file is the in-app Scripting Reference: build.rs parses it at build
time and embeds it (see src/lua_docs.rs). It understands a small subset of
Markdown, which is all this page needs:

- `## Heading` starts a section (the `#` title line above is skipped)
- blank-line-separated text is a paragraph (lines are joined)
- ``` fenced blocks are shown as code, verbatim
- lines starting with `- ` are a bullet list
- `inline code` is shown in a monospace font

HTML comments like this one are skipped. Anything else is shown as plain text.
-->

## Getting started

A visualizer script is a `.lua` file that defines one function, called once per frame:

```lua
function render(width, height, left, right)
    -- draw here
end
```

`width`/`height` are the pixel buffer's current size (it resizes with the visualizer, don't assume a fixed size). `left`/`right` are the new stereo samples played since the last frame, as flat 1-indexed tables of numbers, empty while paused, which is normal, not an error.

Anything a script keeps between frames goes in module-level locals (declared outside `render`), which last until the script is reloaded or restarted.

## Managing scripts

Scripts live in the `visualizers` folder of the app's config folder (`%APPDATA%\synththing\config\visualizers` on Windows); the path of the running one is shown above the visualizer. The row of controls above the visualizer (or above the editor, while the visualizer is closed):

- Script: pick which script runs.
- New: a new script (`untitled-N.lua`) from a template: Blank, or a copy of any bundled script, including the template-only ones like `settings_demo` and `input_demo`.
- Restore default: for a bundled script, puts the built-in version back (they're copied into the folder once, on first run, and yours to edit from then on).
- Restart (or F5, from anywhere): starts the running script over on a fresh Lua VM, all of its own state reset, its settings kept.
- Rename: renames the running script's file (its settings and saved-data files come along). A renamed bundled script no longer offers Restore default, since it's no longer under the bundled name.

Live reload: editing a script in the Script Editor saves it and recompiles it on a fresh Lua VM as you type. Editing the file in another editor works too: when the running script's file changes on disk, the app loads the new version into the editor and visualizer, and the script list updates as files are added, removed or renamed in the folder. A script that fails to compile leaves whatever was running before still running; the error shows above the editor and in this script's log (Script Settings tab) and stays until you fix it or revert. Restart restarts the version that's running, not the broken edit.

## The editor

- Syntax highlighting, with the app's own functions colored apart from plain Lua.
- Functions: a dropdown of every host function; picking one inserts its name at the text cursor.
- Hover a function name, or press F1 with the text cursor on one, to see its one-line description above the editor.
- Errors: the line a compile or runtime error points at is underlined in red, and "Go to line N" next to the error (above the editor, or above the visualizer, which also opens the editor) jumps the text cursor there.
- Opening the editor for the first time in a run puts this reference next to it as a tab.

## Drawing

Four functions, all color-table based:

```lua
clear({r,g,b})
line(x0, y0, x1, y1, {r,g,b,a})
rect(x0, y0, x1, y1, {r,g,b,a})
pixel(x, y, {r,g,b,a})
```

`r`, `g` and `b` are 0 to 255 (values outside that are clamped, fractions are fine). `a` is opacity, 0.0 to 1.0, defaulting to 1.0. An opaque draw (`a = 1`) overwrites; a translucent one alpha-blends over what's already there. `clear` always overwrites regardless of `a`. Coordinates are buffer pixels, (0, 0) at the top left, and can be fractional.

The buffer is cleared to all-black before your `render()` runs, there's no double buffering, so anything you want visible this frame has to be drawn this frame, including whatever scrolling history your own script is keeping track of.

## Audio data

```lua
fft_left(samples) -> spectrum
fft_right(samples) -> spectrum
```

Windowed magnitude spectrum of a channel's last 1024 samples (about 23 ms at 44.1 kHz, Hann window), computed in Rust so a script never has to do its own FFT (interpreted Lua doing a 1024-point FFT every frame would blow the frame budget by itself). Pass the `left`/`right` tables `render()` was given; the analyzer keeps its own internal sliding window, so calling it with an empty table (e.g. while paused) just returns the same spectrum as last time, not an error.

The result is a table of 512 magnitudes. Entry `i` (1-based) is the frequency `(i - 1) * SAMPLE_RATE / 1024` Hz, so entries are about 43 Hz apart at 44.1 kHz, from 0 Hz up to half the sample rate. A full-volume sine wave reads about 1.0 at its frequency. The table is empty until the first 1024 samples have played.

```lua
local spectrum = fft_left(left)
local function bin_for(hz) return math.floor(hz * 1024 / SAMPLE_RATE) + 1 end
local a440 = spectrum[bin_for(440)] or 0
```

## MIDI data

```lua
notes_between(t0, t1) -> notes  -- whole notes sounding in [t0, t1): {id, channel, key, velocity, start, stop}
active_notes() -> notes         -- held right now: {channel, key, velocity}
upcoming_notes() -> notes       -- changing within NOTE_LOOKAHEAD: adds `on`, `seconds_until`
midi_channels() -> channels     -- channels this file uses, sorted
channel_enabled(c) -> bool      -- the GUI's per-channel toggle
set_channel_enabled(c, bool)    -- a script can mute/unmute a channel too
```

`notes_between` is the one to reach for when you want notes as whole things: each comes with its `start` and `stop` time (song seconds, the same clock as `playback().position`), so there's no pairing note-ons with note-offs yourself. It returns every note sounding at any point in the window, including ones that started before `t0` and are still held, in start order. The window can be any size, so a script can look further ahead than `NOTE_LOOKAHEAD` or read the whole song once (`notes_between(0, playback().length)`) to build a level up front. `id` is stable for as long as the song is loaded, so it works as a table key for tracking which notes you've already handled. Notes are read from the file itself, so they're all there whether or not a soundfont is loaded, and regardless of which channels are muted.

```lua
-- Notes starting in the next two seconds, each handled once.
local seen = {}
function render(width, height, left, right)
    local now = playback().position
    for _, note in ipairs(notes_between(now, now + 2)) do
        if note.start >= now and not seen[note.id] then
            seen[note.id] = true
            -- spawn something for note.key, due at note.start
        end
    end
end
```

Channels are 0 to 15 (the GUI shows them as 1 to 16), keys are MIDI note numbers 0 to 127 (60 is middle C), velocities 1 to 127. `upcoming_notes()` reports each upcoming on/off event every frame until it plays; `seconds_until` is in song seconds, like everything else here.

All empty/true for a plain audio file, there's no score to read, so nothing here errors, it just has nothing to report.

`set_channel_enabled` goes through the exact same command the GUI's own checkboxes send, so a script can't disable the last remaining enabled channel either, useful for things like a game script muting a dead player's channel without needing its own "don't silence everything" logic.

## Playback & timing

```lua
playback() -> {position, length, speed, paused, finished, loop_enabled, generation}
set_paused(paused)  -- pause or resume playback
seek(seconds)       -- jump to a position in the song
DT                  -- seconds since the previous render() call (0.0 on the first, at most 0.25)
TIME                -- seconds since this script started, wall clock, never capped
FRAME               -- frames rendered since this script started (1 on the first)
SAMPLE_RATE         -- the engine's sample rate, in Hz
NOTE_LOOKAHEAD      -- song seconds upcoming_notes() looks ahead (4.0)
```

Units: `position` and `length` are seconds of song time. `speed` is the playback rate (1.0 is normal), and it's song time that runs faster or slower: at 2.0, one song-second passes in half a real second. Everything song-related (`position`, note `start`/`stop`, `seconds_until`, `NOTE_LOOKAHEAD`) is in song seconds, while `DT` and `TIME` are real seconds. To turn a song-time gap into real time, divide by `speed`.

`generation` goes up by one every time the position jumps instead of running on: a seek (by the user or a script), a loop back to the start, or a new song. Compare it with the value from the previous frame to know exactly when to reset anything that tracks position, with no guessing from how far `position` moved.

`set_paused` and `seek` are carried out by the app right after the frame, and show up in `playback()` from the next frame on (along with a new `generation` for a seek). Unpausing a song that has finished starts it over. Like `set_channel_enabled`, they're meant for things like pausing a game when the player dies, or a "retry this section" practice loop.

`TIME` and `FRAME` start over when the script is reloaded or restarted. `TIME` keeps counting while the visualizer isn't being drawn, unlike the sum of `DT`.

Check `playback().paused` before writing into a scrolling history buffer, otherwise the picture keeps scrolling through a frozen spectrum while paused instead of actually freezing. See `spectrogram.lua` for the pattern.

`DT` is for frame-rate-independent animation (e.g. a smooth sweep or a moving player), not for gating logic, a visualizer should look right regardless of how fast frames are actually arriving.

`render()` only runs while the visualizer is on screen: not while its tab is closed or hidden behind another tab, and not while Preferences is open (the visualizer freezes on its last frame, and playback pauses). `DT` is capped at 0.25 so the first frame back after a gap like that doesn't make animations jump.

## Input

```lua
mouse() -> x, y               -- buffer pixels; nil, nil when not over the visualizer
mouse_delta() -> dx, dy       -- movement since last frame, buffer pixels
scroll() -> dx, dy            -- scrolling since last frame (positive y is up)
mouse_down([button])          -- held; "left" (default), "right", "middle"
mouse_pressed([button])       -- went down this frame
mouse_released([button])      -- went up this frame
key_down(name)                -- held; "a", "space", "up", "enter", "f6", ...
key_pressed(name)             -- went down this frame (no key repeat)
key_released(name)            -- went up this frame
has_focus() -> bool           -- whether keys come to this script
display_mode() -> string      -- "window", "fullscreen" or "dedicated"
```

Mouse state is reported while the pointer is over the visualizer (and while it has focus, so a drag that wanders off still ends cleanly). Keys only while the visualizer has focus: click into it to give it focus, click anywhere else to take it back. While it has focus the app's own shortcuts (space to pause, arrows to seek, ...) are off, so those keys are the script's. Key names are case-insensitive; an unknown key or button name is a script error that says so. `scroll()` arrives smoothed, a little per frame, so add it up rather than treating each frame as one notch.

Three keys always belong to the app, and are never reported to a script in any mode: Escape means "get me out" (releases focus, and goes from either fullscreen straight back to windowed), F11 toggles the dedicated fullscreen, and F5 restarts the running script.

`display_mode()` tells a script where it's being shown: `"window"` (a tab in the normal window), `"fullscreen"` (a tab, with the whole app fullscreen), or `"dedicated"` (the Fullscreen visualizer button or F11: nothing but the visualizer, filling the screen, with focus).

```lua
set_cursor_visible(visible)   -- false: hide the system cursor over the visualizer
set_cursor_locked(locked)     -- true: keep the cursor on screen (dedicated only)
```

Both stick until changed (or a different script loads). A hidden cursor only hides while it's over the visualizer, so draw your own there. Locking only takes effect in the dedicated fullscreen while it has focus, and lets go whenever that stops (Escape, switching windows), so a script can't trap the cursor. It keeps the cursor inside the screen rather than pinning it in place, so use `mouse_delta()` for motion. See `input_demo.lua` (New > input_demo).

## Logging

```lua
log(message)
```

Appends to this script's log, in the Script Settings tab (View > Script Settings), along with the script's own errors. Identical consecutive messages collapse into one entry with a count instead of flooding the pane, safe to call every single frame. (Help > Log... is the app's own log, not the script's.)

## Debugging

```lua
debug_locals([label])
```

Snapshots whatever local variables (and function parameters, Lua treats those the same way) are in scope at the exact point you call it, shown as a nested, expandable tree in the Script Settings tab, under Variables. Call it from wherever in your script you actually want visibility, inside a loop, after a specific branch, wherever, not just at the top level.

Only one snapshot is kept; calling it again (from the same or a different spot) replaces the previous one. `label` is optional and just shows up next to the snapshot so you can tell which call site it came from.

This only sees true locals and parameters, a module-level value declared with `local` outside any function (the usual way the bundled scripts keep their state, e.g. `waveform.lua`'s `history`) is an upvalue of `render`, not a local inside it, so it won't show up unless you also have a local alias or parameter in scope at the call site.

## Settings

A script can expose typed, user-editable values that show up as widgets in the Script Settings tab. Call the matching function every frame, the first call each compile registers the descriptor (default/range/options); every call after that just returns the live value, so dragging a slider updates the running script immediately, with no recompile and without disturbing any history the script is keeping.

```lua
setting_bool(key, default) -> bool
setting_int(key, default, min, max) -> integer
setting_float(key, default, min, max) -> number
setting_color(key, default) -> {r,g,b}
setting_string(key, default) -> string
setting_selection(key, options, defaults, max_selections) -> selected
```

A selection picks up to `max_selections` of a fixed option list; a pick past the cap evicts whichever option was selected longest ago, so there's no disabled-checkbox state to design around.

Values persist to `<script>.lua.settings.json` next to the script and are re-applied on the next load (and on Restart), falling back to the script's own default for anything the sidecar doesn't have.

```lua
local accent = setting_color("accent", { r = 51, g = 204, b = 255 })
local bins = setting_int("bins", 36, 8, 96)
```

## Saved data

```lua
store_get(key) -> value   -- nil if nothing's saved under key
store_set(key, value)     -- save (nil deletes)
```

The script's own saved state, for things like high scores and unlocks: values survive restarts, reloads and closing the app. Unlike settings, nothing here shows up for the user to edit; it belongs to the script. Values can be booleans, numbers, strings, or tables of those (nested up to 16 deep; a table with keys 1..n comes back as a list, anything else with string keys). Functions and the like can't be saved, and `store_set` says so.

It's kept in `<script>.lua.store.json` next to the script, written within about a second of a change and whenever the script is replaced or the app closes, so calling `store_set` every frame is fine. A script that hasn't been saved to a file keeps its data in memory only.

```lua
local best = store_get("best") or 0
if score > best then store_set("best", score) end
```

## Performance

A script runs on every rendered frame while it's on screen, so the usual budget is the 60fps frame, ~16.7ms, but each draw call's cost scales with how many pixels it actually touches, not just how many calls you make, so a heatmap-style script needs more care than a line plot.

- The buffer is capped at ~1280x720 worth of pixels and scaled up (nearest-neighbor) to fill larger views, so either fullscreen on a large monitor doesn't multiply your render cost, but it's still worth keeping draw counts sane.
- `spectrogram.lua` run-length-merges same-colored cells in a column into one rect instead of one draw per cell, worth copying for any other grid/heatmap-shaped script.
- Keep history buffers fixed-size ring buffers (see `waveform.lua`), not growing/shrinking arrays, removing from the front of a Lua table is O(n), and doing that every frame adds up.

## Bundled scripts, as reference

The first six are copied into your scripts folder on first run; the last two are templates only, made with New when you want them.

- `waveform.lua`, a scrolling oscilloscope trace; the simplest ring-buffer example
- `fft.lua`, log-spaced spectrum bars, left/right overlap shown as a third color
- `keyboard.lua`, an 88-key piano with falling notes; two-pass enabled/disabled channel rendering
- `spectrogram.lua`, a scrolling time/frequency heatmap; run-length merging, pause-awareness, live-tunable resolution
- `letters.lua`, one glyph per channel, colored by pitch; a from-scratch bitmap font
- `snake.lua`, one snake per channel hunting apples spawned by note-ons; the most elaborate example, worth reading end to end
- `settings_demo.lua`, exercises every setting type; not a music visualizer, a reference for the settings API itself
- `input_demo.lua`, mouse, keyboard and cursor control: a paint toy with its own cursor; not a music visualizer either
