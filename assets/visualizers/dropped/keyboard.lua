-- Piano keyboard visualizer.
--
-- Bottom strip: an 88-key keyboard (MIDI 21..108). A key fills with its
-- channel's color while it's held (active_notes()), but only if that channel
-- is enabled in the GUI. Disabled channels never light up the keys.
-- Above it: a "falling notes" lane. Upcoming note-ons (upcoming_notes())
-- descend toward their key; a bar's length is the note's duration, found by
-- pairing each note-on with its matching note-off within NOTE_LOOKAHEAD.
--
-- Bars are drawn in two passes: enabled channels first (opaque), then
-- disabled channels (translucent, alpha DISABLED_ALPHA). Because the
-- translucent pass runs last, a disabled note still shows its ghost even
-- where an enabled note would otherwise sit in front of it.

local FIRST_KEY = 21   -- A0
local LAST_KEY = 108   -- C8
local KEYBOARD_FRACTION = 0.28

-- Pitch classes (note % 12) that are white keys: C D E F G A B.
local WHITE_PCS = { [0] = true, [2] = true, [4] = true, [5] = true, [7] = true, [9] = true, [11] = true }

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

-- Per-key horizontal layout, rebuilt only when the window width changes.
local layout = { width = -1, keys = {} }

local function rebuild_layout(width)
    layout.width = width
    layout.keys = {}

    local white_total = 0
    for k = FIRST_KEY, LAST_KEY do
        if is_white(k) then white_total = white_total + 1 end
    end
    local white_w = width / white_total

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

-- Find the note-off matching `on` at or after index `from` in `upcoming`.
local function matching_off(upcoming, from, channel, key)
    for j = from, #upcoming do
        local o = upcoming[j]
        if (not o.on) and o.channel == channel and o.key == key then
            return o.seconds_until
        end
    end
    return nil
end

function render(width, height, left, right)
    local background = setting_color("background", { r = 14, g = 14, b = 20 })
    local key_idle_white = setting_color("key_idle_white", { r = 232, g = 232, b = 238 })
    local key_idle_black = setting_color("key_idle_black", { r = 28, g = 28, b = 34 })
    local separator = setting_color("separator", { r = 60, g = 60, b = 72 })
    local disabled_alpha = setting_float("disabled_alpha", 0.22, 0.0, 1.0)

    clear(background)

    local kb_h = math.max(1, math.floor(height * KEYBOARD_FRACTION))
    local kb_top = height - kb_h
    local lane_height = math.max(1, kb_top)
    local lookahead = math.max(0.001, NOTE_LOOKAHEAD)

    local function time_to_y(t)
        local clamped = math.max(0.0, math.min(lookahead, t))
        return kb_top - (clamped / lookahead) * lane_height
    end

    local active = active_notes()
    local upcoming = upcoming_notes()

    -- Which keys are held right now, enabled channels only.
    local held = {}
    for _, note in ipairs(active) do
        if channel_enabled(note.channel) then
            held[note.key] = channel_tint(note.channel, 1.0)
        end
    end

    -- One pass over every bar for a given enabled/disabled selection.
    local function draw_bars(want_enabled, alpha)
        for i, ev in ipairs(upcoming) do
            if ev.on and channel_enabled(ev.channel) == want_enabled then
                local box = key_box(ev.key, width)
                if box then
                    local off_t = matching_off(upcoming, i + 1, ev.channel, ev.key) or lookahead
                    rect(box.x0 + 1, time_to_y(off_t), box.x1 - 1, time_to_y(ev.seconds_until),
                        channel_tint(ev.channel, alpha))
                end
            end
        end
        for _, note in ipairs(active) do
            if channel_enabled(note.channel) == want_enabled then
                local box = key_box(note.key, width)
                if box then
                    local off_t = matching_off(upcoming, 1, note.channel, note.key) or lookahead
                    rect(box.x0 + 1, time_to_y(off_t), box.x1 - 1, kb_top,
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
    local black_h = math.floor(kb_h * 0.62)
    for k = FIRST_KEY, LAST_KEY do
        local box = key_box(k, width)
        if box and not box.white then
            rect(box.x0, kb_top, box.x1, kb_top + black_h, held[k] or key_idle_black)
        end
    end

    line(0, kb_top, width, kb_top, separator)
end
