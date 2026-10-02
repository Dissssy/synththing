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

Live reload: editing a script in the Script Editor saves it and recompiles it on a fresh Lua VM as you type. A script that fails to compile leaves whatever was running before still running; the error shows above the editor and in this script's log (Script Settings tab) and stays until you fix it or revert. Restart restarts the version that's running, not the broken edit.

## The editor

- Syntax highlighting, with the app's own functions colored apart from plain Lua.
- Functions: a dropdown of every host function; picking one inserts its name at the text cursor.
- Hover a function name, or press F1 with the text cursor on one, to see its one-line description above the editor.
- Opening the editor for the first time in a run puts this reference next to it as a tab.

## Drawing

Four functions, all color-table based:

```lua
clear({r,g,b})
line(x0, y0, x1, y1, {r,g,b,a})
rect(x0, y0, x1, y1, {r,g,b,a})
pixel(x, y, {r,g,b,a})
```

`a` is opacity, 0.0..1.0, defaults to 1.0. An opaque draw (`a = 1`) overwrites; a translucent one alpha-blends over what's already there. `clear` always overwrites regardless of `a`. Coordinates are buffer pixels and can be fractional.

The buffer is cleared to all-black before your `render()` runs, there's no double buffering, so anything you want visible this frame has to be drawn this frame, including whatever scrolling history your own script is keeping track of.

## Audio data

```lua
fft_left(samples) -> spectrum
fft_right(samples) -> spectrum
```

Windowed magnitude spectrum of a channel's last ~1024 samples, computed in Rust so a script never has to do its own FFT (interpreted Lua doing a 1024-point FFT every frame would blow the frame budget by itself). Pass the `left`/`right` tables `render()` was given; the analyzer keeps its own internal sliding window, so calling it with an empty table (e.g. while paused) just returns the same spectrum as last time, not an error.

## MIDI data

```lua
active_notes() -> notes       -- held right now: {channel, key, velocity}
upcoming_notes() -> notes     -- changing within NOTE_LOOKAHEAD: adds `on`, `seconds_until`
midi_channels() -> channels   -- channels this file uses, sorted
channel_enabled(c) -> bool    -- the GUI's per-channel toggle
set_channel_enabled(c, bool)  -- a script can mute/unmute a channel too
```

All empty/true for a plain audio file, there's no score to read, so nothing here errors, it just has nothing to report.

`set_channel_enabled` goes through the exact same command the GUI's own checkboxes send, so a script can't disable the last remaining enabled channel either, useful for things like a game script muting a dead player's channel without needing its own "don't silence everything" logic.

## Playback & timing

```lua
playback() -> {position, length, speed, paused, finished, loop_enabled}
DT              -- seconds since the previous render() call (0.0 on the first, at most 0.25)
SAMPLE_RATE     -- the engine's sample rate, in Hz
NOTE_LOOKAHEAD  -- seconds upcoming_notes() looks ahead
```

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
