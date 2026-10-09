-- Piano keyboard visualizer.
--
-- Bottom strip: an 88-key keyboard (MIDI 21..108), C keys labeled with their
-- octave. A key fills with its channel's color while it's held
-- (active_notes()), but only if that channel is enabled in the GUI. Disabled
-- channels never light up the keys.
-- Above it: a "falling notes" lane. Every note in the next few seconds
-- (notes_between(), so each comes with its exact start and stop) descends
-- toward its key; a bar's length is the note's duration. Faint lines mark
-- the beats and brighter, numbered ones the bars (beat(), bar(),
-- time_at_beat()), for MIDI files that have tempo information.
--
-- Bars are drawn in two passes: enabled channels first (opaque), then
-- disabled channels (translucent, alpha DISABLED_ALPHA). Because the
-- translucent pass runs last, a disabled note still shows its ghost even
-- where an enabled note would otherwise sit in front of it.
--
-- Play along: click the visualizer (or use the Fullscreen visualizer) and
-- the A S D F G H J K L keys play nine notes of a major scale, marked
-- above the keyboard; Left/Right (or a controller's d-pad) move that
-- span a note at a time and Up/Down change the key (C major, C# major, ...). Notes hold for as long
-- as the key does (note_on/note_off) and sound like the song's own
-- instrument on the "play_channel" setting's channel (0: the song's
-- first). Every key can be rebound with the Controls button.

local FIRST_KEY = 21   -- A0
local LAST_KEY = 108   -- C8
-- A grand piano's white keys are about 6.4 times as long as they're wide
-- (23.5 mm by 150 mm): the keyboard's height follows from the keys' width,
-- so they keep that shape whatever the window's. Never more than
-- MAX_KEYBOARD_FRACTION of the height, so a very wide, short window still
-- has room for the falling notes.
local WHITE_KEY_ASPECT = 150 / 23.5
local MAX_KEYBOARD_FRACTION = 0.4

-- Pitch classes (note % 12) that are white keys: C D E F G A B.
local WHITE_PCS = { [0] = true, [2] = true, [4] = true, [5] = true, [7] = true, [9] = true, [11] = true }

-- (The app's channel toggles use these colors by default too, so they
-- match without channel_colors(): change them here, and call
-- channel_colors() with the new ones.)
local CHANNEL_COLORS = {
    { r = 90,  g = 170, b = 255 },
    { r = 255, g = 130, b = 90 },
    { r = 120, g = 230, b = 140 },
    { r = 240, g = 210, b = 90 },
    { r = 200, g = 120, b = 255 },
    { r = 90,  g = 230, b = 230 },
    { r = 255, g = 110, b = 170 },
    { r = 170, g = 200, b = 120 },
}

local function is_white(key)
    return WHITE_PCS[key % 12] == true
end

local function channel_tint(channel, alpha)
    local base = CHANNEL_COLORS[(channel % #CHANNEL_COLORS) + 1]
    return { r = base.r, g = base.g, b = base.b, a = alpha }
end

-- Play-along controls (see the top). Registered once; the user can rebind
-- them under Controls.
local PLAY = {}
for i, key in ipairs({ "a", "s", "d", "f", "g", "h", "j", "k", "l" }) do
    PLAY[i] = input_register("play " .. i, key)
end
local PLAY_LETTERS = { "A", "S", "D", "F", "G", "H", "J", "K", "L" }
local SPAN_LEFT = input_register("span left", { "left", "pad_dpad_left" })
local SPAN_RIGHT = input_register("span right", { "right", "pad_dpad_right" })
local KEY_UP = input_register("key up", { "up", "pad_dpad_up" })
local KEY_DOWN = input_register("key down", { "down", "pad_dpad_down" })

local MAJOR = { 0, 2, 4, 5, 7, 9, 11 }
local PLAYER_COLOR = { r = 255, g = 214, b = 90 }

-- The key (0 = C ... 11 = B) and the span's first note, in scale steps
-- from that key's tonic around middle C. Both remembered between runs.
local tonic = store_get("tonic") or 0
local span = store_get("span") or 0
local sounding = {} -- [play slot] = { key = .., channel = .. } while held

-- The MIDI key of scale step `step` (0 = the tonic, 7 = an octave up).
local function step_key(step)
    return 60 + tonic + 12 * (step // 7) + MAJOR[step % 7 + 1]
end

-- Keep the whole span on the keyboard.
local function clamp_span()
    while step_key(span + #PLAY - 1) > LAST_KEY do span = span - 1 end
    while step_key(span) < FIRST_KEY do span = span + 1 end
end
clamp_span()

local function release_all()
    for _, s in pairs(sounding) do
        note_off(s.key, s.channel)
    end
    sounding = {}
end

-- Read the play-along keys; returns the span's keys.
local function play_along(channel, velocity)
    local moved = false
    if input(SPAN_LEFT) == "pressed" then span = span - 1; moved = true end
    if input(SPAN_RIGHT) == "pressed" then span = span + 1; moved = true end
    if input(KEY_UP) == "pressed" then tonic = (tonic + 1) % 12; moved = true end
    if input(KEY_DOWN) == "pressed" then tonic = (tonic - 1) % 12; moved = true end
    if moved then
        clamp_span()
        store_set("tonic", tonic)
        store_set("span", span)
    end

    local keys = {}
    for i, action in ipairs(PLAY) do
        keys[i] = step_key(span + i - 1)
        local state = input(action)
        if state == "pressed" then
            if sounding[i] then note_off(sounding[i].key, sounding[i].channel) end
            if note_on(keys[i], { channel = channel, velocity = velocity }) then
                sounding[i] = { key = keys[i], channel = channel }
            end
        elseif sounding[i] and (state == "released" or state == "up") then
            note_off(sounding[i].key, sounding[i].channel)
            sounding[i] = nil
        end
    end
    return keys
end

-- Per-key horizontal layout, rebuilt only when the window width changes.
local layout = { width = -1, keys = {}, white_w = 1 }

local function rebuild_layout(width)
    layout.width = width
    layout.keys = {}

    local white_total = 0
    for k = FIRST_KEY, LAST_KEY do
        if is_white(k) then white_total = white_total + 1 end
    end
    local white_w = width / white_total
    layout.white_w = white_w

    local whites_seen = 0
    for k = FIRST_KEY, LAST_KEY do
        if is_white(k) then
            local x0 = whites_seen * white_w
            layout.keys[k] = { x0 = x0, x1 = x0 + white_w, white = true }
            whites_seen = whites_seen + 1
        else
            -- Straddle the boundary just past the white key below this one.
            local boundary = whites_seen * white_w
            local bw = white_w * 0.62
            layout.keys[k] = { x0 = boundary - bw / 2, x1 = boundary + bw / 2, white = false }
        end
    end
end

local function key_box(key, width)
    if layout.width ~= width then
        rebuild_layout(width)
    end
    return layout.keys[key]
end

function render(width, height, left, right)
    local background = setting_color("background", { r = 14, g = 14, b = 20 })
    local key_idle_white = setting_color("key_idle_white", { r = 232, g = 232, b = 238 })
    local key_idle_black = setting_color("key_idle_black", { r = 28, g = 28, b = 34 })
    local separator = setting_color("separator", { r = 60, g = 60, b = 72 })
    local disabled_alpha = setting_float("disabled_alpha", 0.22, 0.0, 1.0)
    local lookahead = setting_float("lookahead_seconds", 4.0, 0.5, 16.0)
    local show_beats = setting_bool("beat_lines", true)
    local show_octaves = setting_bool("octave_labels", true)
    local playing = setting_bool("play_along", true)
    local play_channel = setting_int("play_channel", 0, 0, 16) -- 1-16 as in the GUI; 0 = the song's first
    local play_velocity = setting_int("play_velocity", 100, 1, 127)

    -- Play along only while focused; let go of everything otherwise (a key
    -- released while focus was elsewhere never reports "released").
    local span_keys = nil
    if playing and has_focus() then
        span_keys = play_along(play_channel > 0 and (play_channel - 1) or nil, play_velocity)
    else
        release_all()
    end

    clear(background)

    if layout.width ~= width then
        rebuild_layout(width)
    end
    local kb_h = math.max(1, math.floor(math.min(layout.white_w * WHITE_KEY_ASPECT, height * MAX_KEYBOARD_FRACTION)))
    local kb_top = height - kb_h
    local lane_height = math.max(1, kb_top)

    local function time_to_y(t)
        local clamped = math.max(0.0, math.min(lookahead, t))
        return kb_top - (clamped / lookahead) * lane_height
    end

    local active = active_notes()
    local now = playback().position
    local notes = notes_between(now, now + lookahead)

    -- Which keys are held right now, enabled channels only.
    local held = {}
    for _, note in ipairs(active) do
        if channel_enabled(note.channel) then
            held[note.key] = channel_tint(note.channel, 1.0)
        end
    end
    for _, s in pairs(sounding) do
        held[s.key] = PLAYER_COLOR
    end

    -- Beat lines behind the notes: song time of each beat in the window,
    -- brighter and numbered where a bar starts.
    if show_beats and beat() then
        for b = math.ceil(beat(now)), math.floor(beat(now + lookahead)) do
            local t = time_at_beat(b)
            local y = math.floor(time_to_y(t - now))
            local bar_number, into = bar(t)
            if into < 0.01 then
                line(0, y, width, y, { r = 255, g = 255, b = 255, a = 0.22 })
                text(4, y - FONT_HEIGHT, tostring(bar_number), { r = 255, g = 255, b = 255, a = 0.45 })
            else
                line(0, y, width, y, { r = 255, g = 255, b = 255, a = 0.07 })
            end
        end
    end

    -- One pass over every bar for a given enabled/disabled selection. Notes
    -- already sounding are clamped to the keyboard edge by time_to_y.
    local function draw_bars(want_enabled, alpha)
        for _, note in ipairs(notes) do
            if channel_enabled(note.channel) == want_enabled then
                local box = key_box(note.key, width)
                if box then
                    rect(box.x0 + 1, time_to_y(note.stop - now), box.x1 - 1, time_to_y(note.start - now),
                        channel_tint(note.channel, alpha))
                end
            end
        end
    end

    -- Enabled first (opaque), then disabled on top (translucent).
    draw_bars(true, 1.0)
    draw_bars(false, disabled_alpha)

    -- Keyboard: all white keys (full height), then black keys on top.
    for k = FIRST_KEY, LAST_KEY do
        local box = key_box(k, width)
        if box and box.white then
            rect(box.x0 + 1, kb_top, box.x1 - 1, height, held[k] or key_idle_white)
        end
    end
    -- Octave labels on the C keys, when the keys are wide enough.
    local c_box = key_box(60, width)
    if show_octaves and c_box and c_box.x1 - c_box.x0 >= 7 then
        for k = 24, LAST_KEY, 12 do
            local box = key_box(k, width)
            local label = note_name(k)
            local tw = text_size(label)
            if box and tw <= (box.x1 - box.x0) * 2.2 then
                text(box.x0 + 2, height - FONT_HEIGHT - 2, label, { r = 90, g = 90, b = 110 })
            end
        end
    end

    local black_h = math.floor(kb_h * 0.62)
    for k = FIRST_KEY, LAST_KEY do
        local box = key_box(k, width)
        if box and not box.white then
            rect(box.x0, kb_top, box.x1, kb_top + black_h, held[k] or key_idle_black)
        end
    end

    line(0, kb_top, width, kb_top, separator)

    -- The play-along span: a marker and its letter above each key, and
    -- which key and controls in the corner.
    if span_keys then
        for i, k in ipairs(span_keys) do
            local box = key_box(k, width)
            if box then
                local color = sounding[i] and PLAYER_COLOR or { r = 255, g = 214, b = 90, a = 0.55 }
                rect(box.x0 + 1, kb_top - 4, box.x1 - 1, kb_top - 1, color)
                local tw = text_size(PLAY_LETTERS[i])
                text((box.x0 + box.x1 - tw) / 2, kb_top - 6 - FONT_HEIGHT, PLAY_LETTERS[i], color)
            end
        end
        -- Which key, and the controls; not in the mini player, too small
        -- for a line of help.
        if display_mode() ~= "mini" then
            local label = note_name(tonic, false) .. " major   A-L play   Left/Right move   Up/Down key"
            if not notes_playable() then
                label = "play along needs a MIDI song and a soundfont"
            end
            local tw = text_size(label)
            rect(width - tw - 12, 4, width - 4, 8 + FONT_HEIGHT, { r = 0, g = 0, b = 0, a = 0.6 })
            text(width - tw - 8, 6, label, PLAYER_COLOR)
        end
    end
end
