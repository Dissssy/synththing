-- Radial Art: a ring of spectrum bars over a picture.
--
-- The picture is the playing song's album art, or a PNG or JPEG from this
-- script's assets folder (the folder button above the visualizer opens
-- it): one you pick in Settings, or a different one for every song with
-- Shuffle. In the middle of the ring: the song's time signature when it has
-- one (MIDI files), otherwise how far into the song it is.
--
-- Shows: image_load (pictures load on their own thread, so this keeps the
-- last picture up until the next is ready), album_art, list_assets,
-- setting_selection built from the folder, fft_band on a log scale,
-- time_signature.

script_options({ assets = true })

---------------------------------------------------------------- settings

local ALBUM_ART = "Album art"
local SHUFFLE = "Shuffle pictures"

-- Every picture in the assets folder. Press F5 after adding some, so the
-- Background setting lists them.
local pictures = {}
for _, name in ipairs(list_assets()) do
    local lower = name:lower()
    if lower:match("%.png$") or lower:match("%.jpe?g$") then pictures[#pictures + 1] = name end
end
local background_options = { ALBUM_ART, SHUFFLE }
for _, name in ipairs(pictures) do background_options[#background_options + 1] = name end

local function settings()
    return {
        background = setting_selection("background", background_options, { ALBUM_ART }, 1, {
            group = "Background",
            info = "The song's album art, a different picture from the assets folder for every song, or one picture. A song without album art gets a shuffled picture. Press F5 after adding pictures.",
        })[1] or ALBUM_ART,
        dim = setting_float("dim", 0.25, 0, 0.9, { group = "Background", info = "How much darker the picture is drawn." }),
        bars = setting_int("bars", 72, 16, 180, { group = "Ring", info = "How many bars go around the circle." }),
        radius = setting_float("radius", 0.2, 0.08, 0.4, { group = "Ring", info = "The circle's size, as a share of the window's shorter side." }),
        length = setting_float("length", 0.75, 0.2, 2, { group = "Ring", info = "How far the loudest bars reach, in circle radii." }),
        mirror = setting_bool("mirror", true, { group = "Ring", info = "Low notes at the top, the two halves mirrored. Off: once around." }),
        pulse = setting_float("pulse", 0.08, 0, 0.3, { group = "Ring", info = "How much the circle swells with the bass." }),
        album_color = setting_bool("album_color", false, { group = "Colors", info = "Draw the bars in the picture's main color." }),
        bar_color = setting_color("bar_color", { r = 255, g = 255, b = 255, a = 0.85 }, { group = "Colors", alpha = true }),
        circle_color = setting_color("circle_color", { r = 255, g = 255, b = 255, a = 0.1 }, { group = "Colors", alpha = true }),
        text_color = setting_color("text_color", { r = 255, g = 255, b = 255, a = 0.95 }, { group = "Colors", alpha = true }),
        time_signature = setting_bool("time_signature", true, {
            group = "Centre",
            info = "Show the time signature when the song has one. Off: always the time in the song.",
        }),
    }
end

---------------------------------------------------------------- background

-- The shuffled picture for this song, picked again when a song loads.
local shuffled, last_loads = nil, nil
-- The picture drawn last, kept up while the next one loads.
local shown = nil

local function shuffled_picture()
    local loads = playback().song_loads
    if loads ~= last_loads or not shuffled then
        last_loads = loads
        if #pictures > 1 then
            local pick
            repeat pick = pictures[math.random(#pictures)] until pick ~= shuffled
            shuffled = pick
        else
            shuffled = pictures[1]
        end
    end
    return shuffled
end

-- The picture to draw now (a Picture table), or nil, and whether one is
-- still loading.
local function picture(choice, width, height)
    local longest = math.max(width, height)
    if choice == ALBUM_ART then
        local art = album_art(math.min(512, longest))
        if art then return art end
    end
    local name = (choice == ALBUM_ART or choice == SHUFFLE) and shuffled_picture() or choice
    if not name then return nil, false end
    -- Asked for in steps of 256 pixels, so resizing the window doesn't load
    -- it again at every size on the way.
    local size = math.min(4096, math.ceil(longest / 256) * 256)
    local loaded, why = image_load(name, size)
    if why then log(why) end
    return loaded, not loaded and not why
end

local function background(s, width, height)
    local now, loading = picture(s.background, width, height)
    if now or not loading then shown = now end
    if not shown then
        clear(theme().background)
        return nil
    end
    local scale = math.max(width / shown.width, height / shown.height) -- cover the window
    sprite(shown.sprite, (width - shown.width * scale) / 2, (height - shown.height * scale) / 2, scale)
    return shown
end

---------------------------------------------------------------- ring

local MIN_HZ, MAX_HZ = 40, 14000
local MIN_DB = -60
local shown = {}
local bass = 0

local function bar_values(count, left, right)
    local sl, sr = fft_left(left), fft_right(right)
    local values = {}
    for i = 1, count do
        local lo = MIN_HZ * (MAX_HZ / MIN_HZ) ^ ((i - 1) / count)
        local hi = MIN_HZ * (MAX_HZ / MIN_HZ) ^ (i / count)
        local peak = math.max(fft_band(sl, lo, hi), fft_band(sr, lo, hi))
        local db = 20 * math.log(math.max(peak, 1e-6), 10)
        local v = math.max(0, math.min(1, (db - MIN_DB) / -MIN_DB))
        shown[i] = approach(shown[i] or 0, v, 40, 9)
        values[i] = shown[i]
    end
    local _, low = fft_band(sl, 30, 150)
    bass = approach(bass, math.min(1, low * 4), 30, 6)
    return values
end

local function format_time(seconds)
    seconds = math.max(0, math.floor(seconds))
    local h, m, s = seconds // 3600, seconds // 60 % 60, seconds % 60
    if h > 0 then return string.format("%d:%02d:%02d", h, m, s) end
    return string.format("%d:%02d", m, s)
end

local function centre_label(s)
    if s.time_signature then
        local num, den = time_signature()
        if num then return num .. "/" .. den end
    end
    return format_time(playback().position)
end

---------------------------------------------------------------- render

function render(width, height, left, right)
    local s = settings()
    local art = background(s, width, height)
    if s.dim > 0 then rect(0, 0, width, height, { r = 0, g = 0, b = 0, a = s.dim }) end

    local cx, cy = width / 2, height / 2
    local radius = math.min(width, height) * s.radius * (1 + s.pulse * bass)
    local count = s.mirror and s.bars // 2 or s.bars
    local values = bar_values(count, left, right)

    local color = s.bar_color
    if s.album_color and art then color = { r = art.color.r, g = art.color.g, b = art.color.b, a = s.bar_color.a } end
    local thickness = math.max(1, 2 * math.pi * radius / s.bars * 0.55)
    local reach = radius * s.length
    local gap = math.max(2, radius * 0.06)

    local function bar(angle, v)
        local dx, dy = math.sin(angle), -math.cos(angle)
        local r0 = radius + gap
        local r1 = r0 + math.max(thickness * 0.5, v * reach)
        line(cx + dx * r0, cy + dy * r0, cx + dx * r1, cy + dy * r1, color, thickness)
    end
    for i = 1, count do
        if s.mirror then
            local angle = math.pi * (i - 0.5) / count
            bar(angle, values[i])
            bar(-angle, values[i])
        else
            bar(2 * math.pi * (i - 0.5) / count, values[i])
        end
    end

    circle(cx, cy, radius, s.circle_color)
    circle_outline(cx, cy, radius, { r = s.text_color.r, g = s.text_color.g, b = s.text_color.b, a = 0.5 }, math.max(1, radius / 60))

    -- The biggest crisp text size (a whole multiple of FONT_HEIGHT) that fits.
    local label = centre_label(s)
    local w1 = text_size(label, FONT_HEIGHT)
    local scale = math.max(1, math.floor(math.min(radius * 1.4 / w1, radius * 0.9 / FONT_HEIGHT)))
    local tw, th = text_size(label, FONT_HEIGHT * scale)
    text(cx - tw / 2, cy - th / 2, label, s.text_color, FONT_HEIGHT * scale)
end
