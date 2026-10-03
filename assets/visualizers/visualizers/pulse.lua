-- Pulse: a rhythm visualizer that follows the song's beats.
--
-- A ring of dots, one per beat of the bar (from the MIDI file's time
-- signature): the current beat lights up, every dot pulses on the beat, the
-- first beat of each bar is the big one. In the middle a polygon turns with
-- the beats and swells with the loudness. Whenever a sound starts (onset()),
-- little eighth notes burst out from the center.
--
-- Uses: beat(), bar(), tempo(), time_signature() for timing, level_left/right
-- and onset() for the audio, circle/polygon for shapes, sprites for the
-- notes, and text for the readout. For a plain audio file (no beats to read)
-- the ring just follows the onsets and loudness instead.

local pi = math.pi

-- An eighth note, 1 = ink, 2 = highlight.
local NOTE = sprite_register({
    palette = { { r = 255, g = 255, b = 255 }, { r = 255, g = 230, b = 150 } },
    image = {
        { 0, 0, 1, 1, 0, 0 },
        { 0, 0, 1, 2, 1, 0 },
        { 0, 0, 1, 0, 2, 1 },
        { 0, 0, 1, 0, 0, 1 },
        { 0, 0, 1, 0, 0, 0 },
        { 0, 0, 1, 0, 0, 0 },
        { 1, 1, 1, 0, 0, 0 },
        { 1, 2, 1, 0, 0, 0 },
        { 1, 1, 1, 0, 0, 0 },
    },
})

local sparks = {}        -- flying notes: {x, y, vx, vy, born}
local level = 0          -- smoothed loudness
local onset_glow = 0     -- flash after an onset, fades
local MAX_SPARKS = 48
local SPARK_LIFE = 1.1

local function mix(a, b, t)
    return { r = a.r + (b.r - a.r) * t, g = a.g + (b.g - a.g) * t, b = a.b + (b.b - a.b) * t, a = a.a }
end

-- Points of a regular polygon around (cx, cy).
local function ring_points(cx, cy, radius, sides, turn)
    local points = {}
    for i = 0, sides - 1 do
        local a = turn + i / sides * 2 * pi
        points[#points + 1] = cx + math.cos(a) * radius
        points[#points + 1] = cy + math.sin(a) * radius
    end
    return points
end

function render(width, height, left, right)
    local background = setting_color("background", { r = 12, g = 10, b = 22 })
    local accent = setting_color("accent", { r = 255, g = 120, b = 200 })
    local calm = setting_color("calm", { r = 70, g = 80, b = 160 })
    local show_text = setting_bool("show_readout", true)
    local note_size = setting_int("note_size", 3, 1, 8)

    clear(background)
    local cx, cy = width / 2, height / 2
    local radius = math.min(width, height) * 0.32

    -- Audio: smoothed loudness and onsets.
    local loud = (level_left() + level_right()) / 2
    level = level + (loud - level) * math.min(1, DT * 12)
    local hit, strength = onset()
    if hit then
        onset_glow = 1
        for _ = 1, math.min(6, 2 + math.floor(strength)) do
            if #sparks >= MAX_SPARKS then table.remove(sparks, 1) end
            local a = math.random() * 2 * pi
            local speed = radius * (0.8 + math.random() * 0.9)
            sparks[#sparks + 1] = { x = cx, y = cy, vx = math.cos(a) * speed, vy = math.sin(a) * speed, born = TIME }
        end
    end
    onset_glow = math.max(0, onset_glow - DT * 3)

    -- Timing, when the song has it.
    local b = beat()
    local beats_per_bar, into, bar_number = 4, nil, nil
    if b then
        local num, den = time_signature()
        beats_per_bar = math.max(1, math.floor(num * 4 / den + 0.5))
        bar_number, into = bar()
    end
    local phase = b and (1 - (b % 1)) or onset_glow -- 1 on the beat, fading

    -- Center: a polygon turning with the beats, swelling with loudness.
    local swell = 0.35 + math.min(1, level * 2.5) * 0.5
    local turn = (b or TIME * 0.5) * pi / 4
    polygon(ring_points(cx, cy, radius * swell, beats_per_bar + 2, turn), mix(calm, accent, phase * 0.6))
    circle(cx, cy, radius * swell * 0.45, mix(background, accent, 0.25 + onset_glow * 0.5))

    -- The ring: one dot per beat of the bar.
    local current = into and (math.floor(into) % beats_per_bar) or -1
    for i = 0, beats_per_bar - 1 do
        local a = -pi / 2 + i / beats_per_bar * 2 * pi
        local x, y = cx + math.cos(a) * radius, cy + math.sin(a) * radius
        local size = radius * (i == 0 and 0.13 or 0.09)
        if i == current then
            size = size * (1 + phase * 0.8)
            circle(x, y, size, accent)
        else
            circle(x, y, size * 0.8, mix(calm, background, 0.3))
        end
    end

    -- Flying notes: out from the center, shrinking as they go.
    for i = #sparks, 1, -1 do
        local s = sparks[i]
        local age = TIME - s.born
        if age > SPARK_LIFE then
            table.remove(sparks, i)
        else
            local k = 1 - age / SPARK_LIFE
            local scale = note_size * (0.4 + 0.6 * k)
            local w, h = sprite_size(NOTE)
            sprite(NOTE, s.x + s.vx * age - w * scale / 2, s.y + s.vy * age - h * scale / 2, { scale = scale, flip_x = s.vx < 0 })
        end
    end

    -- Readout.
    if show_text then
        local th = (height >= 500) and FONT_HEIGHT * 2 or FONT_HEIGHT
        local color = { r = 230, g = 230, b = 245, a = 0.85 }
        if b then
            local num, den = time_signature()
            text(th / 2, th / 2, string.format("%d bpm  %d/%d", math.floor(tempo() + 0.5), num, den), color, th)
            text(th / 2, th * 1.5, string.format("bar %d  beat %d", bar_number, math.floor(into) + 1), color, th)
        else
            text(th / 2, th / 2, "no beats in this song: following the sound", color, th)
        end
    end
end
