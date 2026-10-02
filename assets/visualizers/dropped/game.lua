-- note_runner.lua
-- A platformer whose level is the music. Every note is a platform: pitch
-- sets its height, duration its length, and its left edge crosses the
-- playhead line exactly when the note sounds.
--
-- One channel at a time: only the active channel's notes are solid. Every
-- few bars the song hands over to another channel; a gate in the next
-- channel's color scrolls in ahead of the switch, and that channel's notes
-- are outlined so you can line up the jump. When the gate reaches the
-- playhead, the old channel's platforms vanish under you.
-- (Turn "one channel at a time" off in Script Settings for everything solid.)
--
-- Scoring
--   * A note pays out once, when it starts while you're on it: +50 x mult.
--   * Land on it within PERFECT_WINDOW of it starting: +100 x mult extra.
--   * Standing on a sounding note trickles points in.
--   * Multiplier +1 every 4 combo (max x8). Falling resets the combo.
--
-- Controls are rebindable under Controls in Script Settings. Defaults:
--   move  arrows / A D      jump  space / up / W / Z / left click (x2)
--   drop through  down / S  pause  P      restart song  R
--
-- Plain audio files: platforms come from onsets instead, all solid.
-- Best scores are saved per song (by playback().song_id).

local JUMP    = input_register("jump", { "space", "up", "w", "z" })
local LEFT    = input_register("left", { "left", "a" })
local RIGHT   = input_register("right", { "right", "d" })
local DROP    = input_register("drop through", { "down", "s" })
local PAUSE   = input_register("pause", "p")
local RESTART = input_register("restart song", "r")

-- Player sprite sheet: stand, walk, jump (8x8 each), facing right.
local HERO
do
    local frames = {
        { "00111100", "01111110", "11111211", "11111111", "13333331", "01111110", "01100110", "01100110" },
        { "00111100", "01111110", "11111211", "11111111", "13333331", "01111110", "11000110", "10000011" },
        { "00111100", "01111110", "11111211", "11111111", "13333331", "11111111", "00111100", "00000000" },
    }
    local rows = {}
    for r = 1, 8 do
        local row = {}
        for f = 1, 3 do
            local s = frames[f][r]
            for c = 1, 8 do row[#row + 1] = tonumber(s:sub(c, c)) end
        end
        rows[r] = row
    end
    HERO = sprite_register({
        palette = { { r = 240, g = 240, b = 250 }, { r = 20, g = 20, b = 35 }, { r = 255, g = 200, b = 60 } },
        image = rows,
    })
end

local PALETTE              = {
    { r = 255, g = 99,  b = 132 }, { r = 54, g = 162, b = 235 },
    { r = 255, g = 206, b = 86 }, { r = 75, g = 192, b = 192 },
    { r = 153, g = 102, b = 255 }, { r = 255, g = 159, b = 64 },
    { r = 120, g = 230, b = 120 }, { r = 240, g = 120, b = 220 },
    { r = 100, g = 220, b = 255 }, { r = 230, g = 230, b = 110 },
    { r = 180, g = 140, b = 255 }, { r = 255, g = 130, b = 110 },
    { r = 90,  g = 200, b = 160 }, { r = 200, g = 200, b = 200 },
    { r = 255, g = 180, b = 200 }, { r = 140, g = 180, b = 255 },
}
local WHITE                = { r = 255, g = 255, b = 255 }
local GOLD                 = { r = 255, g = 210, b = 70 }
local RED                  = { r = 255, g = 60, b = 80 }

-- Tuning. x is a fraction of width, y of height (0 = top), so it all
-- survives resizing. Times are song seconds.
local GRAVITY              = 3.2
local JUMP_VEL             = 1.25
local AIR_JUMP_VEL         = 1.0
local MOVE_SPEED           = 0.5
local COYOTE               = 0.08
local JUMP_BUFFER          = 0.12
local PERFECT_WINDOW       = 0.12
local MIN_PLAT_W           = 0.035
local PLAT_THICK           = 0.018
local SWITCH_GRACE         = 0.2 -- old channel stays solid this long after a switch
local MAX_GAP              = 2.0 -- a channel with a longer rest can't take a segment
local MIN_NOTES            = 4 -- ...nor one with fewer notes in it

-- Per-frame values, set at the top of render()
local POS, S, PLAY, LO, HI = 0, 0.3, 0.3, 36, 96
local ONE                  = true
local CUR, NEXT, GRACE_CH  = nil, nil, nil

-- State
local midi                 = false
local segs                 = {} -- switch schedule: {t0, t1, ch}
local sched_key            = nil
local audio_plats, audio_n = {}, 0
local scored               = {} -- note id -> true once it paid out
local player, pad          = nil, nil
local score, combo         = 0, 0
local best, best_key       = 0, nil -- best score for the loaded song, and its store key
local popups               = {}
local flash                = 0
local last_seg             = nil

local function clamp(v, a, b)
    if v < a then return a elseif v > b then return b end
    return v
end
local function rgba(c, a) return { r = c.r, g = c.g, b = c.b, a = a } end
local function mult() return math.min(8, 1 + math.floor(combo / 4)) end
local function ch_color(ch) return PALETTE[(ch or 0) % 16 + 1] end
local function ch_name(ch) return "CH " .. (ch + 1) end -- as the GUI numbers them

local function popup(s, x, y, col)
    popups[#popups + 1] = { text = s, x = x, y = y, age = 0, col = col }
end

-- Switch schedule -----------------------------------------------------------
-- Split the song into chunks of `per_bars` bars (8 s chunks without a tempo
-- map), and give each to a channel that's busy enough there to stand on:
-- at least MIN_NOTES notes and no rest longer than MAX_GAP. Prefer a
-- different channel than last time; neighbors with the same channel merge.

local function build_schedule(per_bars)
    segs = {}
    local len = playback().length or 0
    local notes = notes_between(0, len + 1)

    LO, HI = 127, 0
    for _, n in ipairs(notes) do
        if n.key < LO then LO = n.key end
        if n.key > HI then HI = n.key end
    end
    if LO > HI then LO, HI = 36, 96 end
    LO, HI = LO - 2, HI + 2
    if HI - LO < 12 then
        local mid = (LO + HI) / 2
        LO, HI = mid - 6, mid + 6
    end

    local bounds = {}
    if beat(0) then
        local b, t = 0, 0
        for _ = 1, 100000 do
            if t >= len then break end
            bounds[#bounds + 1] = t
            local num, den = time_signature(t)
            b = b + per_bars * num * 4 / den
            local nt = time_at_beat(b)
            if not nt or nt <= t then break end
            t = nt
        end
    else
        local t = 0
        while t < len do
            bounds[#bounds + 1] = t
            t = t + 8
        end
    end
    if #bounds == 0 then bounds[1] = 0 end

    local ni, prev = 1, nil
    for i = 1, #bounds do
        local t0, t1 = bounds[i], bounds[i + 1] or math.huge
        local stats = {}
        while ni <= #notes and notes[ni].start < t1 do
            local n = notes[ni]
            if channel_enabled(n.channel) then
                local s = stats[n.channel]
                if not s then
                    s = { count = 0, last = t0, gap = 0 }
                    stats[n.channel] = s
                end
                s.count = s.count + 1
                s.gap = math.max(s.gap, n.start - s.last)
                s.last = math.max(s.last, n.stop)
            end
            ni = ni + 1
        end

        local endt = math.min(t1, len)
        local cands, busiest, most = {}, nil, 0
        for ch, s in pairs(stats) do
            local gap = math.max(s.gap, endt - s.last)
            if s.count >= MIN_NOTES and gap <= MAX_GAP then cands[#cands + 1] = ch end
            if s.count > most or (s.count == most and ch < busiest) then busiest, most = ch, s.count end
        end
        table.sort(cands)
        if #cands > 1 and prev then
            for j = #cands, 1, -1 do
                if cands[j] == prev then table.remove(cands, j) end
            end
        end
        local ch
        if #cands > 0 then
            ch = cands[(i * 7 + 3) % #cands + 1]
        else
            ch = busiest or prev
        end

        local last = segs[#segs]
        if last and (ch == nil or ch == last.ch) then
            last.t1 = t1
        else
            segs[#segs + 1] = { t0 = t0, t1 = t1, ch = ch }
        end
        prev = ch or prev
    end
end

local function seg_at(t)
    local lo, hi = 1, #segs
    if hi == 0 then return nil, 0 end
    while lo < hi do
        local mid = math.floor((lo + hi + 1) / 2)
        if segs[mid].t0 <= t then lo = mid else hi = mid - 1 end
    end
    return segs[lo], lo
end

-- Platforms ----------------------------------------------------------------
-- MIDI notes come straight from notes_between each frame; audio onsets are
-- kept in audio_plats in the same shape ({id, start, stop, frac}).

local function plat_y(n)
    local frac = n.frac
    if n.key then frac = clamp((n.key - LO) / (HI - LO), 0, 1) end
    return 0.88 - frac * 0.68
end

local function plat_x(n)
    local x0 = PLAY + (n.start - POS) * S
    local x1 = PLAY + (n.stop - POS) * S
    if x1 < x0 + MIN_PLAT_W then x1 = x0 + MIN_PLAT_W end
    return x0, x1
end

local function sounding(n) return n.start <= POS and POS < n.stop end

local function solid(n)
    if n.channel == nil then return true end
    if not channel_enabled(n.channel) then return false end
    if not ONE or not CUR or CUR.ch == nil then return true end
    return n.channel == CUR.ch or n.channel == GRACE_CH
end

local function gather()
    local list, byid = {}, {}
    if midi then
        for _, n in ipairs(notes_between(POS - PLAY / S - 0.5, POS + (1 - PLAY) / S + 0.5)) do
            if channel_enabled(n.channel) then
                list[#list + 1] = n
                byid[n.id] = n
            end
        end
    else
        for _, n in ipairs(audio_plats) do
            list[#list + 1] = n
            byid[n.id] = n
        end
    end
    return list, byid
end

local function spawn_audio(left)
    local spec = fft_left(left)
    local hit = onset()
    if hit and #spec > 16 then
        -- spectral centroid, log-scaled, picks the height
        local sum, wsum = 0, 0
        for i = 2, math.min(256, #spec) do
            sum = sum + spec[i]
            wsum = wsum + spec[i] * (i - 1)
        end
        if sum > 0 then
            local frac = clamp(math.log(wsum / sum) / math.log(255), 0, 1)
            local lead = (1.05 - PLAY) / S
            local len = 0.2 + clamp(level_left() * 2, 0, 0.5)
            audio_n = audio_n + 1
            audio_plats[#audio_plats + 1] = {
                id = "a" .. audio_n,
                frac = frac,
                start = POS + lead,
                stop = POS + lead + len
            }
        end
    end
    for i = #audio_plats, 1, -1 do
        if audio_plats[i].stop < POS - PLAY / S - 1 then table.remove(audio_plats, i) end
    end
end

-- Player --------------------------------------------------------------------

local function respawn(fell)
    pad = { x0 = PLAY - 0.06, x1 = PLAY + 0.06, y = 0.45, life = 2.0 }
    player = {
        x = PLAY,
        y = pad.y,
        vy = 0,
        on = "pad",
        coyote = 0,
        buffer = 0,
        air_jumps = 1,
        drop_t = 0,
        land_pos = POS,
        last_pos = POS,
        face = 1,
        moving = false
    }
    if fell then
        combo = 0
        flash = 1
    end
end

local function support(id, byid)
    if id == "pad" then
        if pad.life > 0 then return pad.x0, pad.x1, pad.y end
        return nil
    end
    local n = byid[id]
    if not n or not solid(n) then return nil end
    local x0, x1 = plat_x(n)
    return x0, x1, plat_y(n)
end

local function step(dt, dpos, inp, half, list, byid)
    local p = player
    p.buffer = math.max(0, p.buffer - dt)
    p.coyote = math.max(0, p.coyote - dt)
    p.drop_t = math.max(0, p.drop_t - dt)
    pad.life = pad.life - dt

    if inp.dir ~= 0 then p.face = inp.dir end
    p.moving = inp.dir ~= 0
    p.x = p.x + inp.dir * MOVE_SPEED * dt
    if p.on and p.on ~= "pad" then p.x = p.x - S * dpos end -- carried along

    if p.on then
        local x0, x1, y = support(p.on, byid)
        if x0 and p.x + half >= x0 and p.x - half <= x1 then
            p.y, p.vy = y, 0
        else
            p.on = nil
            p.coyote = COYOTE
        end
    end

    if p.buffer > 0 then
        if p.on or p.coyote > 0 then
            p.vy, p.on, p.coyote, p.buffer = -JUMP_VEL, nil, 0, 0
        elseif p.air_jumps > 0 then
            p.vy, p.buffer = -AIR_JUMP_VEL, 0
            p.air_jumps = p.air_jumps - 1
        end
    end

    if not p.on then
        local g = GRAVITY
        if p.vy < 0 and not inp.jump_held then g = g * 2.5 end -- short hop
        p.vy = math.min(p.vy + g * dt, 2.5)
        local prev = p.y
        p.y = p.y + p.vy * dt
        if p.vy > 0 and p.drop_t <= 0 then
            local best_id, best_y = nil, math.huge
            local function try(id)
                local x0, x1, y = support(id, byid)
                if x0 and prev <= y + 0.002 and p.y >= y and y < best_y
                    and p.x + half >= x0 and p.x - half <= x1 then
                    best_id, best_y = id, y
                end
            end
            try("pad")
            for _, n in ipairs(list) do try(n.id) end
            if best_id then
                p.on, p.y, p.vy, p.air_jumps = best_id, best_y, 0, 1
                p.land_pos = POS
            end
        end
    end

    p.x = clamp(p.x, half, 1 - half)
end

local function update_score(dpos, byid)
    local p = player
    local n = p.on and p.on ~= "pad" and byid[p.on]
    if not n or not sounding(n) then return end
    if not scored[n.id] then
        scored[n.id] = true
        combo = combo + 1
        local pts = 50 * mult()
        local perfect = math.abs(p.land_pos - n.start) <= PERFECT_WINDOW
        if perfect then pts = pts + 100 * mult() end
        score = score + pts
        popup("+" .. pts, p.x, p.y - 0.1, perfect and GOLD or WHITE)
    end
    score = score + dpos * 30 * mult()
end

-- Frame ---------------------------------------------------------------------

function render(width, height, left, right)
    local w, h = width, height
    if w < 32 or h < 32 then return end

    S              = setting_float("scroll speed", 0.3, 0.1, 1.0)
    PLAY           = setting_float("playhead position", 0.3, 0.1, 0.8)
    ONE            = setting_bool("one channel at a time", true)
    local per_bars = setting_int("bars per switch", 4, 1, 16)
    local body     = setting_color("player color", { r = 240, g = 240, b = 250 })

    local pb       = playback()
    POS            = pb.position

    -- best score is per song, keyed by the file's contents; a new song
    -- starts a new run
    local bk       = "best:" .. (pb.song_id or "none")
    if bk ~= best_key then
        if best_key then score, combo = 0, 0 end
        best_key = bk
        best = store_get(bk) or 0
    end
    midi = #midi_channels() > 0

    if input(PAUSE) == "pressed" then set_paused(not pb.paused) end
    if input(RESTART) == "pressed" then
        seek(0)
        score, combo = 0, 0
    end

    -- Rebuild the schedule on a new song, a seek, or a change to the
    -- channel toggles or bars setting; reset position-tracking state.
    local key = pb.generation .. "|" .. per_bars
    for _, c in ipairs(midi_channels()) do
        if channel_enabled(c) then key = key .. "," .. c end
    end
    local dpos = 0
    if key ~= sched_key then
        local jumped = sched_key == nil or not sched_key:find("^" .. pb.generation .. "|")
        sched_key = key
        if midi then build_schedule(per_bars) else segs = {} end
        if jumped then
            audio_plats, scored, last_seg = {}, {}, nil
            respawn(false)
        end
    elseif player then
        dpos = POS - player.last_pos
    end
    if not player then respawn(false) end
    player.last_pos = POS

    local i
    CUR, i = seg_at(POS)
    NEXT = CUR and segs[i + 1] or nil
    GRACE_CH = (CUR and i > 1 and POS - CUR.t0 < SWITCH_GRACE) and segs[i - 1].ch or nil
    if ONE and CUR and CUR ~= last_seg then
        if last_seg and CUR.ch then popup(ch_name(CUR.ch), 0.5, 0.12, ch_color(CUR.ch)) end
        last_seg = CUR
    end

    local sc = math.max(1, math.floor(h * 0.05 / 8)) -- sprite scale
    local half = (sc * 4) / w

    if not midi and not pb.paused then spawn_audio(left) end
    local list, byid = gather()

    if not pb.paused then
        local inp = {
            dir = (input_down(RIGHT) and 1 or 0) - (input_down(LEFT) and 1 or 0),
            jump_held = input_down(JUMP) or mouse_down("left"),
        }
        if input(JUMP) == "pressed" or mouse_pressed("left") then player.buffer = JUMP_BUFFER end
        if input(DROP) == "pressed" and player.on then
            player.on, player.drop_t, player.vy = nil, 0.2, 0
        end

        local n = math.max(1, math.ceil(DT / 0.016))
        for _ = 1, n do step(DT / n, dpos / n, inp, half, list, byid) end
        update_score(dpos, byid)

        if player.y > 1.08 then
            popup("miss", PLAY, 0.85, RED)
            respawn(true)
        end
    end
    if score > best then
        best = score
        store_set(best_key, math.floor(best))
    end

    -- draw ------------------------------------------------------------------
    clear({ r = 8, g = 8, b = 16 })
    if ONE and CUR and CUR.ch then rect(0, 0, w, h, rgba(ch_color(CUR.ch), 0.05)) end
    rect(0, h * 0.94, w, h, { r = 120, g = 20, b = 40, a = 0.35 })
    line(PLAY * w, 0, PLAY * w, h, { r = 255, g = 255, b = 255, a = 0.12 })

    local th = PLAT_THICK * h
    local next_ch = ONE and NEXT and NEXT.ch
    for _, n in ipairs(list) do
        local x0, x1 = plat_x(n)
        if x1 > 0 and x0 < 1 then
            local px0, px1, py = x0 * w, x1 * w, plat_y(n) * h
            local c = n.channel and ch_color(n.channel) or ch_color(math.floor(n.frac * 15))
            if solid(n) then
                if sounding(n) then
                    rect(px0, py - th * 1.5, px1, py, rgba(c, 0.22))
                    rect(px0, py, px1, py + th, c)
                else
                    rect(px0, py, px1, py + th, rgba(c, 0.6))
                end
                if scored[n.id] then line(px0, py, px1, py, rgba(WHITE, 0.8)) end
            elseif n.channel == next_ch then
                rect(px0, py, px1, py + th, rgba(c, 0.12))
                line(px0, py, px1, py, rgba(c, 0.85))
                line(px0, py + th, px1, py + th, rgba(c, 0.85))
            else
                rect(px0, py, px1, py + th, rgba(c, 0.07))
            end
        end
    end

    local TH = FONT_HEIGHT * math.max(1, math.floor(h / 300))

    -- the gate for the next switch
    if next_ch and CUR.t1 < math.huge then
        local gx = (PLAY + (CUR.t1 - POS) * S) * w
        if gx >= 0 and gx <= w then
            local c = ch_color(next_ch)
            rect(gx - 2, 0, gx + 2, h, rgba(c, 0.55))
            rect(gx, 0, gx + w * 0.02, h, rgba(c, 0.08))
            text(gx + 4, h * 0.94 - TH - 4, ch_name(next_ch), rgba(c, 0.9), TH)
        end
    end

    if pad.life > 0 then
        rect(pad.x0 * w, pad.y * h, pad.x1 * w, pad.y * h + th, rgba(WHITE, clamp(pad.life / 2, 0.15, 0.8)))
    end

    -- player
    local p = player
    local ph = sc * 8
    local px, py = p.x * w, p.y * h
    local on_note = p.on and p.on ~= "pad" and byid[p.on]
    if on_note and sounding(on_note) and on_note.channel then
        rect(px - ph * 0.7, py - ph * 1.2, px + ph * 0.7, py, rgba(ch_color(on_note.channel), 0.3))
    end
    local frame = 0
    if not p.on then
        frame = 2
    elseif p.moving and math.floor(TIME * 10) % 2 == 1 then
        frame = 1
    end
    -- blink while standing on the respawn pad
    local tint = nil
    if p.on == "pad" and pad.life > 0 and math.floor(TIME * 12) % 2 == 0 then
        tint = { r = 255, g = 255, b = 255, a = 0.35 }
    end
    sprite(HERO, px - ph / 2, py - ph, {
        scale = sc,
        src = { frame * 8, 0, 8, 8 },
        flip_x = p.face < 0,
        palette = { [1] = body },
        tint = tint
    })

    -- popups
    for k = #popups, 1, -1 do
        local u = popups[k]
        u.age = u.age + DT
        u.y = u.y - 0.12 * DT
        if u.age > 0.9 then
            table.remove(popups, k)
        else
            local tw = text_size(u.text, TH)
            text(u.x * w - tw / 2, u.y * h, u.text, rgba(u.col, 1 - u.age / 0.9), TH)
        end
    end

    if flash > 0 then
        rect(0, 0, w, h, rgba(RED, flash * 0.3))
        flash = math.max(0, flash - DT * 2)
    end

    -- HUD
    local m = 6
    text(m, m, tostring(math.floor(score)), WHITE, TH * 2)
    if mult() > 1 then text(m, m + TH * 2, "x" .. mult(), GOLD, TH) end
    local bs = "best " .. math.floor(best)
    text(w - text_size(bs, TH) - m, m, bs, rgba(WHITE, 0.45), TH)

    if ONE and CUR and CUR.ch then
        local y = m + TH * 1.4
        local c = ch_color(CUR.ch)
        local label = ch_name(CUR.ch)
        local lw = text_size(label, TH)
        rect(w - lw - m - TH, y + TH * 0.2, w - lw - m - TH * 0.4, y + TH * 0.8, c)
        text(w - lw - m, y, label, c, TH)
        if next_ch and CUR.t1 < math.huge then
            local left_in
            local b0, b1 = beat(POS), beat(CUR.t1)
            if b0 and b1 then
                left_in = math.ceil(b1 - b0) .. " beats"
            else
                left_in = math.ceil(CUR.t1 - POS) .. "s"
            end
            local s = "next " .. ch_name(next_ch) .. " in " .. left_in
            text(w - text_size(s, TH) - m, y + TH, s, rgba(ch_color(next_ch), 0.8), TH)
        end
    end

    if pb.paused then
        local s = h * 0.06
        rect(w / 2 - s, h / 2 - s, w / 2 - s / 3, h / 2 + s, rgba(WHITE, 0.6))
        rect(w / 2 + s / 3, h / 2 - s, w / 2 + s, h / 2 + s, rgba(WHITE, 0.6))
    end

    if not has_focus() then
        local c = rgba(WHITE, 0.2 + 0.15 * math.sin(TIME * 4))
        line(0, 0, w - 1, 0, c); line(0, h - 1, w - 1, h - 1, c)
        line(0, 0, 0, h - 1, c); line(w - 1, 0, w - 1, h - 1, c)
        local hint = "click to play"
        text((w - text_size(hint, TH)) / 2, h - TH * 2.5, hint, c, TH)
    end
end
