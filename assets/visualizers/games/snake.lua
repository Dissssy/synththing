-- Snake: one snake per MIDI channel on a classic checkerboard field.
--
-- Every new note a channel plays spawns an apple. The apple's column comes
-- from the note's pitch (lowest note in the song at the left edge, highest at
-- the right), and its row is random. Each snake hunts the apples spawned by
-- its OWN channel (nearest first, found with a BFS that routes around walls
-- and other snakes); if its channel has none, it goes for the nearest apple
-- of any channel, and if there are no apples at all it wanders.
--
-- Game time is derived from the audio itself (samples received / SAMPLE_RATE),
-- so the snakes freeze when playback pauses and stay in sync with the music.
-- Snakes tick faster when more apples are waiting, so they keep up with busy
-- passages.
--
-- Disabled channels: their snake vanishes and their apples are removed.
-- A snake that boxes itself in "crashes" in a burst of particles and
-- respawns -- and, if "disable_channel_on_death" is on (the default), mutes
-- its channel too, same as toggling it off in the Channels row. If the
-- crashing snake was the last one left with an enabled channel, that would
-- just silence everything, so instead every channel comes back on -- a
-- last-snake-standing game where a wipe starts the next round rather than
-- ending the music.
--
-- Fit: the cell size is an integer (pixel-crisp) chosen so the field is about
-- 40x22 cells, then cols/rows are whatever fills the window; leftover pixels
-- become an even border. The game restarts if the window size changes.
--
-- Play along: set "player_channel" (Script Settings) to a channel number and
-- that channel's snake is yours while the visualizer has focus (click it, or
-- use the Fullscreen visualizer): arrow keys or WASD steer, or whatever
-- keys you bind under Controls in Script Settings. It drives itself again
-- whenever the visualizer loses focus. Your best length is saved
-- (store_set), and the scoreboard (text) shows every snake's length.
-- Apples are sprites, one per snake color.

local floor, max, min = math.floor, math.max, math.min

-- Tuning ---------------------------------------------------------------
local COLS_TARGET = 40
local ROWS_TARGET = 22
local MIN_CELL = 6
local START_LEN = 4
local MAX_LEN = 24
local BASE_STEPS = 9      -- snake moves per second with no apples waiting
local MAX_STEPS = 26      -- ...and with a backlog of apples
local MAX_STEPS_PER_FRAME = 4
local RESPAWN_DELAY = 1.0 -- seconds
local POP_TIME = 0.18     -- apple spawn pop-in
local FLASH_TIME = 0.16   -- head flash after eating
local MAX_PARTICLES = 140
local MAX_SPAWN_PER_FRAME = 6

-- Colors ---------------------------------------------------------------
local FRAME_COL = { r = 87, g = 138, b = 52 }
local LIGHT = { r = 170, g = 215, b = 81 }
local DARK = { r = 162, g = 209, b = 73 }

local APPLE_RED = { r = 231, g = 53, b = 36 }
local APPLE_HI = { r = 255, g = 170, b = 160 }
local APPLE_STEM = { r = 115, g = 70, b = 40 }
local APPLE_LEAF = { r = 46, g = 125, b = 50 }
local EYE_WHITE = { r = 255, g = 255, b = 255 }
local EYE_PUPIL = { r = 20, g = 20, b = 30 }

local SNAKE_COLORS = {
    { 66,  115, 235 }, -- blue
    { 250, 150, 30 },  -- orange
    { 150, 80,  220 }, -- purple
    { 240, 100, 170 }, -- pink
    { 40,  200, 220 }, -- cyan
    { 250, 225, 60 },  -- yellow
    { 235, 235, 245 }, -- white
    { 40,  60,  140 }, -- navy
}

local function rgb(r, g, b)
    return { r = floor(r + 0.5), g = floor(g + 0.5), b = floor(b + 0.5) }
end

local function make_palette(index)
    local base = SNAKE_COLORS[(index - 1) % #SNAKE_COLORS + 1]
    -- Past the 8th snake, reuse the colors a bit darker so they stay distinct.
    local k = (index > #SNAKE_COLORS) and 0.72 or 1.0
    local r, g, b = base[1] * k, base[2] * k, base[3] * k
    return {
        body_a = rgb(r, g, b),
        body_b = rgb(r * 0.86, g * 0.86, b * 0.86),
        head = rgb(r + (255 - r) * 0.25, g + (255 - g) * 0.25, b + (255 - b) * 0.25),
        flash = rgb(r + (255 - r) * 0.75, g + (255 - g) * 0.75, b + (255 - b) * 0.75),
        apple_hi = rgb(r + (255 - r) * 0.55, g + (255 - g) * 0.55, b + (255 - b) * 0.55),
    }
end

-- Directions: 1 right, 2 down, 3 left, 4 up
local DX = { 1, 0, -1, 0 }
local DY = { 0, 1, 0, -1 }
local OPP = { 3, 4, 1, 2 }

-- State ----------------------------------------------------------------
local cols, rows, cell, ox, oy = 0, 0, 0, 0, 0
local cur_w, cur_h = -1, -1

local occ = {}      -- cell index -> snake occupying it
local apple_at = {} -- cell index -> apple
local seen = {}     -- BFS visit stamps
local stamp = 0
local Q = { x = {}, y = {}, f = {} }

local apples = {}    -- oldest first
local own_count = {} -- channel -> apples currently on the board
local snakes, snake_by_ch = {}, {}
local parts = {}

local game_time = 0.0
local step_acc = 0.0
local prev_held = {}
local lo, hi = nil, nil
-- Updated from the setting at the top of each render() call; read by
-- kill_snake, which isn't itself called from render() directly.
local disable_on_death = true

-- Player: the channel whose snake the keyboard steers (nil = nobody), turns
-- queued from key presses, and the best length ever reached (saved).
local player_ch = nil
local turn_queue = {}
local best = store_get("best_length") or 0
-- Steering actions, rebindable under Controls in Script Settings; the
-- value is the direction each one turns to.
local STEER = {
    [input_register("right", { "right", "d" })] = 1,
    [input_register("down", { "down", "s" })] = 2,
    [input_register("left", { "left", "a" })] = 3,
    [input_register("up", { "up", "w" })] = 4,
}

-- Apple sprite: 1 skin, 2 shine, 3 stem, 4 leaf. Registered once per snake
-- color (plus a default red one), recolored through the palette.
local APPLE_IMAGE = {
    { 0, 0, 0, 0, 3, 4, 4, 0 },
    { 0, 0, 0, 3, 4, 4, 0, 0 },
    { 0, 1, 1, 3, 1, 1, 0, 0 },
    { 1, 2, 1, 1, 1, 1, 1, 0 },
    { 1, 2, 1, 1, 1, 1, 1, 0 },
    { 1, 1, 1, 1, 1, 1, 1, 0 },
    { 0, 1, 1, 1, 1, 1, 0, 0 },
    { 0, 0, 1, 1, 1, 0, 0, 0 },
}

local function apple_sprite(skin, shine)
    return sprite_register({ image = APPLE_IMAGE, palette = { skin, shine, APPLE_STEM, APPLE_LEAF } })
end
local DEFAULT_APPLE = apple_sprite(APPLE_RED, APPLE_HI)

-- Helpers --------------------------------------------------------------
local function cell_index(x, y)
    return y * cols + x + 1
end

local function is_free(x, y)
    return x >= 0 and x < cols and y >= 0 and y < rows and not occ[y * cols + x + 1]
end

local function burst(px, py, n, c, speed)
    for _ = 1, n do
        if #parts >= MAX_PARTICLES then return end
        local ang = math.random() * 6.2832
        local sp = (0.4 + math.random()) * speed * cell
        parts[#parts + 1] = {
            x = px,
            y = py,
            vx = math.cos(ang) * sp,
            vy = math.sin(ang) * sp,
            born = game_time,
            life = 0.35 + math.random() * 0.25,
            size = cell * 0.22 * (0.6 + math.random() * 0.7),
            col = { r = c.r, g = c.g, b = c.b, a = 1 },
        }
    end
end

local function cell_center(x, y)
    return ox + x * cell + cell / 2, oy + y * cell + cell / 2
end

local function remove_apple(a)
    for k = 1, #apples do
        if apples[k] == a then
            table.remove(apples, k)
            break
        end
    end
    apple_at[cell_index(a.x, a.y)] = nil
end

local function purge_apples(ch)
    for k = #apples, 1, -1 do
        local a = apples[k]
        if a.ch == ch then
            apple_at[cell_index(a.x, a.y)] = nil
            table.remove(apples, k)
        end
    end
end

local function layout(width, height)
    if width == cur_w and height == cur_h then return end
    cur_w, cur_h = width, height

    local c = floor(min(width / COLS_TARGET, height / ROWS_TARGET))
    c = max(MIN_CELL, c)
    c = max(1, min(c, floor(width / 8), floor(height / 6)))

    cell = c
    cols = max(8, floor(width / c))
    rows = max(6, floor(height / c))
    ox = floor((width - cols * cell) / 2)
    oy = floor((height - rows * cell) / 2)

    occ, apple_at, seen, stamp = {}, {}, {}, 0
    apples, parts, own_count = {}, {}, {}
    for _, sn in ipairs(snakes) do
        sn.body = {}
        sn.alive = false
        sn.dead_until = 0
        sn.grow = 0
    end
end

-- Snakes ---------------------------------------------------------------
local function place_snake(sn)
    for _ = 1, 40 do
        local x = math.random(START_LEN - 1, cols - START_LEN)
        local y = math.random(0, rows - 1)
        local d = (x < cols / 2) and 1 or 3 -- face toward the middle
        local ok = true
        for k = 0, START_LEN - 1 do
            local i = cell_index(x - DX[d] * k, y)
            if occ[i] or apple_at[i] then
                ok = false
                break
            end
        end
        if ok and is_free(x + DX[d], y) then
            sn.body = {}
            for k = 0, START_LEN - 1 do
                local i = cell_index(x - DX[d] * k, y)
                sn.body[k + 1] = i
                occ[i] = sn
            end
            sn.x, sn.y, sn.dir = x, y, d
            sn.grow = 0
            sn.alive = true
            return
        end
    end
    sn.dead_until = game_time + 0.3 -- crowded; try again shortly
end

local function kill_snake(sn, with_burst)
    local body = sn.body
    for k = 1, #body do
        local i = body[k]
        occ[i] = nil
        if with_burst and k % 2 == 1 then
            local x, y = (i - 1) % cols, floor((i - 1) / cols)
            local px, py = cell_center(x, y)
            burst(px, py, 2, sn.pal.body_a, 3.5)
        end
    end
    sn.body = {}
    sn.alive = false
    sn.dead_until = game_time + RESPAWN_DELAY

    -- `with_burst` is only true for an actual crash (the channel-toggle loop
    -- below calls this with `false` just to clean up a snake whose channel
    -- got disabled some other way) -- so this only fires once per real death,
    -- never as a reaction to its own channel change.
    if with_burst and disable_on_death then
        local enabled = 0
        for _, ch in ipairs(midi_channels()) do
            if channel_enabled(ch) then enabled = enabled + 1 end
        end
        if enabled <= 1 then
            -- This was the last snake with an enabled channel -- muting it
            -- would just silence everything, so bring the whole cast back
            -- instead and start the next round.
            for _, ch in ipairs(midi_channels()) do
                set_channel_enabled(ch, true)
            end
        else
            set_channel_enabled(sn.ch, false)
        end
    end
end

-- BFS from the head. Returns the first move of a shortest path to an apple
-- (own channel preferred), or nil if none is reachable.
local function plan(sn)
    stamp = stamp + 1
    local qx, qy, qf = Q.x, Q.y, Q.f
    local qh, qt = 1, 0
    local any_dir = nil
    local want_any = (own_count[sn.ch] or 0) == 0
    local reverse = OPP[sn.dir]

    for d = 1, 4 do
        if d ~= reverse then
            local nx, ny = sn.x + DX[d], sn.y + DY[d]
            if is_free(nx, ny) then
                local i = cell_index(nx, ny)
                if seen[i] ~= stamp then
                    seen[i] = stamp
                    qt = qt + 1
                    qx[qt], qy[qt], qf[qt] = nx, ny, d
                end
            end
        end
    end

    while qh <= qt do
        local x, y, f = qx[qh], qy[qh], qf[qh]
        qh = qh + 1

        local a = apple_at[cell_index(x, y)]
        if a then
            if want_any or a.ch == sn.ch then return f end
            if not any_dir then any_dir = f end
        end

        for d = 1, 4 do
            local nx, ny = x + DX[d], y + DY[d]
            if is_free(nx, ny) then
                local i = cell_index(nx, ny)
                if seen[i] ~= stamp then
                    seen[i] = stamp
                    qt = qt + 1
                    qx[qt], qy[qt], qf[qt] = nx, ny, f
                end
            end
        end
    end

    return any_dir
end

-- No apple to chase: mostly go straight, sometimes turn. nil = boxed in.
local function wander(sn)
    local reverse = OPP[sn.dir]
    if is_free(sn.x + DX[sn.dir], sn.y + DY[sn.dir]) and math.random() > 0.15 then
        return sn.dir
    end
    local n, pick = 0, nil
    for d = 1, 4 do
        if d ~= reverse and is_free(sn.x + DX[d], sn.y + DY[d]) then
            n = n + 1
            if math.random(n) == 1 then pick = d end
        end
    end
    return pick
end

-- The player's move: the next queued turn (a U-turn is ignored), else
-- straight on. nil if that runs into something.
local function player_move(sn)
    local want = table.remove(turn_queue, 1) or sn.dir
    if want == OPP[sn.dir] then want = sn.dir end
    if is_free(sn.x + DX[want], sn.y + DY[want]) then return want end
    return nil
end

local function step_snake(sn)
    if not sn.alive then return end

    local d
    if sn.ch == player_ch and has_focus() then
        d = player_move(sn)
    else
        d = plan(sn) or wander(sn)
    end
    if not d then
        kill_snake(sn, true) -- crashed
        return
    end

    local nx, ny = sn.x + DX[d], sn.y + DY[d]
    local ni = cell_index(nx, ny)
    sn.x, sn.y, sn.dir = nx, ny, d

    local a = apple_at[ni]
    table.insert(sn.body, 1, ni)
    occ[ni] = sn

    if a then
        remove_apple(a)
        own_count[a.ch] = max(0, (own_count[a.ch] or 1) - 1)
        if #sn.body + sn.grow < MAX_LEN then sn.grow = sn.grow + 1 end
        if sn.ch == player_ch and #sn.body + sn.grow > best then
            best = #sn.body + sn.grow
            store_set("best_length", best)
        end
        sn.flash = game_time
        local px, py = cell_center(nx, ny)
        local owner = snake_by_ch[a.ch]
        burst(px, py, 4, owner and owner.pal.body_a or APPLE_RED, 3.0)
        burst(px, py, 3, sn.pal.body_a, 2.5)
    end

    if sn.grow > 0 then
        sn.grow = sn.grow - 1
    else
        local tail = table.remove(sn.body)
        occ[tail] = nil
    end
end

-- Apples ---------------------------------------------------------------
local function column_for(key)
    local l, h = lo or 48, hi or 84
    if h - l < 12 then
        local mid = (h + l) / 2
        l, h = mid - 6, mid + 6
    end
    local t = max(0, min(1, (key - l) / (h - l)))
    return floor(t * (cols - 1) + 0.5)
end

local function spawn_apple(ch, key)
    local base_x = column_for(key)
    for attempt = 1, 10 do
        local ax = base_x
        if attempt > 3 then ax = max(0, min(cols - 1, base_x + math.random(-2, 2))) end
        local ay = math.random(0, rows - 1)
        local i = cell_index(ax, ay)
        if not occ[i] and not apple_at[i] then
            local a = { x = ax, y = ay, ch = ch, born = game_time }
            apples[#apples + 1] = a
            apple_at[i] = a
            own_count[ch] = (own_count[ch] or 0) + 1

            local cap = max(10, floor(cols * rows / 40))
            if #apples > cap then
                local old = table.remove(apples, 1)
                apple_at[cell_index(old.x, old.y)] = nil
                own_count[old.ch] = max(0, (own_count[old.ch] or 1) - 1)
            end
            return
        end
    end
end

-- Drawing --------------------------------------------------------------
local function draw_apple(a)
    local age = game_time - a.born
    local s = 1.0
    if age < POP_TIME then
        local k = max(0, age / POP_TIME)
        s = 0.3 + 0.7 * (1 - (1 - k) * (1 - k))
    end
    local owner = snake_by_ch[a.ch]
    local size = cell * s
    local cx, cy = cell_center(a.x, a.y)
    sprite(owner and owner.apple or DEFAULT_APPLE, cx - size / 2, cy - size / 2, size / 8)
end

local function draw_snake(sn)
    local body = sn.body
    local n = #body
    if n == 0 then return end
    local pal = sn.pal
    local g = max(1, floor(cell * 0.1))

    -- Tail to head: each rect spans a segment and its neighbor, so the body
    -- reads as one continuous tube with a small gap around the edge.
    for i = n, 2, -1 do
        local a, b = body[i], body[i - 1]
        local ax, ay = (a - 1) % cols, floor((a - 1) / cols)
        local bx, by = (b - 1) % cols, floor((b - 1) / cols)
        rect(ox + min(ax, bx) * cell + g,
            oy + min(ay, by) * cell + g,
            ox + (max(ax, bx) + 1) * cell - g,
            oy + (max(ay, by) + 1) * cell - g,
            (i % 2 == 0) and pal.body_a or pal.body_b)
    end

    -- Head
    local hx, hy = ox + sn.x * cell, oy + sn.y * cell
    local hg = max(0, g - 1)
    local flashing = (game_time - sn.flash) < FLASH_TIME
    rect(hx + hg, hy + hg, hx + cell - hg, hy + cell - hg, flashing and pal.flash or pal.head)

    -- Eyes: two squares on either side of the facing direction, pupils
    -- nudged forward.
    local fx, fy = DX[sn.dir], DY[sn.dir]
    local px, py = -fy, fx
    local cx, cy = hx + cell / 2, hy + cell / 2
    local e = max(2, floor(cell * 0.24))
    local p = max(1, floor(e * 0.5))
    for side = -1, 1, 2 do
        local ex = cx + fx * cell * 0.12 + px * side * cell * 0.22
        local ey = cy + fy * cell * 0.12 + py * side * cell * 0.22
        local wx, wy = floor(ex - e / 2), floor(ey - e / 2)
        rect(wx, wy, wx + e, wy + e, EYE_WHITE)
        local qx = floor(ex + fx * e * 0.2 - p / 2)
        local qy = floor(ey + fy * e * 0.2 - p / 2)
        rect(qx, qy, qx + p, qy + p, EYE_PUPIL)
    end
end

local function draw_particles()
    for i = #parts, 1, -1 do
        local pt = parts[i]
        local age = game_time - pt.born
        if age < 0 or age >= pt.life then
            parts[i] = parts[#parts]
            parts[#parts] = nil
        else
            local k = age / pt.life
            local s = max(1, floor(pt.size * (1 - 0.6 * k)))
            local x = floor(pt.x + pt.vx * age - s / 2)
            local y = floor(pt.y + pt.vy * age - s / 2)
            pt.col.a = 1 - k
            rect(x, y, x + s, y + s, pt.col)
        end
    end
end

-- Scoreboard: a chip per snake with its length, in the top-left corner.
local function draw_scores(width, height)
    local th = (height >= 600) and FONT_HEIGHT * 2 or FONT_HEIGHT
    local pad = floor(th / 4)
    local x = ox + pad
    local y = oy + pad
    for _, sn in ipairs(snakes) do
        if not sn.disabled then
            local label = tostring(#sn.body)
            if sn.ch == player_ch then label = "you " .. label end
            local tw = text_size(label, th)
            rect(x, y, x + th + pad + tw + pad * 2, y + th + pad, { r = 0, g = 0, b = 0, a = 0.35 })
            rect(x + pad, y + pad / 2, x + pad + th - pad, y + th + pad / 2 - pad, sn.pal.head)
            text(x + th + pad, y + pad / 2, label, { r = 255, g = 255, b = 255 }, th)
            x = x + th + pad + tw + pad * 3
        end
    end
    if player_ch then
        local line = has_focus() and ("best " .. best .. "  (arrows / WASD)")
            or ("click to steer channel " .. (player_ch + 1))
        local tw = text_size(line, th)
        local lx, ly = ox + pad, oy + rows * cell - th - pad
        rect(lx, ly - pad / 2, lx + tw + pad * 2, ly + th + pad / 2, { r = 0, g = 0, b = 0, a = 0.35 })
        text(lx + pad, ly, line, { r = 255, g = 255, b = 255 }, th)
    end
end

local function draw_board(width, height, frame_col, light, dark)
    clear(frame_col)
    rect(ox, oy, ox + cols * cell, oy + rows * cell, light)
    for y = 0, rows - 1 do
        local py = oy + y * cell
        for x = (y % 2 == 0) and 1 or 0, cols - 1, 2 do
            local px = ox + x * cell
            rect(px, py, px + cell, py + cell, dark)
        end
    end
end

-- Frame ----------------------------------------------------------------
function render(width, height, left, right)
    local frame_col = setting_color("frame", FRAME_COL)
    local light = setting_color("light", LIGHT)
    local dark = setting_color("dark", DARK)
    local base_steps = setting_int("base_steps", BASE_STEPS, 1, 30)
    local max_steps = setting_int("max_steps", MAX_STEPS, 1, 60)
    disable_on_death = setting_bool("disable_channel_on_death", true)
    local player = setting_int("player_channel", 0, 0, 16) -- 0 = nobody
    local show_scores = setting_bool("show_scores", true)
    player_ch = (player > 0) and (player - 1) or nil

    -- Steering keys (only arrive while the visualizer has focus). Keep at
    -- most two turns queued, so quick taps still all count.
    if player_ch then
        for action, d in pairs(STEER) do
            if input(action) == "pressed" and #turn_queue < 2 then
                turn_queue[#turn_queue + 1] = d
            end
        end
        if not has_focus() then turn_queue = {} end
    end

    layout(width, height)

    -- One snake per channel, created the first time a channel shows up.
    for i, ch in ipairs(midi_channels()) do
        if not snake_by_ch[ch] then
            local sn = {
                ch = ch,
                pal = make_palette(i),
                body = {},
                x = 0,
                y = 0,
                dir = 1,
                alive = false,
                grow = 0,
                dead_until = 0,
                flash = -10,
                disabled = false,
            }
            sn.apple = apple_sprite(sn.pal.body_a, sn.pal.apple_hi)
            snakes[#snakes + 1] = sn
            snake_by_ch[ch] = sn
        end
    end

    -- Audio clock
    local dt = 0
    if SAMPLE_RATE and SAMPLE_RATE > 0 then
        dt = min(0.1, #left / SAMPLE_RATE)
    end
    game_time = game_time + dt

    -- Pitch range of the song (widens as new extremes are seen)
    local active = active_notes()
    for _, n in ipairs(upcoming_notes()) do
        if n.on and n.key then
            if lo == nil or n.key < lo then lo = n.key end
            if hi == nil or n.key > hi then hi = n.key end
        end
    end
    for _, n in ipairs(active) do
        if n.key then
            if lo == nil or n.key < lo then lo = n.key end
            if hi == nil or n.key > hi then hi = n.key end
        end
    end

    -- Channel toggles and respawns
    for _, sn in ipairs(snakes) do
        if not channel_enabled(sn.ch) then
            if not sn.disabled then
                sn.disabled = true
                if sn.alive then kill_snake(sn, false) end
                purge_apples(sn.ch)
                own_count[sn.ch] = 0
            end
        else
            if sn.disabled then
                sn.disabled = false
                sn.dead_until = 0
            end
            if not sn.alive and game_time >= sn.dead_until then
                place_snake(sn)
            end
        end
    end

    -- New note-ons (keys held now that weren't held last frame) -> apples
    local cur = {}
    local spawned = 0
    for _, note in ipairs(active) do
        local ch, key = note.channel, note.key
        if key then
            local row = cur[ch]
            if not row then
                row = {}
                cur[ch] = row
            end
            row[key] = true

            local p = prev_held[ch]
            if not (p and p[key]) and spawned < MAX_SPAWN_PER_FRAME then
                local sn = snake_by_ch[ch]
                if sn and not sn.disabled then
                    spawn_apple(ch, key)
                    spawned = spawned + 1
                end
            end
        end
    end
    prev_held = cur

    -- Tick the snakes (faster when apples are piling up)
    local rate = min(max_steps, base_steps + #apples * 1.2)
    step_acc = step_acc + dt * rate
    local steps = floor(step_acc)
    step_acc = step_acc - steps
    if steps > MAX_STEPS_PER_FRAME then steps = MAX_STEPS_PER_FRAME end
    for _ = 1, steps do
        for _, sn in ipairs(snakes) do
            step_snake(sn)
        end
    end

    -- Draw
    draw_board(width, height, frame_col, light, dark)
    for k = 1, #apples do
        draw_apple(apples[k])
    end
    for _, sn in ipairs(snakes) do
        if sn.alive then draw_snake(sn) end
    end
    draw_particles()
    if show_scores then draw_scores(width, height) end
end
