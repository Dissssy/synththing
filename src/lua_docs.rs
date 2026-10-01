//! The in-app scripting reference ("Docs" button next to Settings) — one
//! embedded page, not a hosted/multi-page wiki, since the whole API is a
//! few dozen functions. Content is plain structured data rather than parsed
//! markdown: `app.rs` renders a `Section`/`Block` list directly with
//! egui's own heading/label/code widgets, so there's no markdown renderer
//! (hand-rolled or otherwise) to maintain for formatting this doesn't need.

pub struct Section {
    pub title: &'static str,
    pub blocks: &'static [Block],
}

pub enum Block {
    P(&'static str),
    Code(&'static str),
    Bullets(&'static [&'static str]),
}

pub fn sections() -> &'static [Section] {
    &[
        Section {
            title: "Getting started",
            blocks: &[
                Block::P(
                    "A visualizer script is a .lua file that defines one function, called once \
                     per frame:",
                ),
                Block::Code("function render(width, height, left, right)\n    -- draw here\nend"),
                Block::P(
                    "width/height are the pixel buffer's current size (it resizes with the \
                     panel — don't assume a fixed size). left/right are the new stereo samples \
                     played since the last frame, as flat 1-indexed tables of numbers — empty \
                     while paused, which is normal, not an error.",
                ),
                Block::P(
                    "Live reload: editing a script recompiles it on a fresh Lua VM. A script \
                     that fails to compile leaves whatever was running before still running — \
                     the error shows above the editor and in this script's log (see Settings), \
                     and stays there until you fix it or revert, even while the old script keeps \
                     rendering fine in the meantime.",
                ),
            ],
        },
        Section {
            title: "Drawing",
            blocks: &[
                Block::P("Four functions, all color-table based:"),
                Block::Code(
                    "clear({r,g,b})\nline(x0, y0, x1, y1, {r,g,b,a})\nrect(x0, y0, x1, y1, {r,g,b,a})\npixel(x, y, {r,g,b,a})",
                ),
                Block::P(
                    "a is opacity, 0.0..1.0, defaults to 1.0. An opaque draw (a = 1) overwrites; \
                     a translucent one alpha-blends over what's already there. clear always \
                     overwrites regardless of a.",
                ),
                Block::P(
                    "The buffer is cleared to all-black before your render() runs — there's no \
                     double buffering, so anything you want visible this frame has to be drawn \
                     this frame, including whatever scrolling history your own script is \
                     keeping track of.",
                ),
            ],
        },
        Section {
            title: "Audio data",
            blocks: &[
                Block::Code("fft_left(samples) -> spectrum\nfft_right(samples) -> spectrum"),
                Block::P(
                    "Windowed magnitude spectrum of a channel's last ~1024 samples, computed in \
                     Rust so a script never has to do its own FFT (interpreted Lua doing a \
                     1024-point FFT every frame would blow the frame budget by itself). Pass the \
                     left/right tables render() was given; the analyzer keeps its own internal \
                     sliding window, so calling it with an empty table (e.g. while paused) just \
                     returns the same spectrum as last time, not an error.",
                ),
            ],
        },
        Section {
            title: "MIDI data",
            blocks: &[
                Block::Code(
                    "active_notes() -> notes       -- held right now: {channel, key, velocity}\nupcoming_notes() -> notes     -- changing within NOTE_LOOKAHEAD: adds `on`, `seconds_until`\nmidi_channels() -> channels   -- channels this file uses, sorted\nchannel_enabled(c) -> bool    -- the GUI's per-channel toggle\nset_channel_enabled(c, bool)  -- a script can mute/unmute a channel too",
                ),
                Block::P(
                    "All empty/true for a plain audio file — there's no score to read, so \
                     nothing here errors, it just has nothing to report.",
                ),
                Block::P(
                    "set_channel_enabled goes through the exact same command the GUI's own \
                     checkboxes send, so a script can't disable the last remaining enabled \
                     channel either — useful for things like a game script muting a dead \
                     player's channel without needing its own \"don't silence everything\" logic.",
                ),
            ],
        },
        Section {
            title: "Playback & timing",
            blocks: &[
                Block::Code(
                    "playback() -> {position, length, speed, paused, finished, loop_enabled}\nDT              -- seconds since the previous render() call (0.0 on the first)\nSAMPLE_RATE     -- the engine's sample rate, in Hz\nNOTE_LOOKAHEAD  -- seconds upcoming_notes() looks ahead",
                ),
                Block::P(
                    "Check playback().paused before writing into a scrolling history buffer — \
                     otherwise the picture keeps scrolling through a frozen spectrum while \
                     paused instead of actually freezing. See spectrogram.lua for the pattern.",
                ),
                Block::P(
                    "DT is for frame-rate-independent animation (e.g. a smooth sweep), not for \
                     gating logic — a visualizer should look right regardless of how fast frames \
                     are actually arriving.",
                ),
            ],
        },
        Section {
            title: "Logging",
            blocks: &[
                Block::Code("log(message)"),
                Block::P(
                    "Appends to this script's log, shown in the Settings popup. Identical \
                     consecutive messages collapse into one entry with a count instead of \
                     flooding the pane — safe to call every single frame.",
                ),
            ],
        },
        Section {
            title: "Debugging",
            blocks: &[
                Block::Code("debug_locals([label])"),
                Block::P(
                    "Snapshots whatever local variables (and function parameters — Lua treats \
                     those the same way) are in scope at the exact point you call it, shown as a \
                     nested, expandable tree in the Settings popup, under Variables. Call it from \
                     wherever in your script you actually want visibility — inside a loop, after \
                     a specific branch, wherever — not just at the top level.",
                ),
                Block::P(
                    "Only one snapshot is kept; calling it again (from the same or a different \
                     spot) replaces the previous one. label is optional and just shows up next \
                     to the snapshot so you can tell which call site it came from.",
                ),
                Block::P(
                    "This only sees true locals and parameters — a module-level value declared \
                     with local outside any function (the usual way the bundled scripts keep \
                     their state, e.g. waveform.lua's history) is an upvalue of render, not a \
                     local inside it, so it won't show up unless you also have a local alias or \
                     parameter in scope at the call site.",
                ),
            ],
        },
        Section {
            title: "Settings",
            blocks: &[
                Block::P(
                    "A script can expose typed, user-editable values that show up as widgets in \
                     the Settings popup. Call the matching function every frame — the first call \
                     each compile registers the descriptor (default/range/options); every call \
                     after that just returns the live value, so dragging a slider in the popup \
                     updates the running script immediately, with no recompile and without \
                     disturbing any history the script is keeping.",
                ),
                Block::Code(
                    "setting_bool(key, default) -> bool\nsetting_int(key, default, min, max) -> integer\nsetting_float(key, default, min, max) -> number\nsetting_color(key, default) -> {r,g,b}\nsetting_string(key, default) -> string\nsetting_selection(key, options, defaults, max_selections) -> selected",
                ),
                Block::P(
                    "selection picks up to max_selections of a fixed option list; a pick past \
                     the cap evicts whichever option was selected longest ago, so there's no \
                     disabled-checkbox state to design around.",
                ),
                Block::P(
                    "Values persist to <script>.lua.settings.json next to the script and are \
                     re-applied on the next load, falling back to the script's own default for \
                     anything the sidecar doesn't have.",
                ),
                Block::Code(
                    "local accent = setting_color(\"accent\", { r = 51, g = 204, b = 255 })\nlocal bins = setting_int(\"bins\", 36, 8, 96)",
                ),
            ],
        },
        Section {
            title: "Performance",
            blocks: &[
                Block::P(
                    "A script runs on every rendered frame, so the usual budget is the 60fps \
                     frame, ~16.7ms — but each draw call's cost scales with how many pixels it \
                     actually touches, not just how many calls you make, so a heatmap-style \
                     script needs more care than a line plot.",
                ),
                Block::Bullets(&[
                    "The panel downscales automatically above ~1280x720 worth of pixels (nearest-neighbor upscaled back to fill it) — fullscreen on a large monitor doesn't multiply your render cost, but it's still worth keeping draw counts sane.",
                    "spectrogram.lua run-length-merges same-colored cells in a column into one rect instead of one draw per cell — worth copying for any other grid/heatmap-shaped script.",
                    "Keep history buffers fixed-size ring buffers (see waveform.lua), not growing/shrinking arrays — removing from the front of a Lua table is O(n), and doing that every frame adds up.",
                ]),
            ],
        },
        Section {
            title: "Bundled scripts, as reference",
            blocks: &[
                Block::Bullets(&[
                    "waveform.lua — a scrolling oscilloscope trace; the simplest ring-buffer example",
                    "fft.lua — log-spaced spectrum bars, left/right overlap shown as a third color",
                    "keyboard.lua — an 88-key piano with falling notes; two-pass enabled/disabled channel rendering",
                    "spectrogram.lua — a scrolling time/frequency heatmap; run-length merging, pause-awareness, live-tunable resolution",
                    "letters.lua — one glyph per channel, colored by pitch; a from-scratch bitmap font",
                    "snake.lua — one snake per channel hunting apples spawned by note-ons; the most elaborate example, worth reading end to end",
                    "settings_demo.lua — exercises every setting type; not a music visualizer, a reference for the settings API itself",
                ]),
            ],
        },
    ]
}
