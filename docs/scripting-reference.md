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
- New: a new script (`untitled-N.lua`) from a template: Blank, or a copy of any bundled script, including the template-only ones like `settings_demo`, `input_demo` and `terminal`.
- Restore default: for a bundled script, puts the built-in version back (they're copied into the folder once, on first run, and yours to edit from then on).
- Restart (or F5, from anywhere): starts the running script over on a fresh Lua VM, all of its own state reset, its settings kept.
- Rename: renames the running script's file (its settings and saved-data files come along). A renamed bundled script no longer offers Restore default, since it's no longer under the bundled name.

Live reload: editing a script in the Script Editor saves it and recompiles it on a fresh Lua VM as you type. Editing the file in another editor works too: when the running script's file changes on disk, the app loads the new version into the editor and visualizer, and the script list updates as files are added, removed or renamed in the folder. A script that fails to compile leaves whatever was running before still running; the error shows above the editor and in this script's log (Script Settings tab) and stays until you fix it or revert. Restart restarts the version that's running, not the broken edit.

## The editor

- Syntax highlighting, with the app's own functions colored apart from plain Lua, and line numbers.
- When edits apply (the "Apply:" choice above the text): every edit, once typing pauses for a moment (the default), or only on Ctrl+S. Applying saves the file and restarts the script from scratch, so a game loses its state; the later two keep that from happening on every keystroke. Ctrl+S always applies right away, and "Not applied yet" shows while edits are waiting. Waiting edits are applied before switching scripts, renaming one or closing the app.
- Typing: Enter keeps the line's indentation, one level deeper after `then`, `do`, `function(...)`, `repeat`, `else` or an opening bracket; `end`, `else`, `elseif`, `until` and closing brackets line themselves up with their block as you type them. Tab and Shift+Tab indent and outdent (every line of a selection); Ctrl+/ comments lines out with `--` or back in. Brackets and quotes close themselves (or wrap the selection), typing a closer that's already there steps over it, and Backspace in an empty pair removes both. The bracket matching the one at the text cursor is highlighted.
- Find and replace: Ctrl+F (Ctrl+H for replace) opens a bar above the text, starting from the selected text. Every match is highlighted; Enter and Shift+Enter go to the next and previous one, Aa matches case, Esc closes it.
- Problems: the editor reads the script as you type (even half-written) and underlines problems: syntax errors in red, and in yellow names that aren't defined anywhere (with a guess when one is close: "Did you mean `playback`?"), locals that are never used, and globals set inside a function that were probably meant to be `local`. Lines with a problem get a dot by their number, hovering an underline shows its message, and the "N problems" list above the text jumps to each.
- Completions: a list pops up as you type a name or after `.` or `:` (Ctrl+Space opens it any time): the locals in scope at the text cursor, the script's own functions and globals, the app's functions, Lua's built-ins (`math.`, `string.`, `table.` members included) and keywords. It also knows the fields of the tables the app hands back, followed through variables (`local p = playback()`, then `p.`; `for _, n in ipairs(active_notes())`, then `n.`), and for the script's own tables offers the fields it uses on them elsewhere. Up and Down choose, Enter or Tab use one, Esc closes it.
- Signature help: inside a call, its parameters show above the line with the one being typed underlined, for the app's functions, Lua's and the script's own.
- Go to: a list of the script's functions to jump between; F12 or Ctrl+click on a name jumps to where it's defined. Hovering one of the script's own names says what it is (a local and its line, a parameter, a function and its parameters).
- Format (Shift+Alt+F): lays the script out tidily (StyLua, 4-space indents, lines up to 120 wide). It needs the script to parse first.
- Functions: a dropdown of every host function; picking one inserts its name at the text cursor.
- Hover a function name, or press F1 with the text cursor on one, to see its one-line description above the editor.
- Errors: the line a compile or runtime error points at is underlined in red, its line number is red, its message is shown at the end of the line, and "Go to line N" next to the error (above the editor, or above the visualizer, which also opens the editor) jumps the text cursor there.
- History...: synththing keeps a copy of the script when it's opened and about once a minute while you edit (the last 50, in its config folder). The History window shows each with how long ago and how many lines differ from now, previews it, and restores it (keeping the current version in the list).
- Text size: Ctrl+scroll over the text, or the size box; Wrap turns line wrapping off for long lines (scroll sideways instead).
- Opening the editor for the first time in a run puts this reference next to it as a tab.

## Drawing

All color-table based:

```lua
clear({r,g,b})
line(x0, y0, x1, y1, {r,g,b,a})
rect(x0, y0, x1, y1, {r,g,b,a})
pixel(x, y, {r,g,b,a})
circle(x, y, radius, {r,g,b,a})                -- filled
triangle(x1, y1, x2, y2, x3, y3, {r,g,b,a})    -- filled
polygon(points, {r,g,b,a})                     -- filled; points {x1, y1, x2, y2, ...} or {{x, y}, ...}
```

Shapes fill every pixel whose center is inside them, so edges are crisp (no anti-aliasing) and a translucent shape blends each pixel exactly once. `polygon` takes up to 4096 points, as a flat list of coordinates or a list of points (`{x, y}` or `{x = .., y = ..}`); a polygon that crosses itself leaves holes where it overlaps (the even-odd rule). For outlines, use `line`.

`r`, `g` and `b` are 0 to 255 (values outside that are clamped, fractions are fine). `a` is opacity, 0.0 to 1.0, defaulting to 1.0. An opaque draw (`a = 1`) overwrites; a translucent one alpha-blends over what's already there. `clear` always overwrites regardless of `a`. Coordinates are buffer pixels, (0, 0) at the top left, and can be fractional.

The buffer is cleared to all-black before your `render()` runs, there's no double buffering, so anything you want visible this frame has to be drawn this frame, including whatever scrolling history your own script is keeping track of.

## Text

```lua
text(x, y, string, {r,g,b,a}, [height]) -> width, height   -- draw, and get the size drawn
text_size(string, [height]) -> width, height               -- measure only, draws nothing
FONT_HEIGHT                                                 -- 12, the font's own line height
```

Text uses the built-in Monogram pixel font, scaled with pure nearest-neighbor so its pixels stay crisp, square blocks. `height` is the height of one line in pixels (default `FONT_HEIGHT`), and the text fills the box from `y` to `y + height` per line: the line box includes room for accents above capitals and descenders below, with capitals taking rows 3 to 10 of the font's 12. Whole multiples of `FONT_HEIGHT` (12, 24, 36, ...) keep every font pixel exactly square; any other height works but some pixels come out a pixel wider or taller than others.

Every character is the same width (monospace): 6 font pixels per character, the last one blank. The returned width ends at the last drawn column of the widest line, and the returned height is lines times `height`. `\n` starts a new line. Characters the font doesn't have draw as `?`; it covers ASCII, accented Latin letters, Cyrillic and a few symbols and arrows.

`text_size` gives the same numbers as `text` without drawing anything, for laying things out before drawing, like a table whose columns depend on its widest entry:

```lua
local rows = { { "Player", "Score" }, { "you", "1200" } }
local h = 24
local col = 0
for _, row in ipairs(rows) do
    local w = text_size(row[1], h) -- just the width; the height is the second value
    col = math.max(col, w)
end
for i, row in ipairs(rows) do
    local y = 8 + (i - 1) * h
    text(8, y, row[1], { r = 230, g = 230, b = 240 }, h)
    text(8 + col + 16, y, row[2], { r = 255, g = 200, b = 60 }, h)
end
```

Text is drawn as rects, so it alpha-blends and clips at the edges exactly like `rect`.

## Sprites

```lua
sprite_register({ image = rows, palette = colors }) -> id   -- once, outside render()
sprite(id, x, y, [scale or options]) -> width, height      -- draw it; the size drawn
sprite_size(id) -> width, height                            -- its size in sprite pixels
```

A sprite is a little pixel image the script hands over once; the app turns it into ready-to-draw colors right then, so drawing it later, at any size, is cheap. `image` is a list of rows, each a list of palette indices; `palette` is a list of color tables. Index 1 is the palette's first color (Lua counting), and 0 is transparent. Rows can be different lengths (a short row is transparent past its end); the sprite is as wide as its longest row. Palette colors can be translucent (`a` below 1), and those pixels blend.

```lua
local heart = sprite_register({
    palette = { { r = 230, g = 40, b = 70 }, { r = 255, g = 160, b = 180 } },
    image = {
        { 0, 1, 1, 0, 1, 1, 0 },
        { 1, 2, 1, 1, 1, 1, 1 },
        { 1, 1, 1, 1, 1, 1, 1 },
        { 0, 1, 1, 1, 1, 1, 0 },
        { 0, 0, 1, 1, 1, 0, 0 },
        { 0, 0, 0, 1, 0, 0, 0 },
    },
})

function render(width, height, left, right)
    sprite(heart, 10, 10, 4)                             -- 4x size
    sprite(heart, 60, 10, { scale = 4, flip_x = true })  -- mirrored
end
```

`x, y` is the sprite's top-left corner, rounded to whole pixels. The fourth argument is a scale (1 by default), or an options table with `scale`, `flip_x`, `flip_y`, `src`, `palette` and `tint`. Scaling is nearest-neighbor, so pixels stay crisp; whole-number scales keep every sprite pixel exactly square. `sprite` returns the size it drew, in buffer pixels.

`src = {x, y, w, h}` (or `{x = .., y = .., w = .., h = ..}`) draws only that part of the sprite, in sprite pixels with (0, 0) at its top-left, clipped to the sprite: a sprite sheet. Register all of a character's animation frames as one image and pick the frame when drawing; scale and flips apply to the part, and the returned size is the part's.

```lua
-- Four 8x8 frames side by side in one 32x8 image.
local frame = math.floor(TIME * 8) % 4
sprite(walker, x, y, { scale = 3, src = { frame * 8, 0, 8, 8 } })
```

`palette` and `tint` recolor the sprite for just that one draw, so one sprite can be drawn in several colors in the same frame (a player per channel, a flash when hit). `palette = { [index] = color, ... }` swaps the palette entries you name for other colors and leaves the rest alone; `tint = color` multiplies every color by it, channel by channel, so white becomes the tint, black stays black, and the tint's `a` fades the whole sprite. With both, the swaps happen first and the tint applies on top. A recolored draw costs about the same as a plain one.

```lua
-- One player sprite: palette entry 1 is its body, 2 its outline.
local color = setting_color("player_color", { r = 90, g = 200, b = 255 })
sprite(player, x, y, { scale = 3, palette = { [1] = color } })
sprite(player, x2, y2, { scale = 3, tint = { r = 255, g = 255, b = 255, a = 0.4 } }) -- a ghost
```

Register sprites once, at the top level of the script (outside `render()`), and keep the ids: each call registers a new sprite, and a script can have at most 10,000. Sprites belong to the script and go away when it's reloaded or restarted, which re-registers them anyway.

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

```lua
level_left() -> number     -- loudness of this frame's left samples (RMS)
level_right() -> number    -- same for the right
onset() -> hit, strength   -- did a sound just start this frame?
```

The levels are the root mean square of the samples `render()` got this frame: 0 for silence, about 0.71 for a full-volume sine wave, 0 while paused. They change quickly from frame to frame, so smooth them if you want a steady meter (`shown = shown + (level_left() - shown) * 0.2`).

`onset()` is true on a frame where the sound suddenly gets stronger (a drum hit, a note attack), worked out in Rust from how much the spectrum gained since the previous frame against the average of the last half second. After an onset, the next 80 ms can't be another, so one hit counts once. `strength` is that frame's gain in the spectrum: bigger means a sharper change, useful for comparing hits within a song rather than as an absolute number. It's meant for visuals reacting to the audio, not as a beat tracker: for MIDI, `notes_between` and `beat()` are exact. The detector starts running the first time a script calls `onset()`, so that first call (and the next few frames, while it builds up a history) returns false.

## MIDI data

```lua
notes_between(t0, t1) -> notes  -- whole notes sounding in [t0, t1): {id, channel, key, velocity, start, stop}
active_notes() -> notes         -- held right now: {channel, key, velocity, source}
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
playback() -> {position, length, speed, paused, finished, loop_enabled, generation, song_name, song_path, song_id}
set_paused(paused)  -- pause or resume playback
seek(seconds)       -- jump to a position in the song
DT                  -- seconds since the previous render() call (0.0 on the first, at most 0.25)
TIME                -- seconds since this script started (the sum of every frame's real length, never capped)
FRAME               -- frames rendered since this script started (1 on the first)
SAMPLE_RATE         -- the engine's sample rate, in Hz
NOTE_LOOKAHEAD      -- song seconds upcoming_notes() looks ahead (4.0)
```

Units: `position` and `length` are seconds of song time. `speed` is the playback rate (1.0 is normal), and it's song time that runs faster or slower: at 2.0, one song-second passes in half a real second. Everything song-related (`position`, note `start`/`stop`, `seconds_until`, `NOTE_LOOKAHEAD`) is in song seconds, while `DT` and `TIME` are real seconds. To turn a song-time gap into real time, divide by `speed`.

`song_name` is the song's name as the app shows it, `song_path` the file's full path, and `song_id` a short code made from the file's contents (16 hex digits): the same song gives the same id wherever it's kept and whatever it's called, and a different file gives a different one. All three are nil with no song loaded. `song_id` is the one to key saved data by, like a best score per song:

```lua
local p = playback()
local key = "best:" .. (p.song_id or "none")
if score > (store_get(key) or 0) then store_set(key, score) end
```

Everything a script sees follows what's being heard, not what's been prepared: the app renders audio a little ahead (the audio buffer in Preferences), and `position`, the notes (`active_notes`, `upcoming_notes`), `paused`/`finished` and the `left`/`right` samples are all held back to match the speakers, so visuals line up with the sound at any buffer size. A seek, pause or new song shows up right away.

`generation` goes up by one every time the position jumps instead of running on: a seek (by the user or a script), a loop back to the start, or a new song. Compare it with the value from the previous frame to know exactly when to reset anything that tracks position, with no guessing from how far `position` moved.

`set_paused` and `seek` are carried out by the app right after the frame, and show up in `playback()` from the next frame on (along with a new `generation` for a seek). Unpausing a song that has finished starts it over. Like `set_channel_enabled`, they're meant for things like pausing a game when the player dies, or a "retry this section" practice loop.

`TIME` and `FRAME` start over when the script is reloaded or restarted. `TIME` keeps counting while the visualizer isn't being drawn, unlike the sum of `DT`.

Check `playback().paused` before writing into a scrolling history buffer, otherwise the picture keeps scrolling through a frozen spectrum while paused instead of actually freezing. See `spectrogram.lua` for the pattern.

`DT` is for frame-rate-independent animation (e.g. a smooth sweep or a moving player), not for gating logic, a visualizer should look right regardless of how fast frames are actually arriving.

`render()` only runs while the visualizer is on screen: not while its tab is closed or hidden behind another tab, and not while Preferences is open (the visualizer freezes on its last frame, and playback pauses). `DT` is capped at 0.25 so the first frame back after a gap like that doesn't make animations jump.

## Musical timing

```lua
beat([seconds]) -> beats                  -- song position in beats (quarter notes), from 0
time_at_beat(beat) -> seconds             -- when a beat falls, in song seconds
bar([seconds]) -> bar, beat_in_bar        -- bar number (from 1) and beats into it (from 0)
tempo([seconds]) -> bpm                   -- quarter notes per minute
time_signature([seconds]) -> num, den     -- e.g. 3, 4 for 3/4
```

These read the MIDI file's own tempo changes and time signatures, so they're exact, and follow every change in the song. Each takes an optional song time in seconds and defaults to the current position. Beats are quarter notes, the MIDI convention, whatever the time signature: in 6/8, a bar is 3 beats long. A song with no tempo marking is 120 bpm in 4/4, as MIDI defines it.

For a plain audio file there's no score to read, so these all return nil (as do MIDI files timed in SMPTE frames, which have no beats); check before doing math with them.

```lua
-- Pulse on every beat, stronger on the first beat of each bar.
local b = beat()
if b then
    local _, into = bar()
    local pulse = 1 - (b % 1)              -- 1 right on the beat, fading to 0
    local size = (into < 1) and 40 or 24
    circle(width / 2, height / 2, size * pulse, { r = 255, g = 200, b = 60 })
end
```

For rhythm games, `time_at_beat` turns a beat into the song time to compare against `playback().position` (both song seconds), and `notes_between` gives each note's exact time.

## Input

```lua
mouse() -> x, y               -- buffer pixels; nil, nil when not over the visualizer
mouse_delta() -> dx, dy       -- movement since last frame, buffer pixels
scroll() -> dx, dy            -- scrolling since last frame (positive y is up)
mouse_down([button])          -- held; "left" (default), "right", "middle"
mouse_pressed([button])       -- went down this frame
mouse_released([button])      -- went up this frame
has_focus() -> bool           -- whether keys come to this script
display_mode() -> string      -- "window", "fullscreen" or "dedicated"
```

Mouse state is reported while the pointer is over the visualizer (and while it has focus, so a drag that wanders off still ends cleanly). Keys only while the visualizer has focus: click into it to give it focus, click anywhere else to take it back. While it has focus the app's own shortcuts (space to pause, arrows to seek, ...) are off, so those keys are the script's. An unknown button name is a script error that says so. `scroll()` arrives smoothed, a little per frame, so add it up rather than treating each frame as one notch.

For the keyboard, see Controls (rebindable actions) and Typing (text entry) below.

Five keys always belong to the app, and are never reported to a script in any mode: Escape means "get me out" (releases focus, and goes from either fullscreen straight back to windowed), F11 toggles the dedicated fullscreen, F5 restarts the running script, F9 starts and stops recording, and F10 pauses recording.

While the visualizer is being recorded (the Record button above it), `render()` gets the recording's size as `width` and `height` (1920 x 1080, say) whatever the panel's size, and the picture is shown letterboxed in the panel, with `mouse()` still in the picture's own pixels. It's also called once per video frame rather than once per screen refresh, with `DT` the video time that passed (1/60 s at 60 fps, more if it had to catch up), so motion in the video is perfectly even. A script that lays itself out from `width` and `height` and moves things by `DT` needs nothing special; a bigger recording size just costs more time per frame, and a script too slow for it makes the video repeat frames.

`display_mode()` tells a script where it's being shown: `"window"` (a tab in the normal window), `"fullscreen"` (a tab, with the whole app fullscreen), or `"dedicated"` (the Fullscreen visualizer button or F11: nothing but the visualizer, filling the screen, with focus).

```lua
set_cursor_visible(visible)   -- false: hide the system cursor over the visualizer
set_cursor_locked(locked)     -- true: keep the cursor on screen (dedicated only)
```

Both stick until changed (or a different script loads). A hidden cursor only hides while it's over the visualizer, so draw your own there. Locking only takes effect in the dedicated fullscreen while it has focus, and lets go whenever that stops (Escape, switching windows), so a script can't trap the cursor. It keeps the cursor inside the screen rather than pinning it in place, so use `mouse_delta()` for motion. See `input_demo.lua` (New > input_demo).

## Controls

```lua
input_register(name, default_keys) -> id   -- once, outside render()
input(id) -> state       -- "pressed", "held", "released" or "up"
input_down(id) -> bool   -- held: "pressed" or "held"
```

Keyboard input comes through actions: a script names each thing it can do ("jump", "left", "clear") and gives it default keys, and the user can rebind them under Controls in the Script Settings tab (click a key, press the new one; x removes a key, + adds another, Reset goes back to the script's keys). Bindings are saved per script, in `<script>.lua.controls.json` next to it.

`default_keys` is a key name or a list of them: `"space"`, `{ "left", "a" }`. Key names are case-insensitive: letters (`"a"`), digits (`"1"`), `"space"`, `"enter"`, `"tab"`, `"backspace"`, arrows (`"up"`, `"down"`, `"left"`, `"right"`), `"f1"` to `"f20"` and so on; an unknown name is a script error that says so, as is one of the app's reserved keys (below). An empty list makes an action with no keys until the user binds some.

`input(id)` says what the action did this frame: `"pressed"` the frame one of its keys went down (key repeat doesn't count), `"held"` while it stays down after that, `"released"` the frame it went up, and `"up"` otherwise. `input_down(id)` is the "is it down" shortcut, for movement. Registering the same name twice gives the same id. Register actions once, at the top level of the script, like sprites; the Controls list shows them in the order they were registered.

```lua
local JUMP = input_register("jump", "space")
local LEFT = input_register("left", { "left", "a" })

function render(width, height, left, right)
    if input(JUMP) == "pressed" then vy = -300 end
    if input_down(LEFT) then x = x - 100 * DT end
end
```

## Typing

```lua
text_typed() -> string                          -- characters typed this frame
typing_begin([text], [{max_length, multiline}]) -- start a typing span
typing_state() -> {active, done, cancelled, text, cursor, line, column, key}
typing_end() -> text                            -- stop it, returning the text
```

`text_typed()` gives the characters typed this frame (already shifted, accented and so on, as the keyboard layout makes them), for a script that does its own editing. It's empty while the visualizer doesn't have focus.

For a text field, a chat line or a name entry, a typing span does the editing for you. `typing_begin` starts one, with optional starting text (the cursor goes at its end), a `max_length` in characters and `multiline`. While it's active, the app edits the text: typing and pasting insert at the cursor, Backspace and Delete remove (with Ctrl, a whole word), Left/Right move (Ctrl: by word), Home/End go to the start and end of the line, and in a multiline span Up/Down move between lines. The script draws everything itself, from `typing_state()`:

- `active`: the span is open
- `done`: it ended with Enter (in a multiline span Shift+Enter adds a new line instead)
- `cancelled`: it ended with Escape or by losing focus
- `text`: the text so far
- `cursor`: how many characters come before the cursor, counting a new line as one
- `line`, `column`: the cursor's line (from 1) and its column in that line (from 0)
- `key`: the last key pressed this frame while the span was active, as a key name (`"up"`, `"enter"`, ...), or nil; handy for things the span doesn't do itself, like recalling earlier lines with Up in a single-line span

While a span is active, the script's actions don't fire (`input` says `"up"`, `input_down` false), so typing a "w" doesn't also walk forward. The state stays readable after the span ends (with `done` or `cancelled` set) until the next `typing_begin`; `typing_end` closes the span from the script's side. Escape still belongs to the app: it ends the span (as cancelled) along with taking focus away.

The font is monospace, so drawing the cursor is simple: it sits `column * 6` font pixels from the start of its line (times `height / FONT_HEIGHT` when the text is drawn bigger).

```lua
local NAME_SCALE = 2
function render(width, height, left, right)
    local s = typing_state()
    if not s.active and not s.done and has_focus() then typing_begin(name or "", { max_length = 16 }) end
    if s.done then name = s.text end
    text(20, 20, s.text, { r = 255, g = 255, b = 255 }, FONT_HEIGHT * NAME_SCALE)
    if s.active then
        local x = 20 + s.column * 6 * NAME_SCALE
        rect(x, 20, x + NAME_SCALE, 20 + FONT_HEIGHT * NAME_SCALE, { r = 255, g = 200, b = 60 })
    end
end
```

See `terminal.lua` (New > terminal) for a full example: a command prompt with history.

## Playing notes

```lua
play_note(key, [options]) -> bool    -- options: channel, velocity, duration, delay
note_on(key, [options]) -> bool      -- options: channel, velocity; held until note_off
note_off(key, [channel])
stop_notes()                         -- let go of everything, cancel every sequence
notes_playable() -> bool             -- whether notes can play right now
```

While a MIDI song is loaded with a soundfont, a script can play notes of its own, on the song's instruments: a note on channel 3 sounds like the song's channel 3 does at that moment (its instrument, volume, pan and pitch bend). Without a MIDI song or a soundfont (or with a plain audio file) there's nothing to play on, so these return false and do nothing; `notes_playable()` says which. Notes play whether the song is paused or not.

- `key`: MIDI note number, 0 to 127 (60 is middle C)
- `channel`: 0 to 15, as in `midi_channels()`. Left out, or a channel the song doesn't use (not every song has a channel 0), it's the song's lowest channel.
- `velocity`: how hard, 1 to 127 (default 100)
- `duration`: seconds (default 0.5); `play_note` only
- `delay`: seconds from now until it starts (default 0); `play_note` only

`play_note` is fire and forget: the note ends by itself after `duration`. `note_on` holds the note until `note_off` with the same key (and channel, if one was given), for things like playing along on the keyboard, where the note should last as long as the key is held:

```lua
local PLAY = input_register("play", "space")

function render(width, height, left, right)
    local state = input(PLAY)
    if state == "pressed" then note_on(60) end
    if state == "released" then note_off(60) end
end
```

Script notes play on a separate synthesizer from the song's, mixed in at the output with very little delay (tens of milliseconds, not the song's audio buffer), and they never cut off the song's notes or get cut off by them. They show up in `active_notes()` while they sound, with `source = "script"` (the song's own notes have `source = "song"`), so a visualizer lights up for them like for the song's, and one that only wants the score can skip them. Their sound is in the `left`/`right` samples `render()` gets, mixed with the song's (and on their own while the song is paused), so the waveform, `fft_left`/`fft_right`, the levels and `onset()` react to them too. They aren't in `notes_between` or `upcoming_notes`, which read the song file. Everything a script plays stops when it's reloaded or restarted, when another song loads, and when the visualizer is closed or hidden. A script holding notes with `note_on` should let go when it loses focus (`has_focus()`), since a key released while the visualizer didn't have focus never reports `"released"`.

## Sequences

```lua
sequence_register(notes) -> id               -- once, outside render()
sequence_play(id, [options]) -> handle       -- options: channel, transpose, tempo, delay
sequence_stop(handle)
```

A sequence is a little score a script registers once and plays whenever it likes: a fanfare, an arpeggio, a riff to answer the song with. Times are in beats, so the same sequence fits any tempo. `notes` is a list, and each entry is either a key number (a one-beat note) or a table:

- `key`: MIDI note number; leave it out for a rest
- `at`: when it starts, in beats from the start of the sequence; defaults to where the previous entry ended, so a melody can just be listed in order
- `length`: beats (default 1)
- `velocity`: 1 to 127 (default 100)
- `channel`: this note's channel, over the one `sequence_play` picks

```lua
local fanfare = sequence_register({
    { key = 60, length = 0.5 }, { key = 64, length = 0.5 }, { key = 67, length = 0.5 },
    { length = 0.5 },                                         -- a rest
    { key = 72, length = 2, velocity = 120 },
    { key = 48, at = 2, length = 2 },                         -- under the top note
})

if onset() and notes_playable() then
    sequence_play(fanfare, { transpose = 5 })
end
```

`sequence_play` options: `channel` (as for `play_note`; for notes that don't name their own), `transpose` (semitones up, or down when negative; notes pushed past 0 to 127 are skipped), `tempo` (beats per minute) and `delay` (seconds before it starts). Without a `tempo` it plays at the song's tempo right now, as heard (times the playback speed), so it keeps time with the song; 120 for a song with no tempo marking. It returns a handle for `sequence_stop`, which cancels the notes still to come and lets go of the ones sounding, or nil when notes can't play (see `notes_playable()`). The same sequence can play several times at once, each with its own handle. A script can register up to 10,000 sequences.

See `keyboard.lua` (play along on the keyboard) for `note_on`/`note_off` in use.

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

## Testing scripts from the command line

```
synththing run-script my_script.lua --song song.mid --frames 1800
```

Runs a script with no window, playing the song into it, and prints its errors (with the frame and song position each happened at), its log and a summary; the exit code is 0 if it ran cleanly and 1 if it had errors. Add `--screenshot frame.png` to save the last frame as an image and see what it drew, or `--video clip.mp4` to record the whole run as a video with its sound (every frame, at 1080p unless `--video-size` says otherwise). `DT` is exactly 1/60 per frame and `TIME` counts frames, so runs are repeatable, and saved data isn't written unless you add `--save-data`. Mouse and keyboard input isn't simulated. All the options are in `docs/cli.md` in the repository, or `synththing run-script --help`.

## Performance

A script runs on every rendered frame while it's on screen, so the usual budget is the 60fps frame, ~16.7ms, but each draw call's cost scales with how many pixels it actually touches, not just how many calls you make, so a heatmap-style script needs more care than a line plot.

The top of the Script Settings tab shows how long `render()` takes (average and worst over the last second, drawing included), against that budget. A script that averages more than 16.7 ms for 3 seconds straight drops to 30 fps: it renders every other frame, the visualizer keeps showing its last picture in between, and nothing is lost meanwhile (the audio and any key presses arrive with the next render, and `DT` covers the whole gap). It stays at 30 through Restart and song changes, and goes back to 60 when the script's code changes or another script loads, or with "Try 60 fps again". `run-script` prints the same numbers at the end of a run.

- The buffer is capped at ~1280x720 worth of pixels and scaled up (nearest-neighbor) to fill larger views, so either fullscreen on a large monitor doesn't multiply your render cost, but it's still worth keeping draw counts sane.
- `spectrogram.lua` run-length-merges same-colored cells in a column into one rect instead of one draw per cell, worth copying for any other grid/heatmap-shaped script.
- Keep history buffers fixed-size ring buffers (see `waveform.lua`), not growing/shrinking arrays, removing from the front of a Lua table is O(n), and doing that every frame adds up.

## Bundled scripts, as reference

The first seven are copied into your scripts folder on first run (one that came out after your first run, like `pulse.lua`, is under New instead); the last two are templates only, made with New when you want them.

- `waveform.lua`, a scrolling oscilloscope trace; the simplest ring-buffer example
- `fft.lua`, log-spaced spectrum bars, left/right overlap shown as a third color; frequency labels (text) and loudness meters (`level_left`/`level_right`)
- `keyboard.lua`, an 88-key piano with falling notes from `notes_between` (exact lengths, adjustable look-ahead), beat and numbered bar lines (`beat`, `bar`, `time_at_beat`), octave labels; two-pass enabled/disabled channel rendering; and play along: with focus, A to L play a major scale on the song's instrument (`note_on`/`note_off`), the arrows move the span and change the key
- `spectrogram.lua`, a scrolling time/frequency heatmap; run-length merging, pause-awareness, live-tunable resolution, frequency labels, optional onset ticks (`onset`)
- `letters.lua`, one glyph per channel, colored by pitch; a from-scratch bitmap font
- `snake.lua`, one snake per channel hunting apples spawned by note-ons; apples are sprites, a text scoreboard, and a `player_channel` setting to steer one snake yourself with rebindable steering actions (best length saved with `store_set`); the most elaborate example, worth reading end to end
- `disco.lua`, a spinning mirror ball firing a laser per note, colored by channel and aimed by pitch, anticipated with `upcoming_notes()`
- `game.lua`, a platformer whose level is the music: notes are platforms, channels take turns being solid; rebindable controls, recolored player sprite (`palette`, `tint`), best scores per song (`song_id`)
- `pulse.lua`, a ring that beats with the song: the time signature's beats around a circle, a polygon turning with the beat and swelling with loudness, eighth-note sprites bursting out on onsets, and a tempo/bar readout
- `settings_demo.lua`, exercises every setting type; not a music visualizer, a reference for the settings API itself
- `input_demo.lua`, mouse, keyboard actions and cursor control: a paint toy with its own cursor; not a music visualizer either
- `terminal.lua`, a typing span: a command prompt with history, its prompt and cursor drawn from a sprite sheet; not a music visualizer
