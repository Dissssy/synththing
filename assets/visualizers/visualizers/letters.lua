-- Channel Letters
-- One big letter per MIDI channel. The channel's current pitch maps to
-- A (lowest note in the song) .. Z (highest note in the song), colored
-- along a spectrum from blue (low) to red (high).
-- Channels with no sounding note (or toggled off) show a dash.

-- 5x7 bitmap font
local FONT = {
    A = { ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#" },
    B = { "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####." },
    C = { ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###." },
    D = { "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####." },
    E = { "#####", "#....", "#....", "####.", "#....", "#....", "#####" },
    F = { "#####", "#....", "#....", "####.", "#....", "#....", "#...." },
    G = { ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###." },
    H = { "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#" },
    I = { "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####" },
    J = { "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.." },
    K = { "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#" },
    L = { "#....", "#....", "#....", "#....", "#....", "#....", "#####" },
    M = { "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#" },
    N = { "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#" },
    O = { ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###." },
    P = { "####.", "#...#", "#...#", "####.", "#....", "#....", "#...." },
    Q = { ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#" },
    R = { "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#" },
    S = { ".####", "#....", "#....", ".###.", "....#", "....#", "####." },
    T = { "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.." },
    U = { "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###." },
    V = { "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.." },
    W = { "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "#.#.#", ".#.#." },
    X = { "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#" },
    Y = { "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.." },
    Z = { "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####" },
    ["-"] = { ".....", ".....", ".....", "#####", ".....", ".....", "....." },
}

-- Running pitch range of the song (grows as new extremes are seen)
local lo, hi = nil, nil

local function pitch_of(n)
    return n.note or n.pitch or n.key
end

local function track_range(notes)
    for _, n in ipairs(notes) do
        local p = pitch_of(n)
        if p then
            if lo == nil or p < lo then lo = p end
            if hi == nil or p > hi then hi = p end
        end
    end
end

-- Pick the grid (cols x rows) that makes the letters as big as possible
local function best_grid(n, w, h)
    local best_c, best_s = 1, 0
    for c = 1, n do
        local r = math.ceil(n / c)
        local s = math.min(w / c / 6, h / r / 8) -- 5x7 glyph + 1 cell spacing
        if s > best_s then best_c, best_s = c, s end
    end
    return best_c, math.ceil(n / best_c), best_s
end

local function draw_glyph(glyph, x, y, s, col)
    local size = math.ceil(s)
    for row = 1, 7 do
        local bits = glyph[row]
        for c = 1, 5 do
            if bits:sub(c, c) == "#" then
                local px = math.floor(x + (c - 1) * s)
                local py = math.floor(y + (row - 1) * s)
                rect(px, py, px + size, py + size, col)
            end
        end
    end
end

function render(width, height, left, right)
    local background = setting_color("background", { r = 16, g = 16, b = 24 })
    local idle_color = setting_color("idle_color", { r = 90, g = 90, b = 110 })
    local saturation = setting_float("saturation", 0.85, 0.0, 1.0)

    clear(background)

    local channels = midi_channels()
    local n = #channels
    if n == 0 then return end

    local active = active_notes()
    track_range(active)
    track_range(upcoming_notes())

    -- Highest sounding pitch per channel
    local top = {}
    for _, note in ipairs(active) do
        local p, ch = pitch_of(note), note.channel
        if p and ch and (top[ch] == nil or p > top[ch]) then
            top[ch] = p
        end
    end

    local cols, rows, s = best_grid(n, width, height)
    local cell_w, cell_h = width / cols, height / rows
    local gw, gh = 5 * s, 7 * s

    for i, ch in ipairs(channels) do
        local cx = ((i - 1) % cols) * cell_w
        local cy = math.floor((i - 1) / cols) * cell_h
        local x = cx + (cell_w - gw) / 2
        local y = cy + (cell_h - gh) / 2

        local p = channel_enabled(ch) and top[ch] or nil
        local glyph, col
        if p then
            local t = (hi > lo) and (p - lo) / (hi - lo) or 0.5
            local idx = math.floor(t * 25 + 0.5)              -- 0..25
            glyph = FONT[string.char(65 + idx)]               -- 'A'..'Z'
            col = hsv(240 * (1 - idx / 25), saturation, 1.0)  -- blue -> red
        else
            glyph = FONT["-"]
            col = idle_color
        end
        draw_glyph(glyph, x, y, s, col)
    end
end
