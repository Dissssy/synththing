-- highway.lua: a Guitar Hero style game charted on the fly from any track of the song.
--
-- Songs load paused while this runs. Every track is previewed side by side, charted the
-- way you'd play it; pick one, a difficulty and a practice speed, then press Enter, click
-- PLAY, or just hit play in the app. The song waits until the first notes have had time
-- to scroll all the way down the highway. N and B change song within the playlist.
--
-- Charting: notes that start together become chords. A note's lane comes from its pitch
-- relative to the notes around it (a few seconds either side), then the melodic contour is
-- enforced on top: higher pitch moves right, lower moves left, a repeated pitch stays in
-- its lane, and big leaps jump further. Chords hang down from their top note. Long notes
-- become sustains. Drums go by kit piece instead (kick, snare, hats, toms, crashes).
-- Optionally (mute_on_miss_experimental setting), missing a note mutes the track until you hit again.

script_options({ start_paused = true })

local LANE_COLORS = {
    { r = 60,  g = 220, b = 90 },
    { r = 235, g = 60,  b = 60 },
    { r = 245, g = 215, b = 50 },
    { r = 60,  g = 140, b = 255 },
    { r = 255, g = 140, b = 30 },
}
local LANE_KEYS = { { "d", "1" }, { "f", "2" }, { "j", "3" }, { "k", "4" }, { "l", "5" } }
local LANE_INPUT = {}
for i = 1, 5 do
    LANE_INPUT[i] = input_register("lane " .. i, LANE_KEYS[i])
end
local START = input_register("start", { "enter" })
local PAUSE = input_register("pause", { "space" })
local BACK = input_register("back to menu", { "backspace" })
local PREV = input_register("previous track", { "left" })
local NEXT = input_register("next track", { "right" })
local HARDER = input_register("harder", { "up" })
local EASIER = input_register("easier", { "down" })
local SPEED = input_register("practice speed", { "s" })
local NEXT_SONG = input_register("next song", { "n" })
local PREV_SONG = input_register("previous song", { "b" })

-- gap: least time between chords (song seconds), chord: most notes in one chord
local DIFFS = {
    { name = "EASY",   gap = 0.30, chord = 1 },
    { name = "MEDIUM", gap = 0.18, chord = 2 },
    { name = "HARD",   gap = 0.10, chord = 2 },
    { name = "EXPERT", gap = 0.0,  chord = 3 },
}

local WIN_PERFECT, WIN_GOOD = 0.05, 0.10 -- real seconds either side of a note
local PREVIEW_SECONDS = 4
local SPEEDS = { 1, 0.75, 0.5 }

local state = "init" -- menu, preroll, starting, play, results
local song_key = false
local last_loads = nil
local speed_idx = 1
local hw_secs_cur = 1.6
local borrowed = nil -- the user's speed and loop mode, while a game has changed them
local tracks = {}    -- { ch, notes, nps }
local charts = {}    -- cache, "ch:diff:lanes" -> chart
local sel_track, sel_diff = 1, 2
local menu_seen_paused = false
local preview_t = 0
local game_time = 0
local chart, play_track = nil, nil
local first_live = 1
local holding = {}
local stats = {}
local last_gen, last_pos = nil, 0
local last_focus = false
local muted_ch = nil
local flash = {}
local particles = {}
local popup = nil
local result_best = 0
local new_best = false

-- helpers -------------------------------------------------------------------

local function fmt(n)
    local s = tostring(math.floor(n))
    local out = s:reverse():gsub("(%d%d%d)", "%1,"):reverse()
    return (out:gsub("^,", ""))
end

local function with_a(c, a)
    return { r = c.r, g = c.g, b = c.b, a = a }
end

local function ring(x, y, radius, col, segs)
    segs = segs or 32
    local px, py = x + radius, y
    for i = 1, segs do
        local a = i / segs * math.pi * 2
        local nx, ny = x + math.cos(a) * radius, y + math.sin(a) * radius
        line(px, py, nx, ny, col)
        px, py = nx, ny
    end
end

local function centered(str, cx, y, col, h)
    local w = text_size(str, h)
    text(cx - w / 2, y, str, col, h)
end

local function first_at(gems, t)
    local lo, hi = 1, #gems + 1
    while lo < hi do
        local mid = math.floor((lo + hi) / 2)
        if gems[mid].t < t then
            lo = mid + 1
        else
            hi = mid
        end
    end
    return lo
end

local function set_muted(ch)
    if muted_ch == ch then return end
    if muted_ch then set_channel_enabled(muted_ch, true) end
    if ch then set_channel_enabled(ch, false) end
    muted_ch = ch
    store_set("muted", ch) -- remembered so a restart mid-miss can unmute it
end

-- A game sets its own speed and turns looping off (so the song ends on the results screen
-- instead of moving on); these put the user's settings back afterwards, even after a restart.
local function borrow_playback(speed)
    local p = playback()
    if not borrowed then
        borrowed = { speed = p.speed or 1, loop = p.loop_mode or "off" }
        store_set("borrowed", borrowed)
    end
    if p.loop_mode ~= "off" then set_loop("off") end
    if p.speed ~= speed then set_speed(speed) end
end

local function give_back_playback()
    if not borrowed then return end
    set_speed(borrowed.speed)
    set_loop(borrowed.loop)
    borrowed = nil
    store_set("borrowed", nil)
end

local function track_name(t)
    return t.ch == 9 and "DRUMS" or ("CH " .. (t.ch + 1))
end

-- analysis and charting -----------------------------------------------------

local function analyze(p)
    tracks, charts = {}, {}
    if not p.song_id or not p.length then return end
    local by_ch = {}
    for _, n in ipairs(notes_between(0, p.length)) do
        local t = by_ch[n.channel]
        if not t then
            t = { ch = n.channel, notes = {} }
            by_ch[n.channel] = t
        end
        t.notes[#t.notes + 1] = n
    end
    for ch = 0, 15 do
        local t = by_ch[ch]
        if t then
            local span = math.max(1, t.notes[#t.notes].start - t.notes[1].start)
            t.nps = #t.notes / span
            tracks[#tracks + 1] = t
        end
    end
    sel_track = math.max(1, math.min(sel_track, #tracks))
end

local function drum_lane(key, lanes)
    local cat
    if key == 35 or key == 36 then
        cat = 1 -- kick
    elseif key >= 37 and key <= 40 then
        cat = 2 -- snare, rim, clap
    elseif key == 42 or key == 44 or key == 46 or key == 51 or key == 53 or key == 59 then
        cat = 3 -- hats, rides
    elseif key == 49 or key == 52 or key == 55 or key == 57 then
        cat = 5 -- crashes
    else
        cat = 4 -- toms, percussion
    end
    return math.min(cat, lanes) - 1
end

local function build_chart(track, diff, lanes)
    local d = DIFFS[diff]
    local notes = track.notes
    local drums = track.ch == 9

    -- 1. notes that start together become one chord
    local groups = {}
    local i = 1
    while i <= #notes do
        local g = { t = notes[i].start, notes = {}, stop = 0 }
        while i <= #notes and notes[i].start - g.t <= 0.03 do
            g.notes[#g.notes + 1] = notes[i]
            g.stop = math.max(g.stop, notes[i].stop)
            i = i + 1
        end
        groups[#groups + 1] = g
    end

    -- 2. thin out for the difficulty
    local kept = {}
    local last_t = -math.huge
    for _, g in ipairs(groups) do
        if g.t - last_t >= d.gap then
            kept[#kept + 1] = g
            last_t = g.t
            table.sort(g.notes, function(a, b) return a.key < b.key end)
            g.top = g.notes[#g.notes].key
        end
    end

    -- 3. lanes
    local gems = {}
    local prev_top, prev_lane, prev_t = nil, nil, -math.huge
    local lo_i, hi_i = 1, 1
    for gi, g in ipairs(kept) do
        local next_t = kept[gi + 1] and kept[gi + 1].t or math.huge
        local glanes = {}

        if drums then
            local used = {}
            for _, n in ipairs(g.notes) do
                local l = drum_lane(n.key, lanes)
                if not used[l] and #glanes < d.chord then
                    used[l] = true
                    glanes[#glanes + 1] = l
                end
            end
        else
            -- pitch range of the top notes within 3 seconds either side
            while kept[lo_i].t < g.t - 3 do lo_i = lo_i + 1 end
            while kept[hi_i + 1] and kept[hi_i + 1].t <= g.t + 3 do hi_i = hi_i + 1 end
            local lo, hi = 127, 0
            for j = lo_i, hi_i do
                lo = math.min(lo, kept[j].top)
                hi = math.max(hi, kept[j].top)
            end
            local lane = hi > lo and math.floor((g.top - lo) / (hi - lo + 1) * lanes) or math.floor(lanes / 2)

            -- the contour wins over the range, unless it's a fresh phrase
            if prev_top and g.t - prev_t < 1.5 then
                local iv = g.top - prev_top
                local mag = math.abs(iv)
                if iv == 0 then
                    lane = prev_lane
                else
                    local dir = iv > 0 and 1 or -1
                    local step_min = mag >= 7 and 2 or 1
                    local step_max = mag <= 2 and 1 or (mag <= 6 and 2 or lanes)
                    local moved = (lane - prev_lane) * dir
                    moved = math.max(step_min, math.min(step_max, moved))
                    lane = prev_lane + moved * dir
                end
            end
            lane = math.max(0, math.min(lanes - 1, lane))

            -- chord: distinct pitches, top note always kept, hanging down from its lane
            local keys = {}
            for _, n in ipairs(g.notes) do
                if keys[#keys] ~= n.key then keys[#keys + 1] = n.key end
            end
            local k = math.min(#keys, d.chord, lanes)
            local offsets = {}
            if k == 2 and keys[#keys] - keys[1] >= 7 and lanes >= 3 then
                offsets = { 0, 2 } -- wide interval, wide shape
            else
                for o = 0, k - 1 do offsets[#offsets + 1] = o end
            end
            local span = offsets[#offsets]
            local start = math.max(0, math.min(lanes - 1 - span, lane - span))
            for _, o in ipairs(offsets) do
                glanes[#glanes + 1] = start + o
            end
            prev_top, prev_lane, prev_t = g.top, start + span, g.t
        end

        local tail = nil
        if not drums then
            local tail_end = math.min(g.stop, next_t - 0.08)
            if tail_end - g.t >= 0.4 then tail = tail_end end
        end
        table.sort(glanes)
        for _, l in ipairs(glanes) do
            gems[#gems + 1] = { t = g.t, lane = l, tail = tail }
        end
    end
    return { gems = gems, lanes = lanes, ch = track.ch }
end

local function get_chart(track, diff, lanes)
    local key = track.ch .. ":" .. diff .. ":" .. lanes
    if not charts[key] then charts[key] = build_chart(track, diff, lanes) end
    return charts[key]
end

local function best_key(ch, diff, speed)
    local key = "best:" .. tostring(song_key) .. ":" .. ch .. ":" .. diff
    if speed ~= 1 then key = key .. ":" .. math.floor(speed * 100) end -- practice runs keep their own
    return key
end

-- state changes -------------------------------------------------------------

-- rewind: false right after a song loads, since it's already paused at the start
local function enter_menu(rewind)
    set_muted(nil)
    give_back_playback()
    if rewind and #tracks > 0 then
        set_paused(true)
        seek(0)
    end
    state = "menu"
    menu_seen_paused = false
    chart = nil
    local t = tracks[sel_track]
    preview_t = t and t.notes[1].start - 0.5 or 0
end

local function start_game()
    local pspeed = SPEEDS[speed_idx]
    local look = hw_secs_cur * pspeed
    local tr = tracks[sel_track]
    if not tr then return end
    play_track = tr
    chart = get_chart(tr, sel_diff, setting_int("lanes", 4, 3, 5))
    for _, g in ipairs(chart.gems) do
        g.state, g.holding = nil, false
    end
    stats = { score = 0, combo = 0, max_combo = 0, perfect = 0, good = 0, miss = 0 }
    first_live, holding, particles, popup = 1, {}, {}, nil
    set_muted(nil)
    borrow_playback(pspeed)
    local p = playback()
    set_paused(true)
    if p.position > 0 or p.finished then seek(0) end
    -- start the clock early enough for the first note to travel the whole highway
    local first = chart.gems[1] and chart.gems[1].t or 0
    game_time = math.min(-pspeed, first - look - 0.5 * pspeed)
    state = "preroll"
end

local function finish()
    set_paused(true)
    set_muted(nil)
    give_back_playback()
    local key = best_key(play_track.ch, sel_diff, SPEEDS[speed_idx])
    local prev = store_get(key) or 0
    new_best = stats.score > prev
    if new_best then store_set(key, stats.score) end
    result_best = math.max(prev, stats.score)
    state = "results"
end

-- game geometry -------------------------------------------------------------

local K = 2.2 -- perspective strength
local function s_of(z) return 1 / (1 + K * z) end
local G1 = 1 - s_of(1)

local function game_layout(width, height, lanes)
    local L = { lanes = lanes, cx = width / 2, hit_y = height * 0.86, top_y = height * 0.07 }
    L.range = L.hit_y - L.top_y
    L.bw = math.min(width * 0.62, L.range * 1.15)
    -- the z where the highway meets the bottom of the screen
    local s = 1 + (height - L.hit_y) * G1 / L.range
    L.zb = (1 / s - 1) / K
    return L
end
local function y_of(L, z) return L.hit_y - (1 - s_of(z)) / G1 * L.range end
local function x_of(L, lanepos, z) return L.cx + (lanepos / L.lanes - 0.5) * L.bw * s_of(z) end

local function sparks(x, y, col, n, size)
    for _ = 1, n do
        if #particles > 400 then return end
        local a = -math.pi / 2 + (math.random() - 0.5) * 2.2
        local sp = size * (8 + math.random() * 18)
        local life = 0.25 + math.random() * 0.35
        particles[#particles + 1] = {
            x = x,
            y = y,
            vx = math.cos(a) * sp,
            vy = math.sin(a) * sp,
            life = life,
            max = life,
            col = col,
            size = size * (0.5 + math.random()),
        }
    end
end

-- gameplay ------------------------------------------------------------------

local function hit(g, nowj, pspeed, L)
    local err = math.abs(g.t - nowj) / pspeed
    local perfect = err <= WIN_PERFECT
    g.state = "hit"
    stats.combo = stats.combo + 1
    stats.max_combo = math.max(stats.max_combo, stats.combo)
    local mult = math.min(4, 1 + math.floor(stats.combo / 10))
    stats.score = stats.score + (perfect and 100 or 60) * mult
    if perfect then stats.perfect = stats.perfect + 1 else stats.good = stats.good + 1 end
    if g.tail then
        g.holding = true
        holding[#holding + 1] = g
    end
    flash[g.lane] = 1
    local lw = L.bw / L.lanes
    sparks(x_of(L, g.lane + 0.5, 0), L.hit_y, LANE_COLORS[g.lane + 1], 10, lw * 0.05)
    popup = { text = perfect and "PERFECT" or "GOOD", life = 0.5, col = perfect and { r = 255, g = 230, b = 120 } or
    { r = 180, g = 220, b = 255 } }
    set_muted(nil)
end

local function miss(g, mute)
    g.state = "miss"
    stats.combo = 0
    stats.miss = stats.miss + 1
    popup = { text = "MISS", life = 0.5, col = { r = 255, g = 90, b = 90 } }
    if mute then set_muted(play_track.ch) end
end

local function update_play(p, pspeed, L, offset, mute_on_miss, ghost_penalty)
    local gems = chart.gems
    local nowj = game_time - offset * pspeed
    local win = WIN_GOOD * pspeed

    for lane = 0, chart.lanes - 1 do
        local st = input(LANE_INPUT[lane + 1])
        if st == "pressed" and not p.paused then
            local target = nil
            for i = first_live, #gems do
                local g = gems[i]
                if g.t > nowj + win then break end
                if not g.state and g.lane == lane and math.abs(g.t - nowj) <= win then
                    target = g
                    break
                end
            end
            if target then
                hit(target, nowj, pspeed, L)
            elseif ghost_penalty then
                stats.combo = 0
            end
        end
    end

    -- notes that went past unplayed
    while first_live <= #gems do
        local g = gems[first_live]
        if g.state then
            first_live = first_live + 1
        elseif g.t < nowj - win then
            miss(g, mute_on_miss)
            first_live = first_live + 1
        else
            break
        end
    end

    -- sustains score while held
    for i = #holding, 1, -1 do
        local g = holding[i]
        if not input_down(LANE_INPUT[g.lane + 1]) or game_time >= g.tail then
            g.holding = false
            table.remove(holding, i)
        elseif not p.paused then
            local mult = math.min(4, 1 + math.floor(stats.combo / 10))
            stats.score = stats.score + DT * 40 * mult
            flash[g.lane] = math.max(flash[g.lane] or 0, 0.5)
            if math.random() < 0.3 then
                sparks(x_of(L, g.lane + 0.5, 0), L.hit_y, LANE_COLORS[g.lane + 1], 1, L.bw / L.lanes * 0.04)
            end
        end
    end
end

-- drawing -------------------------------------------------------------------

local function draw_game(p, L, look, th, width, height)
    local lanes = chart.lanes
    local lw = L.bw / lanes
    local zt = 1

    -- road
    polygon({
        x_of(L, 0, zt), y_of(L, zt), x_of(L, lanes, zt), y_of(L, zt),
        x_of(L, lanes, L.zb), height, x_of(L, 0, L.zb), height,
    }, { r = 18, g = 18, b = 28 })

    -- beat and bar lines
    local b0 = beat(math.max(0, game_time))
    if b0 then
        local b = math.ceil(b0)
        for _ = 1, 64 do
            local bt = time_at_beat(b)
            if not bt or bt > game_time + look then break end
            local z = (bt - game_time) / look
            if z >= L.zb then
                local _, into = bar(bt)
                local num, den = time_signature(bt)
                local r = into and math.floor(into + 0.5) or 1
                local strong = r == 0 or (num and math.abs(r - num * 4 / den) < 0.01)
                local y = y_of(L, z)
                local c = strong and { r = 120, g = 120, b = 150 } or { r = 55, g = 55, b = 75 }
                rect(x_of(L, 0, z), y, x_of(L, lanes, z), y + (strong and 2 or 1), c)
            end
            b = b + 1
        end
    end

    for e = 0, lanes do
        local edge = e == 0 or e == lanes
        line(x_of(L, e, zt), y_of(L, zt), x_of(L, e, L.zb), height,
            edge and { r = 150, g = 150, b = 190 } or { r = 45, g = 45, b = 65 })
    end

    -- hit line and frets
    rect(x_of(L, 0, 0), L.hit_y - 1, x_of(L, lanes, 0), L.hit_y + 1, { r = 200, g = 200, b = 230 })
    for lane = 0, lanes - 1 do
        local x = x_of(L, lane + 0.5, 0)
        local c = LANE_COLORS[lane + 1]
        local r = lw * 0.36
        if input_down(LANE_INPUT[lane + 1]) then circle(x, L.hit_y, r, with_a(c, 0.45)) end
        ring(x, L.hit_y, r, c)
        ring(x, L.hit_y, r - 1, c)
        local f = flash[lane] or 0
        if f > 0 then
            circle(x, L.hit_y, r * (1 + (1 - f) * 0.6), with_a(c, f * 0.5))
        end
    end

    -- gems, far to near
    local gems = chart.gems
    local i0 = first_at(gems, game_time - 4)
    local i1 = i0
    while gems[i1] and gems[i1].t <= game_time + look do i1 = i1 + 1 end
    for i = i1 - 1, i0, -1 do
        local g = gems[i]
        local c = LANE_COLORS[g.lane + 1]
        local z = (g.t - game_time) / look
        if g.tail then
            local zs = g.holding and 0 or z
            local ze = math.min(1, (g.tail - game_time) / look)
            zs = math.max(zs, L.zb)
            if ze > zs then
                local col
                if g.holding then
                    col = with_a(c, 0.95)
                elseif g.state == "hit" or g.state == "miss" then
                    col = { r = 90, g = 90, b = 100, a = 0.5 }
                else
                    col = with_a(c, 0.6)
                end
                local w0, w1 = lw * 0.09 * s_of(zs), lw * 0.09 * s_of(ze)
                local xc0, xc1 = x_of(L, g.lane + 0.5, zs), x_of(L, g.lane + 0.5, ze)
                local y0, y1 = y_of(L, zs), y_of(L, ze)
                polygon({ xc0 - w0, y0, xc1 - w1, y1, xc1 + w1, y1, xc0 + w0, y0 }, col)
            end
        end
        if g.state ~= "hit" and z >= L.zb and z <= 1 then
            local s = s_of(z)
            local x, y = x_of(L, g.lane + 0.5, z), y_of(L, z)
            local r = lw * 0.34 * s
            local body = g.state == "miss" and { r = 80, g = 80, b = 90 } or c
            circle(x, y, r, { r = 10, g = 10, b = 15 })
            circle(x, y, r * 0.85, body)
            circle(x, y, r * 0.45, { r = 245, g = 245, b = 250, a = g.state == "miss" and 0.2 or 0.85 })
        end
    end

    for _, pt in ipairs(particles) do
        local h = pt.size / 2
        rect(pt.x - h, pt.y - h, pt.x + h, pt.y + h, with_a(pt.col, pt.life / pt.max))
    end

    -- HUD
    local hud = { r = 225, g = 228, b = 240 }
    local mult = math.min(4, 1 + math.floor(stats.combo / 10))
    text(12, 12, fmt(stats.score), hud, th * 2)
    text(12, 12 + th * 2, "x" .. mult .. "   " .. stats.combo .. " COMBO", { r = 255, g = 200, b = 80 }, th)
    local judged = stats.perfect + stats.good + stats.miss
    local acc = judged > 0 and (stats.perfect + stats.good) / judged * 100 or 100
    local right_txt = string.format("%d%%", math.floor(acc + 0.5))
    local w = text_size(right_txt, th * 2)
    text(width - w - 12, 12, right_txt, hud, th * 2)
    local label = track_name(play_track) .. "  " .. DIFFS[sel_diff].name
    w = text_size(label, th)
    text(width - w - 12, 12 + th * 2, label, { r = 150, g = 155, b = 180 }, th)

    -- progress
    if p.length and p.length > 0 then
        local frac = math.max(0, math.min(1, game_time / p.length))
        rect(0, 0, width * frac, 3, { r = 120, g = 160, b = 255 })
    end

    if popup and popup.life > 0 then
        centered(popup.text, L.cx, L.hit_y - lw * 1.3, with_a(popup.col, math.min(1, popup.life * 3)), th * 2)
    end
end

local function draw_menu(width, height, th, lanes, pspeed, look)
    local white = { r = 230, g = 232, b = 245 }
    local dim = { r = 140, g = 145, b = 170 }
    local p = playback()
    text(16, 12, "HIGHWAY", { r = 255, g = 200, b = 80 }, th * 2)
    text(16 + text_size("HIGHWAY", th * 2) + 16, 12 + th * 0.8, p.song_name or "", dim, th)

    if #tracks == 0 then
        centered("load a MIDI song to play", width / 2, height / 2, white, th)
        return
    end

    local mx, my = mouse()
    local clicked = mouse_pressed("left") and mx

    -- strips: every track charted at the chosen difficulty, scrolling
    local pad = 12
    local n = #tracks
    local sw = math.min((width - pad * 2) / n, th * 14)
    local x0 = (width - sw * n) / 2
    local ytop = 12 + th * 3
    local ybot = height - th * 5.4
    local hsc = math.max(FONT_HEIGHT, math.min(th, math.floor(sw / 70) * FONT_HEIGHT))
    for i, tr in ipairs(tracks) do
        local sx = x0 + (i - 1) * sw
        local sel = i == sel_track
        local over = mx and mx >= sx and mx < sx + sw and my >= ytop and my <= ybot
        if clicked and over then sel_track = i end
        rect(sx + 2, ytop, sx + sw - 2, ybot,
            sel and { r = 35, g = 35, b = 55 } or (over and { r = 26, g = 26, b = 38 } or { r = 18, g = 18, b = 26 }))
        if sel then
            local c = { r = 255, g = 200, b = 80 }
            line(sx + 2, ytop, sx + sw - 2, ytop, c)
            line(sx + 2, ybot, sx + sw - 2, ybot, c)
            line(sx + 2, ytop, sx + 2, ybot, c)
            line(sx + sw - 2, ytop, sx + sw - 2, ybot, c)
        end
        local ch = get_chart(tr, sel_diff, lanes)
        text(sx + 6, ytop + 4, track_name(tr), sel and white or dim, hsc)
        text(sx + 6, ytop + 4 + hsc, #ch.gems .. " gems", dim, hsc)

        local hy0 = ytop + 8 + hsc * 2
        local iw = sw - 12
        local lw = iw / lanes
        local hit_y = ybot - lw * 0.6
        for e = 1, lanes - 1 do
            line(sx + 6 + e * lw, hy0, sx + 6 + e * lw, ybot - 2, { r = 40, g = 40, b = 58 })
        end
        line(sx + 6, hit_y, sx + 6 + iw, hit_y, { r = 110, g = 110, b = 140 })
        local gems = ch.gems
        local k = first_at(gems, preview_t - 3)
        while gems[k] and gems[k].t <= preview_t + PREVIEW_SECONDS do
            local g = gems[k]
            local c = LANE_COLORS[g.lane + 1]
            local function yt(t) return hit_y - (t - preview_t) / PREVIEW_SECONDS * (hit_y - hy0) end
            local x = sx + 6 + (g.lane + 0.5) * lw
            if g.tail then
                local ya, yb = math.max(hy0, yt(g.tail)), math.min(ybot - 2, yt(g.t))
                if yb > ya then rect(x - lw * 0.1, ya, x + lw * 0.1, yb, with_a(c, 0.55)) end
            end
            local y = yt(g.t)
            if y >= hy0 and y <= ybot - 2 then
                circle(x, y, math.max(1.5, lw * 0.32), c)
            end
            k = k + 1
        end
    end

    -- selected track info
    local tr = tracks[sel_track]
    local info = string.format("%s   %d notes   %.1f notes/s   best %s", track_name(tr), #tr.notes, tr.nps,
        fmt(store_get(best_key(tr.ch, sel_diff, SPEEDS[speed_idx])) or 0))
    centered(info, width / 2, height - th * 4.8, white, th)

    -- difficulty buttons and play
    local bw, bh = th * 6, th * 1.5
    local total = bw * 6 + 8 * 5
    local bx = (width - total) / 2
    local by = height - th * 3.2
    for i = 1, 6 do
        local x = bx + (i - 1) * (bw + 8)
        local over = mx and mx >= x and mx < x + bw and my >= by and my < by + bh
        local label, bg
        if i <= 4 then
            label = DIFFS[i].name
            bg = i == sel_diff and { r = 90, g = 70, b = 20 } or { r = 30, g = 30, b = 44 }
        elseif i == 5 then
            label = "SPEED " .. math.floor(SPEEDS[speed_idx] * 100) .. "%"
            bg = speed_idx > 1 and { r = 30, g = 60, b = 100 } or { r = 30, g = 30, b = 44 }
        else
            label = "PLAY"
            bg = { r = 40, g = 120, b = 60 }
        end
        if over then bg = { r = bg.r + 25, g = bg.g + 25, b = bg.b + 25 } end
        rect(x, by, x + bw, by + bh, bg)
        centered(label, x + bw / 2, by + (bh - th) / 2, white, th)
        if clicked and over then
            if i <= 4 then
                sel_diff = i
            elseif i == 5 then
                speed_idx = speed_idx % #SPEEDS + 1
            else
                start_game()
            end
        end
    end

    local list = playlist()
    local songs = (list and #list.entries > 1) and "   N/B song" or ""
    local hint = has_focus() and ("LEFT/RIGHT track   UP/DOWN difficulty   S speed" .. songs .. "   ENTER to start")
        or "click here first so the keys reach the game"
    if list and list.current then
        local where = string.format("%s  %d/%d", list.name, list.current, #list.entries)
        local w = text_size(where, th)
        text(width - w - 16, 12 + th * 0.8, where, dim, th)
    end
    centered(hint, width / 2, height - th * 1.4, dim, FONT_HEIGHT * math.max(1, math.floor(th / FONT_HEIGHT)))
end

local function draw_results(width, height, th)
    local white = { r = 230, g = 232, b = 245 }
    local gold = { r = 255, g = 200, b = 80 }
    local judged = stats.perfect + stats.good + stats.miss
    local acc = judged > 0 and (stats.perfect + stats.good) / judged * 100 or 0
    local y = height * 0.2
    local spd = SPEEDS[speed_idx] ~= 1 and ("  " .. math.floor(SPEEDS[speed_idx] * 100) .. "%") or ""
    centered(track_name(play_track) .. "  " .. DIFFS[sel_diff].name .. spd, width / 2, y, gold, th * 2)
    y = y + th * 3
    centered(fmt(stats.score), width / 2, y, white, th * 4)
    y = y + th * 4.5
    if new_best then
        centered("NEW BEST!", width / 2, y, gold, th)
    else
        centered("best " .. fmt(result_best), width / 2, y, white, th)
    end
    y = y + th * 2
    centered(string.format("%d%% hit   %d perfect   %d good   %d missed   longest combo %d",
        math.floor(acc + 0.5), stats.perfect, stats.good, stats.miss, stats.max_combo), width / 2, y, white, th)
    local list = playlist()
    local more = (list and #list.entries > 1) and "   N next song" or ""
    centered("ENTER or click to go back" .. more, width / 2, height - th * 2, { r = 140, g = 145, b = 170 }, th)
end

-- main ----------------------------------------------------------------------

function render(width, height, left, right)
    local lanes = setting_int("lanes", 4, 3, 5)
    local hw_secs = setting_float("highway_seconds", 1.6, 0.6, 4)
    local offset_ms = setting_int("input_offset_ms", 0, -250, 250)
    -- off by default for now: toggling a channel causes a jump in playback
    local mute_on_miss = setting_bool("mute_on_miss_experimental", false)
    local ghost_penalty = setting_bool("ghost_taps_break_combo", true)

    local p = playback()
    local pspeed = math.max(p.speed or 1, 0.01)
    local look = hw_secs * pspeed
    hw_secs_cur = hw_secs
    local th = FONT_HEIGHT * math.max(1, math.floor(math.min(width, height) / 360))

    if state == "init" then
        -- left over from a restart mid-game
        local m = store_get("muted")
        if m then
            set_channel_enabled(m, true)
            store_set("muted", nil)
        end
        borrowed = store_get("borrowed")
    end
    if state == "init" or p.song_loads ~= last_loads then
        -- a song was loaded (the same one again counts): it's paused at the start, so just
        -- show the menu, rewinding only if the script was started on a song already going
        local first = state == "init"
        last_loads = p.song_loads
        if p.song_id ~= song_key or first then
            song_key = p.song_id
            analyze(p)
        end
        enter_menu(first)
    end

    local focus = has_focus()

    -- changing song: the new one loads paused, and the song_loads check brings up its menu
    if state == "menu" or state == "results" then
        local list = playlist()
        if list and #list.entries > 1 then
            if input(NEXT_SONG) == "pressed" then next_track() end
            if input(PREV_SONG) == "pressed" then previous_track() end
        end
    end

    if state == "menu" then
        if p.paused then menu_seen_paused = true end
        if #tracks > 0 then
            if input(PREV) == "pressed" then sel_track = (sel_track - 2) % #tracks + 1 end
            if input(NEXT) == "pressed" then sel_track = sel_track % #tracks + 1 end
            if input(HARDER) == "pressed" then sel_diff = math.min(4, sel_diff + 1) end
            if input(EASIER) == "pressed" then sel_diff = math.max(1, sel_diff - 1) end
            if input(SPEED) == "pressed" then speed_idx = speed_idx % #SPEEDS + 1 end
            local _, sy = scroll()
            preview_t = preview_t + DT - sy * 0.02
            if p.length and preview_t > p.length then preview_t = tracks[sel_track].notes[1].start - 0.5 end
            preview_t = math.max(-1, preview_t)
            draw_menu(width, height, th, lanes, pspeed, look)
            if state == "menu" and (input(START) == "pressed" or input(PAUSE) == "pressed"
                    or (menu_seen_paused and not p.paused)) then
                start_game() -- pressing play in the app starts too
            end
        else
            draw_menu(width, height, th, lanes, pspeed, look)
        end
    elseif state == "results" then
        draw_results(width, height, th)
        if input(START) == "pressed" or mouse_pressed("left") then enter_menu(true) end
    else
        local L = game_layout(width, height, chart.lanes)

        if input(BACK) == "pressed" then
            enter_menu(true)
            return
        end

        if state == "preroll" then
            game_time = game_time + DT * pspeed
            if game_time >= 0 then
                game_time = 0
                set_paused(false)
                state = "starting"
            end
        elseif state == "starting" then
            if not p.paused then
                state = "play"
                last_gen, last_pos = p.generation, p.position
            end
        end

        if state == "play" then
            game_time = p.position
            if p.generation ~= last_gen then
                if p.position < last_pos and p.length and last_pos > p.length - 1.5 then
                    finish() -- looped back around
                else
                    -- a seek: notes before it don't count, everything after is fresh
                    local cut = p.position - WIN_GOOD * pspeed
                    for _, g in ipairs(chart.gems) do
                        if g.t < cut then
                            if not g.state then g.state = "skip" end
                        else
                            g.state, g.holding = nil, false
                        end
                    end
                    holding, first_live = {}, 1
                end
                last_gen = p.generation
            end
            if state == "play" and p.finished then finish() end
            if state == "play" then
                if input(PAUSE) == "pressed" then set_paused(not p.paused) end
                if last_focus and not focus and not p.paused then set_paused(true) end
                update_play(p, pspeed, L, offset_ms / 1000, mute_on_miss, ghost_penalty)
            end
            last_pos = p.position
        end

        -- effects
        for lane = 0, 4 do
            flash[lane] = math.max(0, (flash[lane] or 0) - DT * 4)
        end
        if popup then popup.life = popup.life - DT end
        for i = #particles, 1, -1 do
            local pt = particles[i]
            pt.life = pt.life - DT
            if pt.life <= 0 then
                particles[i] = particles[#particles]
                particles[#particles] = nil
            else
                pt.vy = pt.vy + L.range * 1.5 * DT
                pt.x, pt.y = pt.x + pt.vx * DT, pt.y + pt.vy * DT
            end
        end

        if state ~= "results" and chart then
            draw_game(p, L, look, th, width, height)
            if state == "preroll" then
                local secs = math.ceil(-game_time / pspeed)
                centered("GET READY", L.cx, height * 0.35, { r = 255, g = 200, b = 80 }, th * 2)
                if secs <= 3 then centered(tostring(secs), L.cx, height * 0.35 + th * 2.5, { r = 255, g = 255, b = 255 },
                        th * 3) end
            elseif state == "play" and p.paused then
                centered("PAUSED", L.cx, height * 0.4, { r = 255, g = 255, b = 255 }, th * 3)
                centered("SPACE to resume   BACKSPACE for the menu", L.cx, height * 0.4 + th * 3.5,
                    { r = 170, g = 175, b = 200 }, th)
            end
            if not focus then
                centered("click to focus", L.cx, height * 0.55, { r = 255, g = 120, b = 120 }, th)
            end
        end
    end
    last_focus = focus
end
