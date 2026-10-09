-- Dashboard: four small views of the same sound, side by side.
--
-- A scope (the last moment of the waveform), a spectrum, level meters and a
-- scrolling piano roll, each in its own panel. Every panel is drawn in its
-- own coordinates, from (0, 0) at its top-left corner: translate() moves the
-- drawing into place and clip() keeps it inside the panel, so nothing a
-- panel draws can spill into its neighbors, however loud it gets. When a
-- sound starts (onset()), the whole dashboard gives a little shake, one more
-- translate() around everything.
--
-- Uses: translate, clip, push_view/pop_view for the panels; history_left/right
-- for the scope, fft_band for the spectrum, level_*/peak_* for the meters,
-- notes_between and channel_program for the roll and its legend; hsv and mix
-- for colors; approach for smoothing; line widths.

local GAP = 6
local TITLE_H = FONT_HEIGHT + 4
local ROLL_SECONDS = 4

local meter = { l = 0, r = 0, peak_l = 0, peak_r = 0 }
local bars = {}
local shake = 0

-- A color per channel, shared with the app's channel toggles: each one the
-- golden angle (about 137.5 degrees) around the color wheel from the last,
-- so neighboring channels never look alike.
local CHANNEL_COLORS = {}
for c = 0, 15 do
    CHANNEL_COLORS[c] = hsv(200 + c * 137.508, 0.6, 1)
end
channel_colors(CHANNEL_COLORS)

-- 0..1 on a -60..0 dB scale, the meters' and the spectrum's.
local function db_unit(v)
    local db = 20 * math.log(math.max(v, 1e-6), 10)
    return math.max(0, math.min(1, (db + 60) / 60))
end

local function scope(w, h, accent)
    local count = math.max(2, math.floor(w))
    local l = history_left(0.03, count)
    local r = history_right(0.03, count)
    local mid = h / 2
    line(0, mid, w, mid, { r = 255, g = 255, b = 255, a = 0.08 })
    local px, py
    for i = 1, count do
        local x = (i - 1) * (w - 1) / (count - 1)
        local y = mid - (l[i] + r[i]) * 0.5 * h * 0.9
        if px then line(px, py, x, y, accent, 2) end
        px, py = x, y
    end
end

local function spectrum(w, h, spec_l, spec_r)
    local n = math.max(1, math.floor(w / 6))
    local bar_w = w / n
    for i = 1, n do
        -- Log-spaced from 40 Hz to 12 kHz.
        local lo = 40 * (12000 / 40) ^ ((i - 1) / n)
        local hi = 40 * (12000 / 40) ^ (i / n)
        local target = db_unit(math.max(fft_band(spec_l, lo, hi), fft_band(spec_r, lo, hi)))
        bars[i] = math.max(target, (bars[i] or 0) - DT * 1.5)
        local bh = bars[i] * h
        rect((i - 1) * bar_w + 1, h - bh, i * bar_w - 1, h, hsv(260 - 260 * (i - 1) / n, 0.7, 0.4 + 0.6 * bars[i]))
    end
end

local function meters(w, h, accent)
    meter.l = approach(meter.l, level_left(), 26)
    meter.r = approach(meter.r, level_right(), 26)
    meter.peak_l = math.max(peak_left(), meter.peak_l - DT * 0.4)
    meter.peak_r = math.max(peak_right(), meter.peak_r - DT * 0.4)
    local bw = (w - GAP) / 2
    for i, m in ipairs({ { meter.l, meter.peak_l }, { meter.r, meter.peak_r } }) do
        local x = (i - 1) * (bw + GAP)
        rect(x, 0, x + bw, h, { r = 255, g = 255, b = 255, a = 0.05 })
        local level = db_unit(m[1])
        rect(x, h - level * h, x + bw, h, mix(accent, { r = 255, g = 80, b = 60 }, level ^ 4))
        local py = h - db_unit(m[2]) * h
        rect(x, py - 1, x + bw, py + 1, { r = 255, g = 255, b = 255, a = 0.9 })
    end
end

-- Which color is which: each channel's instrument right now, in its color,
-- in the top-left corner (over the oldest notes, out of the way of new ones).
local function legend()
    local y = 0
    for _, c in ipairs(midi_channels()) do
        local _, name = channel_program(c)
        if not name then return end
        local tw = text_size(name)
        rect(0, y, tw + 16, y + FONT_HEIGHT + 2, { r = 0, g = 0, b = 0, a = 0.6 })
        rect(4, y + 4, 10, y + 10, CHANNEL_COLORS[c])
        text(14, y + 1, name, { r = 230, g = 230, b = 240 })
        y = y + FONT_HEIGHT + 2
    end
end

local function roll(w, h)
    local now = playback().position
    local notes = notes_between(now - ROLL_SECONDS, now)
    if #notes == 0 then
        text(4, h / 2 - FONT_HEIGHT / 2, "no notes", { r = 255, g = 255, b = 255, a = 0.3 })
        return
    end
    local lo, hi = 127, 0
    for _, n in ipairs(notes) do
        lo, hi = math.min(lo, n.key), math.max(hi, n.key)
    end
    lo, hi = lo - 1, hi + 1
    local row_h = h / (hi - lo + 1)
    for _, n in ipairs(notes) do
        -- Now is the right edge; older notes scroll off to the left.
        local x0 = w - (now - n.start) / ROLL_SECONDS * w
        local x1 = w - (now - math.min(n.stop, now)) / ROLL_SECONDS * w
        local y = (hi - n.key) * row_h
        local color = CHANNEL_COLORS[n.channel]
        if not channel_enabled(n.channel) then color = mix(color, { r = 0, g = 0, b = 0 }, 0.7) end
        rect(x0, y, math.max(x1, x0 + 2), y + math.max(1, row_h - 1), color)
    end
    -- (Not in the mini player, where it would cover too much of the roll.)
    if display_mode() ~= "mini" then
        legend()
    end
end

-- One panel: a title (none in the mini player, for room), a frame, and
-- `draw(w, h)` inside it with (0, 0) at its top-left and nothing drawn
-- outside it.
local function panel(x, y, w, h, title, colors, draw)
    push_view()
    translate(x, y)
    rect(0, 0, w, h, colors.panel)
    local title_h = TITLE_H
    if display_mode() == "mini" then
        title_h = 6
    else
        text(6, 2, title, colors.title)
    end
    translate(6, title_h)
    local iw, ih = w - 12, h - title_h - 6
    clip(0, 0, iw, ih)
    draw(iw, ih)
    pop_view()
end

function render(width, height, left, right)
    local background = setting_color("background", { r = 10, g = 10, b = 18 })
    local accent = setting_color("accent", { r = 90, g = 200, b = 255 })
    local shake_amount = setting_float("shake", 4, 0, 16, { info = "How far the dashboard jumps when a sound starts, in pixels" })
    local colors = {
        panel = mix(background, { r = 255, g = 255, b = 255 }, 0.05),
        title = mix(accent, { r = 255, g = 255, b = 255 }, 0.5),
    }

    -- fft_* every frame, so their windows stay current.
    local spec_l, spec_r = fft_left(left), fft_right(right)
    local hit, strength = onset()
    if hit then shake = math.min(1, shake + 0.4 + strength) end
    shake = math.max(0, shake - DT * 4)

    clear(background)
    push_view()
    local s = shake * shake_amount
    translate(math.sin(TIME * 90) * s, math.cos(TIME * 77) * s)

    local views = {
        { "scope", function(pw, ph) scope(pw, ph, accent) end },
        { "spectrum", function(pw, ph) spectrum(pw, ph, spec_l, spec_r) end },
        { "notes", roll },
        { "levels", function(pw, ph) meters(pw, ph, accent) end },
    }
    if height > width then
        -- Taller than wide (a narrow mini player, say): one above another.
        local h = (height - 5 * GAP) / 4
        for i, v in ipairs(views) do
            panel(GAP, GAP + (i - 1) * (h + GAP), width - 2 * GAP, h, v[1], colors, v[2])
        end
    else
        local w = (width - 3 * GAP) / 2
        local h = (height - 3 * GAP) / 2
        for i, v in ipairs(views) do
            local col, row = (i - 1) % 2, (i - 1) // 2
            panel(GAP + col * (w + GAP), GAP + row * (h + GAP), w, h, v[1], colors, v[2])
        end
    end
    pop_view()
end
