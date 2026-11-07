-- Everything: a test page that calls every part of the script API it can
-- and shows what comes back, to check them by eye. Each new API call gets a
-- line here as it's added (a test lists the ones still to cover).
--
--   Clock     now(), now(true), unix_time(), format_time() (text, number,
--             table, bad code); "REC" while recording time is in use
--   Noise     noise() as a 1D line and a 2D field; ease() for each curve
--   Random    random(seed) must show the same numbers every frame;
--             math.random() changes every frame unless recording
--   Theme     every theme() color; switch themes and it follows at once
--   Song      song_details() for the playing song (star it, edit it, play
--             it to the end: it updates)
--   Up next   up_next(): queue songs (marked Q), then what follows
--   Image     image_load(): the first PNG or JPEG in the assets folder (the
--             folder button above opens it), at its own size and at 48 px;
--             "loading" for a moment, then the picture and its color

script_options({ assets = true })

local DIM = { r = 140, g = 140, b = 150 }
local BAD = { r = 240, g = 90, b = 80 }
local GOOD = { r = 120, g = 220, b = 120 }

local function show(v)
    if type(v) == "table" then
        local parts = {}
        for i, item in ipairs(v) do parts[i] = tostring(item) end
        return "{" .. table.concat(parts, ", ") .. "}"
    elseif type(v) == "number" and math.type(v) == "float" then
        return string.format("%.3f", v)
    end
    return tostring(v)
end

-- A column that writes lines down from the top.
local function column(x, y, w, colors)
    local c = { x = x, y = y, w = w, colors = colors }
    function c:line(value, color)
        local s = tostring(value)
        -- Cut lines that would run past the column.
        while #s > 4 and text_size(s) > self.w do s = s:sub(1, #s - 2) end
        text(self.x, self.y, s, color or self.colors.text)
        self.y = self.y + FONT_HEIGHT
    end
    function c:title(s)
        self.y = self.y + 4
        self:line(s, self.colors.accent)
    end
    return c
end

local function clock_panel(c)
    c:title("Clock")
    local t = now()
    c:line(format_time("%Y-%m-%d %H:%M:%S") .. (recording() and recording().phase == "recording" and "  REC" or ""))
    c:line(string.format("now(): %04d-%02d-%02d %02d:%02d:%02d.%03d", t.year, t.month, t.day, t.hour, t.minute, t.second, t.ms))
    c:line(string.format("weekday %d  yearday %d  offset %+ds", t.weekday, t.yearday, t.utc_offset))
    local u = now(true)
    c:line(string.format("UTC: %02d:%02d  unix %.1f", u.hour, u.minute, unix_time()))
    c:line("table: " .. format_time("%A %d %B, %I:%M %p", t))
    c:line("number 0: " .. format_time("%Y-%m-%d", 0))
    local ok = pcall(format_time, "%Q")
    c:line(ok and "bad code accepted (wrong)" or "bad code errors (right)", ok and BAD or GOOD)
end

local CURVES = { "linear", "in_out_sine", "in_out_cubic", "out_expo", "in_out_circ", "out_back", "out_elastic", "out_bounce", "in_quad", "in_quint" }

local function noise_panel(c)
    c:title("Noise & easing")
    -- 1D: a wandering line.
    local top, span = c.y + 2, 30
    local prev
    for x = 0, c.w - 1, 2 do
        local y = top + span / 2 + noise(x * 0.03, TIME * 0.7) * span / 2
        if prev then line(c.x + x - 2, prev, c.x + x, y, c.colors.accent) end
        prev = y
    end
    c.y = top + span + 4
    -- 2D: a drifting field.
    local cell = 8
    for gy = 0, 4 do
        for gx = 0, math.floor(c.w / cell) - 1 do
            local v = (noise(gx * 0.15, gy * 0.15, TIME * 0.3) + 1) / 2
            rect(c.x + gx * cell, c.y + gy * cell, c.x + gx * cell + cell, c.y + gy * cell + cell, { r = v * 255, g = v * 120, b = 255 - v * 255 })
        end
    end
    c.y = c.y + 5 * cell + 4
    -- Easing: each dot crosses the track once a second.
    local t = TIME % 1.5
    local label_w = 90
    for _, name in ipairs(CURVES) do
        local track = c.w - label_w - 8
        text(c.x, c.y, name, c.colors.weak)
        rect(c.x + label_w, c.y + 5, c.x + label_w + track, c.y + 6, c.colors.weak)
        circle(c.x + label_w + ease(name, t) * track, c.y + 6, 3, c.colors.accent)
        c.y = c.y + FONT_HEIGHT
    end
    local ok = pcall(ease, "wobble", 0.5)
    c:line(ok and "unknown curve accepted (wrong)" or "unknown curve errors (right)", ok and BAD or GOOD)
end

local function random_panel(c)
    c:title("Random")
    local gen = random(42)
    local fixed = {}
    for i = 1, 4 do fixed[i] = string.format("%.3f", gen:next()) end
    c:line("random(42): " .. table.concat(fixed, " ") .. " (steady)")
    c:line(string.format("d6 %d  10..20 %d", gen:next(6), gen:next(10, 20)))
    local ok = pcall(gen.next, gen, 5, 3)
    c:line(ok and "next(5, 3) accepted (wrong)" or "next(5, 3) errors (right)", ok and BAD or GOOD)
    c:line(string.format("math.random(): %.3f", math.random()))
    c:line(recording() and "recording: seeded from the song" or "not recording: changes every frame", c.colors.weak)
end

local THEME_KEYS = { "background", "window", "faint", "deep", "text", "widget_text", "strong_text", "weak_text", "selection", "accent", "link", "warning", "error", "code", "widget", "widget_hovered", "widget_pressed", "widget_outline", "border", "playing", "star" }

local function theme_panel(c, th)
    c:title("Theme")
    c:line(string.format("%s (%s)", th.name, th.dark and "dark" or "light"))
    for _, key in ipairs(THEME_KEYS) do
        local color = th[key]
        rect(c.x, c.y + 1, c.x + 10, c.y + 10, color)
        rect_outline(c.x, c.y + 1, c.x + 10, c.y + 10, DIM)
        text(c.x + 14, c.y, string.format("%s %d,%d,%d %.2f", key, color.r, color.g, color.b, color.a), c.colors.text)
        c.y = c.y + FONT_HEIGHT
    end
end

local function image_panel(c)
    c:title("Image")
    local name
    for _, file in ipairs(list_assets()) do
        if file:lower():match("%.png$") or file:lower():match("%.jpe?g$") then name = file break end
    end
    local missing = image_load("no such file.png")
    c:line("missing file: " .. show(missing), missing == nil and GOOD or BAD)
    if not name then return c:line("put a PNG or JPEG in the assets folder", c.colors.weak) end
    local full, why = image_load(name)
    local small = image_load(name, 48)
    if why then return c:line(why, BAD) end
    if not (full and small) then return c:line(name .. ": loading...", c.colors.weak) end
    c:line(string.format("%s: %dx%d, at 48: %dx%d", name, full.width, full.height, small.width, small.height))
    sprite(small.sprite, c.x, c.y)
    rect(c.x + small.width + 4, c.y, c.x + small.width + 20, c.y + 16, full.color)
    c.y = c.y + small.height + 2
end

local function song_panel(c, bottom)
    c:title("Song details")
    local d = song_details()
    if not d then return c:line("nil (nothing playing, or not in the library)", c.colors.weak) end
    local keys = {}
    for k in pairs(d) do keys[#keys + 1] = k end
    table.sort(keys)
    for _, k in ipairs(keys) do
        if c.y > bottom then return c:line("...", c.colors.weak) end
        local v = d[k]
        if k == "last_played" or k == "last_skipped" or k == "imported" then
            v = format_time("%Y-%m-%d %H:%M", v)
        end
        c:line(k .. ": " .. show(v), (k == "starred" and d.starred) and c.colors.star or nil)
    end
end

local function up_next_panel(c, bottom)
    c:title("Up next")
    local songs = up_next()
    c:line(string.format("%d songs, up_next(2) gives %d", #songs, #up_next(2)), c.colors.weak)
    for i, s in ipairs(songs) do
        if c.y > bottom then return c:line(string.format("... %d more", #songs - i + 1), c.colors.weak) end
        local len = s.length and string.format(" %d:%02d", s.length // 60, math.floor(s.length % 60)) or ""
        c:line((s.queued and "Q " or "  ") .. s.name .. len, s.queued and c.colors.accent or nil)
    end
end

function render(width, height)
    local th = theme()
    local colors = { text = th.text, weak = th.weak_text, accent = th.accent, star = th.star }
    clear(th.background)

    local pad = 6
    local cols = width >= 900 and 3 or (width >= 560 and 2 or 1)
    local w = math.floor((width - pad * (cols + 1)) / cols)
    local columns = {}
    for i = 1, cols do columns[i] = column(pad + (i - 1) * (w + pad), pad, w, colors) end
    local function pick(i) return columns[math.min(i, cols)] end

    clock_panel(pick(1))
    random_panel(pick(1))
    noise_panel(pick(1))
    theme_panel(pick(2), th)
    image_panel(pick(2))
    local c3 = pick(3)
    song_panel(c3, c3.y + (height - c3.y) * 0.55)
    up_next_panel(c3, height - FONT_HEIGHT)
end
