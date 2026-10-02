-- Disco Laser Ball
--
-- A faceted mirror ball spins in the middle of the screen and fires a laser
-- out of one of its tiles for every note. Inspired by Animusic, where the
-- instruments seem to PRODUCE the music rather than react to it: everything
-- is note-for-note, and anticipation comes from upcoming_notes().
--
--   * Channel -> color. Channels are spread across the spectrum in file
--     order: first channel blue, last channel red.
--   * Pitch -> where on the ball. Each channel owns a wedge of longitude and
--     its pitch range maps from the south pole (low) to the north pole (high),
--     so bass fires toward the floor and treble toward the sky. The tile that
--     fires lights up in the laser's color.
--   * Length -> bolt length. A note emits light for as long as it lasts, and
--     the light travels outward at a fixed speed. A short note is a short bolt
--     that flies off; a held note is a long beam that stays attached to the
--     ball, sweeping as the ball turns, and detaches the moment the note ends.
--   * Anticipation: ~0.16s before a note, its tile starts charging with a
--     contracting glow, so the beam appears to be "fired" on the beat.
--   * 3D: tiles are foreshortened toward the limb and shaded by a light, the
--     axis is tilted toward you, beams on the far side are dimmer and pass
--     behind the ball, and two orbit rings pass behind and in front of it.
--   * Spin = tempo. Each revolution takes BEATS_PER_REV beats. No BPM is
--     exposed by the API, so tempo is estimated from note onsets (folded into
--     70-180 BPM). Set FIXED_BPM below to override. The ball and rings also
--     pulse on an estimated beat phase.
--
-- Time is the audio clock (samples / SAMPLE_RATE), so everything freezes on
-- pause. Beam layers drop away automatically when there are lots of beams.

local floor, max, min, sqrt, exp, abs = math.floor, math.max, math.min, math.sqrt, math.exp, math.abs
local sin, cos, random, pi = math.sin, math.cos, math.random, math.pi
local TAU = pi * 2

-- Tuning ---------------------------------------------------------------
local FIXED_BPM = nil        -- e.g. 128 to force the tempo
local BEATS_PER_REV = 4      -- beats per full turn of the ball
local BANDS = 14             -- latitude rows of mirror tiles
local TILT = 0.38            -- axis tilt toward the viewer (radians)
local T_TRAVEL = 0.26        -- seconds for light to reach the screen edge
local CHARGE_T = 0.16        -- anticipation window before a note
local FLASH_T = 0.14         -- muzzle flash length
local MAX_BEAMS = 120
local MAX_PARTS = 120

-- Static data ----------------------------------------------------------
local LX, LY, LZ = -0.45, 0.55, 0.70
do
    local l = sqrt(LX * LX + LY * LY + LZ * LZ)
    LX, LY, LZ = LX / l, LY / l, LZ / l
end

-- Mirror tiles: BANDS latitude rows, each with a number of tiles that
-- shrinks toward the poles so tiles stay roughly square.
local ROW_COLS, ROW_START = {}, {}
local T_CPHI, T_SPHI, T_SLAM, T_CLAM, T_SEED = {}, {}, {}, {}, {}
local NTILES = 0
do
    local dphi = pi / BANDS
    for k = 1, BANDS do
        local phi = -pi / 2 + (k - 0.5) * dphi
        local cph = cos(phi)
        local cols = max(4, floor(TAU * cph / dphi + 0.5))
        ROW_COLS[k] = cols
        ROW_START[k] = NTILES + 1
        for j = 1, cols do
            NTILES = NTILES + 1
            local lam = (j - 0.5) / cols * TAU
            T_CPHI[NTILES] = cph
            T_SPHI[NTILES] = sin(phi)
            T_SLAM[NTILES] = sin(lam)
            T_CLAM[NTILES] = cos(lam)
            T_SEED[NTILES] = random()
        end
    end
end

local NX, NY, NZ = {}, {}, {}                 -- world normal per tile
local TG, TGR, TGG, TGB = {}, {}, {}, {}      -- laser glow per tile
for id = 1, NTILES do
    NX[id], NY[id], NZ[id] = 0, 0, 1
    TG[id], TGR[id], TGG[id], TGB[id] = 0, 0, 0, 0
end

local NSTARS = 70
local STAR_X, STAR_Y, STAR_S = {}, {}, {}
for i = 1, NSTARS do
    STAR_X[i], STAR_Y[i], STAR_S[i] = random(), random(), random()
end

-- Two orbit rings around the ball
local RING_N = 72
local RINGS = {
    { rad = 1.62, tilt = 1.15, roll = 0.35, col = { 120, 200, 255 }, ticks = 12, dir = 1 },
    { rad = 2.10, tilt = 0.55, roll = -0.50, col = { 255, 150, 225 }, ticks = 16, dir = -1 },
}
local RTC, RTS, RZC, RZS = {}, {}, {}, {}
local RPX, RPY, RPZ = {}, {}, {}
for ri = 1, #RINGS do
    RPX[ri], RPY[ri], RPZ[ri] = {}, {}, {}
end

local BALL_DARK = { r = 7, g = 9, b = 16 }

-- State ----------------------------------------------------------------
local game_time = 0.0
local yaw = 0.0
local spin_w = 0.0
local bpm = 120.0
local bpm_next = 0.0
local beat_phase = 0.0
local beats_total = 0.0
local onsets = {}

local beams = {}
local beams_by = {}
local prev_held = {}
local parts = {}

local ch_lo, ch_hi = {}, {}
local ch_idx, ch_on, chan_col = {}, {}, {}
local nch = 0

local amb_r, amb_g, amb_b = 70, 130, 255

local chg, chg_n = {}, 0          -- pooled charge-ups this frame

-- Per-frame values
local W, H, cx, cy, R, TS = 0, 0, 0, 0, 100, 20
local bw = 1.0
local S_MAX, V = 1000, 4000
local CL_L, CL_R, CL_T, CL_B = 0, 0, 0, 0
local pulse = 0.0

local TMP = { r = 0, g = 0, b = 0, a = 1 }
local CIRC = {}
local PP, QQ = {}, {}

-- Helpers --------------------------------------------------------------
local function tmp(r_, g_, b_, a_)
    TMP.r, TMP.g, TMP.b, TMP.a = r_, g_, b_, a_ or 1
    return TMP
end

local function set_hsv(dst, h, s, v)
    local c = v * s
    local x = c * (1 - abs((h / 60) % 2 - 1))
    local m = v - c
    local rr, gg, bb
    if h < 60 then rr, gg, bb = c, x, 0
    elseif h < 120 then rr, gg, bb = x, c, 0
    elseif h < 180 then rr, gg, bb = 0, c, x
    elseif h < 240 then rr, gg, bb = 0, x, c
    elseif h < 300 then rr, gg, bb = x, 0, c
    else rr, gg, bb = c, 0, x end
    dst.r = (rr + m) * 255
    dst.g = (gg + m) * 255
    dst.b = (bb + m) * 255
end

local function circle(ccx, ccy, rad, col, step)
    rad = floor(rad)
    if rad < 1 then return end
    ccx, ccy = floor(ccx + 0.5), floor(ccy + 0.5)
    step = step or 1
    local hw = CIRC[rad]
    if not hw then
        hw = {}
        local rr = (rad + 0.4) * (rad + 0.4)
        for dy = -rad, rad do
            hw[dy] = floor(sqrt(max(0, rr - dy * dy)))
        end
        CIRC[rad] = hw
    end
    for dy = -rad, rad, step do
        local w = hw[dy]
        rect(ccx - w, ccy + dy, ccx + w + 1, ccy + dy + step, col)
    end
end

-- Liang-Barsky: clip the segment to the screen so lines stay short.
local function clip_seg(x0, y0, x1, y1)
    local dx, dy = x1 - x0, y1 - y0
    PP[1], QQ[1] = -dx, x0 - CL_L
    PP[2], QQ[2] = dx, CL_R - x0
    PP[3], QQ[3] = -dy, y0 - CL_T
    PP[4], QQ[4] = dy, CL_B - y0
    local t0, t1 = 0.0, 1.0
    for i = 1, 4 do
        local p, q = PP[i], QQ[i]
        if p == 0 then
            if q < 0 then return nil end
        else
            local t = q / p
            if p < 0 then
                if t > t1 then return nil end
                if t > t0 then t0 = t end
            else
                if t < t0 then return nil end
                if t < t1 then t1 = t end
            end
        end
    end
    return x0 + dx * t0, y0 + dy * t0, x0 + dx * t1, y0 + dy * t1
end

-- A line `2*half` px wide, built from parallel 1px lines.
local function thick(x0, y0, x1, y1, half, step, col)
    local dx, dy = x1 - x0, y1 - y0
    local len = sqrt(dx * dx + dy * dy)
    if len < 0.5 then return end
    local px, py = -dy / len, dx / len
    local n = floor(half / step + 0.5)
    for i = -n, n do
        local k = i * step
        line(x0 + px * k, y0 + py * k, x1 + px * k, y1 + py * k, col)
    end
end

-- Tempo ----------------------------------------------------------------
local PMIN, PMAX, PBIN = 60 / 180, 60 / 70, 0.004
local NB = floor((PMAX - PMIN) / PBIN) + 1
local HIST = {}
local LN2 = math.log(2)

-- Histogram of pairwise onset intervals, folded by octaves into one beat
-- period range, lightly biased toward ~120 BPM.
local function estimate_bpm()
    local n = #onsets
    if n < 8 then return nil end
    for i = 1, NB do HIST[i] = 0 end
    for i = 1, n - 1 do
        local ti = onsets[i]
        for j = i + 1, n do
            local d = onsets[j] - ti
            if d > 3.5 then break end
            if d >= 0.06 then
                local guard = 0
                while d < PMIN and guard < 8 do
                    d = d * 2
                    guard = guard + 1
                end
                while d > PMAX and guard < 16 do
                    d = d * 0.5
                    guard = guard + 1
                end
                local b = floor((d - PMIN) / PBIN) + 1
                if b >= 1 and b <= NB then HIST[b] = HIST[b] + 1 end
            end
        end
    end
    local best, bi = 0, nil
    for i = 1, NB do
        local s = 0
        for k = -2, 2 do
            local v = HIST[i + k]
            if v then s = s + v * (3 - abs(k)) end
        end
        local period = PMIN + (i - 0.5) * PBIN
        local oct = math.log(60 / period / 120) / LN2
        s = s * exp(-(oct * oct) / 0.5)
        if s > best then
            best, bi = s, i
        end
    end
    if bi and best >= 8 then
        return 60 / (PMIN + (bi - 0.5) * PBIN)
    end
    return nil
end

-- Emitters -------------------------------------------------------------
-- Channel picks a wedge of longitude, pitch picks the latitude row.
local function emitter_tile(ch, key)
    local ci = ch_idx[ch]
    local l, h = ch_lo[ch] or key, ch_hi[ch] or key
    if h - l < 12 then
        local mid = (l + h) / 2
        l, h = mid - 6, mid + 6
    end
    local t = (key - l) / (h - l)
    if t < 0 then t = 0 elseif t > 1 then t = 1 end
    local row = 2 + floor(t * (BANDS - 3) + 0.5)

    local n = max(1, nch)
    local sect = (ci - 1) / n * TAU
    local half = pi / n * 0.85
    local lam = (sect + ((key % 12) / 11 - 0.5) * 2 * half) % TAU
    local cols = ROW_COLS[row]
    local col = floor(lam / TAU * cols) + 1
    if col > cols then col = cols end
    return ROW_START[row] + col - 1
end

local function spawn_sparks(b)
    if b.nz < 0 then return end
    local px, py = cx + R * b.nx, cy - R * b.ny
    local c = b.cc.mid
    for _ = 1, 3 do
        if #parts >= MAX_PARTS then return end
        local sp = (0.35 + random() * 0.6) * H
        local jx, jy = (random() - 0.5) * 0.9, (random() - 0.5) * 0.9
        local dx, dy = b.nx + jx, -b.ny + jy
        local ln = sqrt(dx * dx + dy * dy)
        if ln < 1e-3 then ln = 1 end
        parts[#parts + 1] = {
            x = px, y = py,
            vx = dx / ln * sp, vy = dy / ln * sp,
            born = game_time, life = 0.22 + random() * 0.2,
            col = { r = c.r, g = c.g, b = c.b, a = 1 },
        }
    end
end

local function drop_oldest()
    local oi, ot = 1, beams[1].t_on
    for i = 2, #beams do
        if beams[i].t_on < ot then
            oi, ot = i, beams[i].t_on
        end
    end
    beams[oi] = beams[#beams]
    beams[#beams] = nil
end

-- Drawing --------------------------------------------------------------
local function draw_background()
    clear({ r = 4, g = 5, b = 12 })
    local bands = 10
    for i = 0, bands - 1 do
        local f = i / (bands - 1)
        rect(0, floor(i * H / bands), W, floor((i + 1) * H / bands) + 1,
            tmp(4 + 9 * f, 5 + 3 * f, 12 + 12 * f, 1))
    end
    for i = 1, NSTARS do
        local tw = 0.5 + 0.5 * sin(game_time * (0.6 + STAR_S[i]) + i * 7.3)
        local s = (STAR_S[i] > 0.85) and 2 or 1
        local x, y = floor(STAR_X[i] * W), floor(STAR_Y[i] * H)
        rect(x, y, x + s, y + s, tmp(200, 215, 255, 0.12 + 0.5 * tw))
    end
end

local function draw_halo(act)
    local a = 0.045 + min(0.07, act * 0.01) + 0.03 * pulse
    circle(cx, cy, R * 2.5, tmp(amb_r, amb_g, amb_b, a * 0.8), 4)
    circle(cx, cy, R * 1.8, tmp(amb_r, amb_g, amb_b, a), 4)
    circle(cx, cy, R * 1.3, tmp(amb_r, amb_g, amb_b, a * 1.2), 3)
end

local function draw_cable(cT)
    local y1 = cy - R * cT * 0.98
    line(cx, 0, cx, y1, tmp(150, 160, 185, 0.6))
    line(cx + 1, 0, cx + 1, y1, tmp(60, 66, 85, 0.6))
    rect(cx - 6, 0, cx + 7, 5, tmp(90, 98, 120, 1))
end

local function ring_point(ri, ang, rad)
    local x0, z0 = cos(ang), sin(ang)
    local y1 = -z0 * RTS[ri]
    local z1 = z0 * RTC[ri]
    local x2 = x0 * RZC[ri] - y1 * RZS[ri]
    local y2 = x0 * RZS[ri] + y1 * RZC[ri]
    return cx + x2 * rad, cy - y2 * rad, z1
end

local function prepare_rings()
    for ri = 1, #RINGS do
        local rg = RINGS[ri]
        local tl = rg.tilt + 0.12 * sin(game_time * 0.4 + ri * 2.0)
        local rl = rg.roll + 0.10 * sin(game_time * 0.3 + ri)
        RTC[ri], RTS[ri] = cos(tl), sin(tl)
        RZC[ri], RZS[ri] = cos(rl), sin(rl)
        local rad = R * rg.rad
        for i = 0, RING_N do
            RPX[ri][i], RPY[ri][i], RPZ[ri][i] = ring_point(ri, i / RING_N * TAU, rad)
        end
    end
end

local function draw_rings(front)
    for ri = 1, #RINGS do
        local rg = RINGS[ri]
        local col = rg.col
        local px, py, pz = RPX[ri], RPY[ri], RPZ[ri]
        local a = front and (0.55 + 0.35 * pulse) or 0.2
        local c = tmp(col[1] + (255 - col[1]) * pulse * 0.4,
            col[2] + (255 - col[2]) * pulse * 0.4,
            col[3] + (255 - col[3]) * pulse * 0.4, a)
        for i = 0, RING_N - 1 do
            local isf = (pz[i] + pz[i + 1]) >= 0
            if isf == front then
                line(px[i], py[i], px[i + 1], py[i + 1], c)
                if front then line(px[i], py[i] + 1, px[i + 1], py[i + 1] + 1, c) end
            end
        end

        -- Ticks advance one step per beat so the ring shows the tempo.
        local rad = R * rg.rad
        local off = beats_total * TAU / rg.ticks * rg.dir
        local tc = tmp(255, 255, 255, front and 0.8 or 0.3)
        for k = 0, rg.ticks - 1 do
            local ang = off + k * TAU / rg.ticks
            local x0, y0 = ring_point(ri, ang, rad * 0.93)
            local x1, y1, z1 = ring_point(ri, ang, rad * 1.08)
            if (z1 >= 0) == front then
                line(x0, y0, x1, y1, tc)
            end
        end
    end
end

local function draw_ball()
    circle(cx, cy, R, BALL_DARK, 2)

    local gap = max(0.6, TS * 0.07)
    for id = 1, NTILES do
        local nz = NZ[id]
        if nz > 0.02 then
            local nx, ny = NX[id], NY[id]

            local lam = nx * LX + ny * LY + nz * LZ
            if lam < 0 then lam = 0 end
            local v = (22 + 170 * lam ^ 1.5) * (0.45 + 0.55 * nz) * (1 + 0.3 * pulse)

            local seed = T_SEED[id]
            local sp = sin(game_time * (2.0 + seed * 3.0) + seed * 97) * 0.5 + 0.5
            sp = sp * sp
            sp = sp * sp
            sp = sp * sp

            local r_ = v * 0.78 + amb_r * 0.10
            local g_ = v * 0.90 + amb_g * 0.10
            local b_ = v + amb_b * 0.10

            local gi = TG[id]
            if gi > 0 then
                r_ = r_ + (TGR[id] - r_) * gi
                g_ = g_ + (TGG[id] - g_) * gi
                b_ = b_ + (TGB[id] - b_) * gi
                r_ = r_ + (255 - r_) * gi * 0.25
                g_ = g_ + (255 - g_) * gi * 0.25
                b_ = b_ + (255 - b_) * gi * 0.25
            end
            local sk = sp * 0.85
            r_ = r_ + (255 - r_) * sk
            g_ = g_ + (255 - g_) * sk
            b_ = b_ + (255 - b_) * sk

            local px, py = cx + R * nx, cy - R * ny
            local rxy = sqrt(nx * nx + ny * ny)
            local ax, ay = 1, 0
            if rxy > 1e-3 then ax, ay = nx / rxy, ny / rxy end
            local ew = TS * sqrt((nz * ax) ^ 2 + ay * ay)
            local eh = TS * sqrt((nz * ay) ^ 2 + ax * ax)
            local hw, hh = ew * 0.5 - gap, eh * 0.5 - gap
            if hw > 0.4 and hh > 0.4 then
                local x0, y0 = floor(px - hw + 0.5), floor(py - hh + 0.5)
                local x1, y1 = floor(px + hw + 0.5), floor(py + hh + 0.5)
                if x1 <= x0 then x1 = x0 + 1 end
                if y1 <= y0 then y1 = y0 + 1 end
                rect(x0, y0, x1, y1, tmp(r_, g_, b_, 1))
                if x1 - x0 > 5 then
                    rect(x0, y0, x0 + (x1 - x0) * 0.5, y0 + (y1 - y0) * 0.45,
                        tmp(255, 255, 255, 0.08 + 0.25 * lam))
                end
            end
        end
    end

    -- Rim light
    local rc = tmp(amb_r * 0.5 + 100, amb_g * 0.5 + 110, amb_b * 0.5 + 120, 0.45 + 0.3 * pulse)
    local px, py = cx + (R + 0.5), cy
    for i = 1, 48 do
        local a = i / 48 * TAU
        local qx, qy = cx + cos(a) * (R + 0.5), cy + sin(a) * (R + 0.5)
        line(px, py, qx, qy, rc)
        px, py = qx, qy
    end
end

local function draw_beam(b, lod, front)
    local x0, y0, x1, y1 = clip_seg(b.x0, b.y0, b.x1, b.y1)
    if not x0 then return end
    local cc = b.cc
    local depth = b.nz
    local fade = (depth < 0) and 0.55 or 1.0
    local wsc = bw * (0.85 + 0.25 * max(0, depth))
    local mid, core = cc.mid, cc.core

    if lod >= 4 then
        local c = tmp(mid.r, mid.g, mid.b, 0.07 * fade)
        thick(x0, y0, x1, y1, 9 * wsc, 1.8, c)
    end
    if lod >= 3 then
        local c = tmp(mid.r, mid.g, mid.b, 0.16 * fade)
        thick(x0, y0, x1, y1, 5 * wsc, 1.2, c)
    end
    thick(x0, y0, x1, y1, 2.2 * wsc, 0.9, tmp(mid.r, mid.g, mid.b, 0.4 * fade))
    thick(x0, y0, x1, y1, 0.6 * wsc, 0.6, tmp(core.r, core.g, core.b, fade))

    -- Bright head while the light is still traveling
    if b.s1 < S_MAX - 1 and b.x1 > -20 and b.x1 < W + 20 and b.y1 > -20 and b.y1 < H + 20 then
        local hr = max(2, 4 * wsc)
        circle(b.x1, b.y1, hr * 2.2, tmp(mid.r, mid.g, mid.b, 0.22 * fade), 2)
        circle(b.x1, b.y1, hr, tmp(core.r, core.g, core.b, 0.95 * fade), 1)
    end

    -- Muzzle flash on the tile it just left
    if front and b.age < FLASH_T and depth > 0 then
        local k = b.age / FLASH_T
        circle(cx + R * b.nx, cy - R * b.ny, TS * (0.5 + 0.7 * (1 - k)),
            tmp(core.r, core.g, core.b, 0.75 * (1 - k)), 2)
    end
end

local function draw_beams(front, lod)
    for i = 1, #beams do
        local b = beams[i]
        if (b.nz >= 0) == front then
            draw_beam(b, lod, front)
        end
    end
end

local function draw_charges()
    for i = 1, chg_n do
        local c = chg[i]
        if NZ[c.tid] > 0 then
            local k = c.tau / CHARGE_T
            local col = chan_col[c.ch].mid
            circle(cx + R * NX[c.tid], cy - R * NY[c.tid], TS * (0.35 + 0.9 * k),
                tmp(col.r, col.g, col.b, 0.35 * (1 - k)), 2)
        end
    end
end

local function draw_particles()
    for i = #parts, 1, -1 do
        local p = parts[i]
        local age = game_time - p.born
        if age < 0 or age >= p.life then
            parts[i] = parts[#parts]
            parts[#parts] = nil
        else
            local k = age / p.life
            local s = max(1, floor(bw * 3 * (1 - 0.5 * k)))
            local x = floor(p.x + p.vx * age)
            local y = floor(p.y + p.vy * age)
            p.col.a = 1 - k
            rect(x, y, x + s, y + s, p.col)
        end
    end
end

-- Frame ----------------------------------------------------------------
function render(width, height, left, right)
    W, H = width, height
    cx, cy = W / 2, H / 2
    R = min(W, H) * 0.20
    TS = R * pi / BANDS
    bw = max(0.6, H / 720)
    local lmax = sqrt(cx * cx + cy * cy)
    S_MAX = lmax / 0.55
    V = S_MAX / T_TRAVEL
    CL_L, CL_R, CL_T, CL_B = -24, W + 24, -24, H + 24

    local dt = 0
    if SAMPLE_RATE and SAMPLE_RATE > 0 then
        dt = min(0.1, #left / SAMPLE_RATE)
    end
    game_time = game_time + dt

    -- Channels: index (file order) picks the hue, blue -> red
    local channels = midi_channels()
    nch = #channels
    for i, ch in ipairs(channels) do
        ch_idx[ch] = i
        ch_on[ch] = channel_enabled(ch) and true or false
        local cc = chan_col[ch]
        if not cc then
            cc = {
                mid = { r = 0, g = 0, b = 0 },
                core = { r = 0, g = 0, b = 0 },
            }
            chan_col[ch] = cc
        end
        local t = (nch > 1) and ((i - 1) / (nch - 1)) or 0.0
        set_hsv(cc.mid, 240 * (1 - t), 0.88, 1.0)
        cc.core.r = cc.mid.r + (255 - cc.mid.r) * 0.72
        cc.core.g = cc.mid.g + (255 - cc.mid.g) * 0.72
        cc.core.b = cc.mid.b + (255 - cc.mid.b) * 0.72
    end

    local active = active_notes()
    local upcoming = upcoming_notes()

    -- Pitch range per channel; upcoming note-offs; charge-ups
    local offs = {}
    chg_n = 0
    for _, ev in ipairs(upcoming) do
        local ch, key, tau = ev.channel, ev.key, ev.seconds_until
        if ch ~= nil and key and tau and ch_on[ch] then
            if ev.on then
                if ch_lo[ch] == nil or key < ch_lo[ch] then ch_lo[ch] = key end
                if ch_hi[ch] == nil or key > ch_hi[ch] then ch_hi[ch] = key end
                if tau <= CHARGE_T then
                    chg_n = chg_n + 1
                    local c = chg[chg_n]
                    if not c then
                        c = {}
                        chg[chg_n] = c
                    end
                    c.ch, c.key, c.tau = ch, key, max(0, tau)
                end
            else
                local row = offs[ch]
                if not row then
                    row = {}
                    offs[ch] = row
                end
                if row[key] == nil or tau < row[key] then row[key] = tau end
            end
        end
    end
    for _, n in ipairs(active) do
        local ch, key = n.channel, n.key
        if ch ~= nil and key and ch_on[ch] then
            if ch_lo[ch] == nil or key < ch_lo[ch] then ch_lo[ch] = key end
            if ch_hi[ch] == nil or key > ch_hi[ch] then ch_hi[ch] = key end
        end
    end

    -- Tempo -> spin speed and beat pulse
    if FIXED_BPM then
        bpm = FIXED_BPM
    elseif game_time >= bpm_next then
        bpm_next = game_time + 0.5
        while #onsets > 0 and onsets[1] < game_time - 10 do
            table.remove(onsets, 1)
        end
        local est = estimate_bpm()
        if est then bpm = bpm + (est - bpm) * 0.3 end
    end
    local target_w = bpm / 60 * TAU / BEATS_PER_REV
    spin_w = spin_w + (target_w - spin_w) * (1 - exp(-dt * 3))
    yaw = yaw + spin_w * dt
    beats_total = beats_total + dt * bpm / 60
    beat_phase = (beat_phase + dt * bpm / 60) % 1

    -- Ball orientation: world normal for every tile
    local cY, sY = cos(yaw), sin(yaw)
    local tilt = TILT + 0.05 * sin(game_time * 0.5)
    local cT, sT = cos(tilt), sin(tilt)
    for id = 1, NTILES do
        local cph = T_CPHI[id]
        local x = cph * (T_SLAM[id] * cY + T_CLAM[id] * sY)
        local z = cph * (T_CLAM[id] * cY - T_SLAM[id] * sY)
        local y = T_SPHI[id]
        NX[id] = x
        NY[id] = y * cT - z * sT
        NZ[id] = y * sT + z * cT
        TG[id] = 0
    end

    -- New notes -> new beams; ended notes -> detach the tail
    local cur = {}
    local err_sum, err_n = 0, 0
    for _, note in ipairs(active) do
        local ch, key = note.channel, note.key
        if ch ~= nil and key and ch_on[ch] then
            local row = cur[ch]
            if not row then
                row = {}
                cur[ch] = row
            end
            row[key] = true
            local p = prev_held[ch]
            if not (p and p[key]) then
                if #beams >= MAX_BEAMS then drop_oldest() end
                local tid = emitter_tile(ch, key)
                local b = {
                    ch = ch, key = key, tid = tid, cc = chan_col[ch],
                    t_on = game_time - dt * 0.5, t_off = nil,
                    nx = NX[tid], ny = NY[tid], nz = NZ[tid],
                    s0 = 0, s1 = 0, age = 0,
                    x0 = 0, y0 = 0, x1 = 0, y1 = 0,
                }
                beams[#beams + 1] = b
                local brow = beams_by[ch]
                if not brow then
                    brow = {}
                    beams_by[ch] = brow
                end
                brow[key] = b
                spawn_sparks(b)

                onsets[#onsets + 1] = game_time
                if #onsets > 64 then table.remove(onsets, 1) end
                local e = ((beat_phase + 0.5) % 1) - 0.5
                if abs(e) < 0.2 then
                    err_sum = err_sum + e
                    err_n = err_n + 1
                end
            end
        end
    end
    if err_n > 0 then
        beat_phase = (beat_phase - (err_sum / err_n) * 0.12) % 1
    end
    pulse = exp(-beat_phase * 6)

    for ch, prow in pairs(prev_held) do
        local crow = cur[ch]
        local brow = beams_by[ch]
        for key in pairs(prow) do
            if not (crow and crow[key]) and brow then
                local b = brow[key]
                if b then
                    if not b.t_off or b.t_off > game_time then b.t_off = game_time end
                    brow[key] = nil
                end
            end
        end
    end
    prev_held = cur

    -- Held beams learn their exact end time from the upcoming note-off
    for ch, brow in pairs(beams_by) do
        local orow = offs[ch]
        if orow then
            for key, b in pairs(brow) do
                local tau = orow[key]
                if tau then b.t_off = game_time + tau end
            end
        end
    end

    -- Charge-up glow on tiles about to fire
    for i = 1, chg_n do
        local c = chg[i]
        c.tid = emitter_tile(c.ch, c.key)
        local g = 0.6 * (1 - c.tau / CHARGE_T)
        if g > TG[c.tid] then
            local col = chan_col[c.ch].mid
            TG[c.tid] = g
            TGR[c.tid], TGG[c.tid], TGB[c.tid] = col.r, col.g, col.b
        end
    end

    -- Beam geometry, tile glow, ambient color
    local ar, ag, ab, aw = 0, 0, 0, 0
    for i = #beams, 1, -1 do
        local b = beams[i]
        local age = game_time - b.t_on
        local s1 = V * age
        if s1 > S_MAX then s1 = S_MAX end
        local s0 = 0
        if b.t_off then
            s0 = V * (game_time - b.t_off)
            if s0 < 0 then s0 = 0 end
        end
        if s0 >= S_MAX then
            beams[i] = beams[#beams]
            beams[#beams] = nil
        else
            local tid = b.tid
            local nx, ny, nz = NX[tid], NY[tid], NZ[tid]
            b.nx, b.ny, b.nz = nx, ny, nz
            b.s0, b.s1, b.age = s0, s1, age
            local d0, d1 = R + s0, R + s1
            b.x0, b.y0 = cx + nx * d0, cy - ny * d0
            b.x1, b.y1 = cx + nx * d1, cy - ny * d1

            local held = (not b.t_off) or b.t_off > game_time
            local g = 1.0
            if not held then g = max(0, 1 - (game_time - b.t_off) / 0.25) end
            local mid = b.cc.mid
            if g > TG[tid] then
                TG[tid] = g
                TGR[tid], TGG[tid], TGB[tid] = mid.r, mid.g, mid.b
            end
            local w = held and 1 or 0.25
            ar, ag, ab, aw = ar + mid.r * w, ag + mid.g * w, ab + mid.b * w, aw + w
        end
    end

    local ka = 1 - exp(-dt * 6)
    if aw > 0 then
        amb_r = amb_r + (ar / aw - amb_r) * ka
        amb_g = amb_g + (ag / aw - amb_g) * ka
        amb_b = amb_b + (ab / aw - amb_b) * ka
    else
        amb_r = amb_r + (70 - amb_r) * ka
        amb_g = amb_g + (130 - amb_g) * ka
        amb_b = amb_b + (255 - amb_b) * ka
    end

    -- Fewer glow layers when there are lots of beams
    local nb = #beams
    local lod = 4
    if nb > 80 then lod = 2 elseif nb > 40 then lod = 3 end

    -- Draw: back to front
    prepare_rings()
    draw_background()
    draw_halo(aw)
    draw_cable(cT)
    draw_rings(false)
    draw_beams(false, lod)
    draw_ball()
    draw_beams(true, lod)
    draw_charges()
    draw_rings(true)
    draw_particles()
end