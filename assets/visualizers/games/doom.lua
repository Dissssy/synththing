-- Doom: plays the first maps of a Doom WAD you supply, in pure Lua.
--
-- Put a WAD in this script's assets folder (the folder button above the
-- visualizer opens it): the shareware DOOM1.WAD, DOOM.WAD, or Freedoom's
-- freedoom1.wad. Then restart the script (F5) and click into the
-- visualizer to play: WASD to move, the arrows or the mouse to turn, E or
-- Space to open doors and flip switches (all rebindable in Controls). The
-- map's music plays on the loaded soundfont.
--
-- A software renderer like the original: BSP front to back, textured
-- walls, floors, ceilings and sky with Doom's light diminishing, and
-- things drawn as sprites. Doors, lifts, moving floors, switches and
-- exits work; monsters stand still and there's no shooting.

local floor, min, max, abs = math.floor, math.min, math.max, math.abs
local sin, cos, atan, pi = math.sin, math.cos, math.atan, math.pi
local sunpack, ssub, sbyte = string.unpack, string.sub, string.byte

-- The view is RW columns (320, or 160 in low detail, Doom's own
-- double-wide pixels) by VIEWH rows, over a 320x32 status bar. PROJX scales
-- across, PROJ up and down; they differ in low detail.
local W, H, VIEWH, BARH = 320, 200, 168, 32
local RW, CX, CY, PROJX, PROJ = W, W / 2, VIEWH / 2, W / 2, W / 2
local NEAR = 4
local TIC = 1 / 35

script_options({ assets = true })

local FORWARD = input_register("forward", { "w", "up", "pad_lstick_up", "pad_dpad_up" }, { group = "Move" })
local BACK = input_register("back", { "s", "down", "pad_lstick_down", "pad_dpad_down" }, { group = "Move" })
local STRAFE_L = input_register("strafe left", { "a", "pad_lstick_left" }, { group = "Move" })
local STRAFE_R = input_register("strafe right", { "d", "pad_lstick_right" }, { group = "Move" })
local TURN_L = input_register("turn left", { "left", "pad_rstick_left", "pad_dpad_left" }, { group = "Move" })
local TURN_R = input_register("turn right", { "right", "pad_rstick_right", "pad_dpad_right" }, { group = "Move" })
local WALK = input_register("walk", { "pad_lb" }, { group = "Move", info = "Hold to walk instead of run." })
local USE = input_register("use", { "e", "space", "pad_a" }, { group = "Act", info = "Open doors, flip switches." })

---------------------------------------------------------------- WAD

-- Every WAD in the assets folder, to pick from in Settings.
local wads = {}
for _, name in ipairs(list_assets()) do
    if name:lower():match("%.wad$") then wads[#wads + 1] = name end
end
local WAD_INFO = { info = "The WAD to play, from this script's assets folder. Press F5 after changing it." }
local function chosen_wad()
    if #wads == 0 then return nil end
    -- Doom or Freedoom before anything else (a PWAD alone has no graphics).
    local default = wads[1]
    for _, name in ipairs(wads) do
        if name:lower():match("^doom") or name:lower():match("^freedoom") then default = name break end
    end
    return setting_selection("WAD", wads, { default }, 1, WAD_INFO)[1]
end
local wad_name = chosen_wad()
local wad = wad_name and read_asset(wad_name)

local lumps, by_name = {}, {}
local function lump_name(s) return (s:gsub("%z.*", "")):upper() end
if wad then
    local count, dir = sunpack("<i4i4", wad, 5)
    for i = 0, count - 1 do
        local pos, size, name = sunpack("<i4i4c8", wad, dir + 1 + i * 16)
        name = lump_name(name)
        lumps[i + 1] = { name = name, pos = pos, size = size }
        by_name[name] = i + 1
    end
end
local function lump_data(i) local l = lumps[i] return ssub(wad, l.pos + 1, l.pos + l.size) end
local function lump(name) local i = by_name[name] return i and lump_data(i) end
-- Lumps between two markers, by name (later ones win, like Doom).
local function namespace(starts, ends, into)
    local inside = false
    for i, l in ipairs(lumps) do
        if starts[l.name] then inside = true
        elseif ends[l.name] then inside = false
        elseif inside and l.size > 0 then into[l.name] = i end
    end
    return into
end

-- Palette and the 34 colormaps. Pixel values everywhere below are palette
-- index + 1, so a colormap table maps them straight to a palette index.
local palette, cmaps = {}, {}
if wad then
    local pal = lump("PLAYPAL")
    for i = 0, 255 do
        local r, g, b = sbyte(pal, i * 3 + 1, i * 3 + 3)
        palette[i + 1] = { r = r, g = g, b = b }
    end
    local cm = lump("COLORMAP")
    for level = 0, 33 do
        local t = {}
        for i = 1, 256 do t[i] = sbyte(cm, level * 256 + i) end
        cmaps[level] = t
    end
end

-- Light tables as in r_main.c: walls and sprites by scale, planes by distance.
local scalelight, zlight = {}, {}
for l = 0, 15 do
    local startmap = (15 - l) * 4
    scalelight[l], zlight[l] = {}, {}
    for j = 0, 47 do
        scalelight[l][j] = cmaps[min(31, max(0, floor(startmap - j / 2)))]
    end
    for i = 0, 127 do
        zlight[l][i] = cmaps[min(31, max(0, floor(startmap - 80 / (i + 1))))]
    end
end

-- Pictures (patches): columns of posts, transparent where there are none.
local patch_cache = {}
local function patch(name)
    local p = patch_cache[name]
    if p ~= nil then return p end
    local data = name and lump(name)
    if not data then patch_cache[name] = false return false end
    local w, h, lo, to = sunpack("<i2i2i2i2", data, 1)
    local cols = {}
    for x = 0, w - 1 do
        local col = {}
        local o = sunpack("<I4", data, 9 + x * 4) + 1
        local top = sbyte(data, o)
        local last = -1
        while top and top ~= 255 do
            -- Tall patches: a delta not past the last post adds to it.
            if top <= last then top = last + top end
            local len = sbyte(data, o + 1)
            for i = 0, len - 1 do col[top + i + 1] = sbyte(data, o + 3 + i) + 1 end
            last = top
            o = o + len + 4
            top = sbyte(data, o)
        end
        cols[x + 1] = col
    end
    p = { w = w, h = h, lo = lo, to = to, cols = cols }
    patch_cache[name] = p
    return p
end

-- Wall textures, composited from patches the first time they're drawn.
local texdefs, textures = {}, {}
if wad then
    local pnames = {}
    local pn = lump("PNAMES")
    for i = 0, sunpack("<i4", pn, 1) - 1 do pnames[i] = lump_name(ssub(pn, 5 + i * 8, 12 + i * 8)) end
    for _, tl in ipairs({ "TEXTURE1", "TEXTURE2" }) do
        local d = lump(tl)
        if d then
            for i = 0, sunpack("<i4", d, 1) - 1 do
                local o = sunpack("<i4", d, 5 + i * 4) + 1
                local name, _, w, h, _, n = sunpack("<c8i4i2i2i4i2", d, o)
                local parts = {}
                for k = 0, n - 1 do
                    local px, py, pi_ = sunpack("<i2i2i2", d, o + 22 + k * 10)
                    parts[#parts + 1] = { x = px, y = py, name = pnames[pi_] }
                end
                texdefs[lump_name(name)] = { w = w, h = h, parts = parts }
            end
        end
    end
end
local function texture(name)
    local t = textures[name]
    if t ~= nil then return t end
    local def = texdefs[name]
    if not def then textures[name] = false return false end
    local cols = {}
    for x = 1, def.w do
        local c = {}
        for y = 1, def.h do c[y] = 1 end
        cols[x] = c
    end
    for _, part in ipairs(def.parts) do
        local p = patch(part.name)
        if p then
            for px = 1, p.w do
                local c = cols[part.x + px]
                if c then
                    for py, v in pairs(p.cols[px]) do
                        local y = part.y + py
                        if y >= 1 and y <= def.h then c[y] = v end
                    end
                end
            end
        end
    end
    t = { w = def.w, h = def.h, cols = cols }
    textures[name] = t
    return t
end

-- Flats: 64x64 raw, kept as flat lists of pixel values.
local flat_lumps = wad and namespace({ F_START = true, FF_START = true }, { F_END = true, FF_END = true }, {}) or {}
local flats = {}
local function flat(name)
    local f = flats[name]
    if f ~= nil then return f end
    local i = flat_lumps[name]
    if not i then flats[name] = false return false end
    local d = lump_data(i)
    f = {}
    for k = 1, 4096 do f[k] = (sbyte(d, k) or 0) + 1 end
    flats[name] = f
    return f
end
local FLAT_ANIMS = {
    { "NUKAGE1", "NUKAGE2", "NUKAGE3" }, { "FWATER1", "FWATER2", "FWATER3", "FWATER4" },
    { "LAVA1", "LAVA2", "LAVA3", "LAVA4" }, { "BLOOD1", "BLOOD2", "BLOOD3" },
    { "SLIME01", "SLIME02", "SLIME03", "SLIME04" }, { "SLIME05", "SLIME06", "SLIME07", "SLIME08" },
}
local flat_now = {} -- flat name -> the frame it shows this tic

local sprite_lumps = wad and namespace({ S_START = true, SS_START = true }, { S_END = true, SS_END = true }, {}) or {}
local sprite_frames = {}
for name in pairs(sprite_lumps) do
    sprite_frames[name:sub(1, 6)] = { name = name, flip = false }
    if #name == 8 then sprite_frames[name:sub(1, 4) .. name:sub(7, 8)] = { name = name, flip = true } end
end

---------------------------------------------------------------- music

-- Events as parallel lists: time in seconds, status, data1, data2.
local music = { t = {}, st = {}, d1 = {}, d2 = {}, n = 0, pos = 1, time = 0 }

local function music_reset()
    music.t, music.st, music.d1, music.d2, music.n, music.pos, music.time = {}, {}, {}, {}, 0, 1, 0
end
local function music_add(time, st, d1, d2)
    local n = music.n + 1
    music.n = n
    music.t[n], music.st[n], music.d1[n], music.d2[n] = time, st, d1, d2
end

local MUS_CTRL = { [1] = 0, [2] = 1, [3] = 7, [4] = 10, [5] = 11, [6] = 91, [7] = 93, [8] = 64, [9] = 67 }
local MUS_SYS = { [10] = 120, [11] = 123, [12] = 126, [13] = 127, [14] = 121 }

local function load_mus(d)
    local start = sunpack("<I2", d, 7) + 1
    local o, tick, vol = start, 0, {}
    local function ch(c)
        if c == 15 then return 9 elseif c >= 9 then return c + 1 end
        return c
    end
    while o <= #d do
        local ev = sbyte(d, o)
        o = o + 1
        local kind, c = (ev >> 4) & 7, ch(ev & 15)
        local t = tick / 140
        if kind == 0 then
            music_add(t, 0x80 | c, sbyte(d, o) & 127, 0)
            o = o + 1
        elseif kind == 1 then
            local note = sbyte(d, o)
            o = o + 1
            if note & 128 ~= 0 then vol[c] = sbyte(d, o) & 127 o = o + 1 end
            music_add(t, 0x90 | c, note & 127, vol[c] or 100)
        elseif kind == 2 then
            local bend = sbyte(d, o) * 64
            o = o + 1
            music_add(t, 0xE0 | c, bend & 127, bend >> 7)
        elseif kind == 3 then
            local cc = MUS_SYS[sbyte(d, o)]
            o = o + 1
            if cc then music_add(t, 0xB0 | c, cc, 0) end
        elseif kind == 4 then
            local ctrl, value = sbyte(d, o, o + 1)
            o = o + 2
            if ctrl == 0 then music_add(t, 0xC0 | c, value & 127, 0)
            elseif MUS_CTRL[ctrl] then music_add(t, 0xB0 | c, MUS_CTRL[ctrl], min(value, 127)) end
        elseif kind == 6 then
            break
        end
        if ev & 128 ~= 0 then
            local delay = 0
            repeat
                local b = sbyte(d, o)
                o = o + 1
                delay = delay * 128 + (b & 127)
            until b < 128
            tick = tick + delay
        end
    end
    music.length = tick / 140
end

local function load_midi(d)
    local _, ntracks, div = sunpack(">I2I2I2", d, 9)
    local events, tempos = {}, { { 0, 500000 } }
    local o = 15
    for _ = 1, ntracks do
        local _, len = sunpack(">c4I4", d, o)
        local p, stop, tick, running = o + 8, o + 8 + len, 0, 0
        o = stop
        while p < stop do
            local delta = 0
            repeat
                local b = sbyte(d, p)
                p = p + 1
                delta = delta * 128 + (b & 127)
            until b < 128
            tick = tick + delta
            local st = sbyte(d, p)
            if st >= 0x80 then p = p + 1 else st = running end
            if st == 0xFF then
                local kind = sbyte(d, p)
                local mlen = 0
                p = p + 1
                repeat
                    local b = sbyte(d, p)
                    p = p + 1
                    mlen = mlen * 128 + (b & 127)
                until b < 128
                if kind == 0x51 then tempos[#tempos + 1] = { tick, sunpack(">I3", d, p) } end
                if kind == 0x2F then break end
                p = p + mlen
            elseif st == 0xF0 or st == 0xF7 then
                local mlen = 0
                repeat
                    local b = sbyte(d, p)
                    p = p + 1
                    mlen = mlen * 128 + (b & 127)
                until b < 128
                p = p + mlen
            else
                running = st
                local hi = st & 0xF0
                local d1, d2 = sbyte(d, p), 0
                if hi == 0xC0 or hi == 0xD0 then p = p + 1 else d2 = sbyte(d, p + 1) p = p + 2 end
                events[#events + 1] = { tick, #events, st, d1, d2 }
            end
        end
    end
    table.sort(events, function(a, b) if a[1] ~= b[1] then return a[1] < b[1] end return a[2] < b[2] end)
    table.sort(tempos, function(a, b) return a[1] < b[1] end)
    local ti, last_tick, seconds, tempo = 1, 0, 0, 500000
    local function time_at(tick)
        while tempos[ti + 1] and tempos[ti + 1][1] <= tick do
            ti = ti + 1
            seconds = seconds + (tempos[ti][1] - last_tick) * tempo / div / 1e6
            last_tick, tempo = tempos[ti][1], tempos[ti][2]
        end
        return seconds + (tick - last_tick) * tempo / div / 1e6
    end
    for _, e in ipairs(events) do music_add(time_at(e[1]), e[3], e[4] & 127, e[5] & 127) end
    music.length = music.n > 0 and music.t[music.n] or 0
end

local function music_start(map_name)
    for c = 0, 15 do midi_send(0xB0 | c, 123, 0) midi_send(0xB0 | c, 121, 0) end
    music_reset()
    local name = map_name:match("^E(%d)M(%d)$") and ("D_" .. map_name) or nil
    if not name then
        -- Doom II names its music by song, in map order.
        local songs = { "RUNNIN", "STALKS", "COUNTD", "BETWEE", "DOOM", "THE_DA", "SHAWN", "DDTBLU", "IN_CIT", "DEAD" }
        name = "D_" .. (songs[tonumber(map_name:match("%d+")) or 1] or "RUNNIN")
    end
    local d = lump(name)
    if not d then return end
    if ssub(d, 1, 4) == "MUS\26" then load_mus(d) elseif ssub(d, 1, 4) == "MThd" then load_midi(d) end
end

local function music_tick(dt)
    if music.n == 0 then return end
    music.time = music.time + dt
    local t, n = music.t, music.n
    while music.pos <= n and t[music.pos] <= music.time do
        local i = music.pos
        midi_send(music.st[i], music.d1[i], music.d2[i])
        music.pos = i + 1
    end
    if music.pos > n then
        for c = 0, 15 do midi_send(0xB0 | c, 123, 0) end
        music.pos, music.time = 1, 0
    end
end

---------------------------------------------------------------- the map

local map -- vertexes, lines, sides, sectors, segs, subsectors, nodes, things
local player = { x = 0, y = 0, z = 0, angle = 0, momx = 0, momy = 0, momz = 0, viewh = 41, dvh = 0, bob = 0 }
local map_name, message, message_until, leveltime = nil, nil, 0, 0

local function point_in_subsector(x, y)
    local nodes = map.nodes
    local n = map.node_count - 1
    while true do
        local node = nodes[n]
        local side = ((y - node.y) * node.dx - (x - node.x) * node.dy >= 0) and 1 or 0
        local child = node.child[side]
        if child & 0x8000 ~= 0 then return map.subsectors[child & 0x7FFF] end
        n = child
    end
end
local function sector_at(x, y) return point_in_subsector(x, y).sector end

-- Thing type -> sprite name and frame; `ceiling` hangs it from the ceiling.
local THING_SPRITES = {
    [3004] = "POSSA", [9] = "SPOSA", [3001] = "TROOA", [3002] = "SARGA", [58] = "SARGA", [3006] = "SKULA",
    [3005] = "HEADA", [3003] = "BOSSA", [65] = "CPOSA", [69] = "BOS2A", [66] = "SKELA", [67] = "FATTA",
    [68] = "BSPIA", [71] = "PAINA", [64] = "VILEA", [16] = "CYBRA", [7] = "SPIDA", [84] = "SSWVA",
    [2011] = "STIMA", [2012] = "MEDIA", [2014] = "BON1A", [2015] = "BON2A", [2018] = "ARM1A", [2019] = "ARM2A",
    [2001] = "SHOTA", [82] = "SGN2A", [2002] = "MGUNA", [2003] = "LAUNA", [2004] = "PLASA", [2005] = "CSAWA",
    [2006] = "BFUGA", [2007] = "CLIPA", [2008] = "SHELA", [2010] = "ROCKA", [2046] = "BROKA", [2047] = "CELLA",
    [17] = "CELPA", [2048] = "AMMOA", [2049] = "SBOXA", [8] = "BPAKA", [2013] = "SOULA", [2022] = "PINVA",
    [2023] = "PSTRA", [2024] = "PINSA", [2025] = "SUITA", [2026] = "PMAPA", [2045] = "PVISA", [83] = "MEGAA",
    [5] = "BKEYA", [6] = "YKEYA", [13] = "RKEYA", [40] = "BSKUA", [39] = "YSKUA", [38] = "RSKUA",
    [2035] = "BAR1A", [2028] = "COLUA", [34] = "CANDA", [35] = "CBRAA", [44] = "TBLUA", [45] = "TGRNA",
    [46] = "TREDA", [55] = "SMBTA", [56] = "SMGTA", [57] = "SMRTA", [48] = "ELECA", [47] = "SMITA", [43] = "TRE1A",
    [54] = "TRE2A", [41] = "CEYEA", [42] = "FSKUA", [30] = "COL1A", [31] = "COL2A", [32] = "COL3A", [33] = "COL4A",
    [36] = "COL5A", [37] = "COL6A", [24] = "POL5A", [25] = "POL1A", [26] = "POL6A", [27] = "POL4A", [28] = "POL2A",
    [29] = "POL3A", [10] = "PLAYW", [12] = "PLAYW", [15] = "PLAYN", [18] = "POSSL", [19] = "SPOSL", [20] = "TROOM",
    [21] = "SARGN", [22] = "HEADL", [23] = "SKULK", [70] = "FCANA", [85] = "TLMPA", [86] = "TLP2A",
    [49] = "GOR1A", [50] = "GOR2A", [51] = "GOR3A", [52] = "GOR4A", [53] = "GOR5A", [59] = "GOR2A",
    [60] = "GOR4A", [61] = "GOR3A", [62] = "GOR5A", [63] = "GOR1A", [73] = "HDB1A", [74] = "HDB2A",
    [75] = "HDB3A", [76] = "HDB4A", [77] = "HDB5A", [78] = "HDB6A", [79] = "POB1A", [80] = "POB2A", [81] = "BRS1A",
}
local CEILING_THINGS = { [49] = true, [50] = true, [51] = true, [52] = true, [53] = true, [59] = true,
    [60] = true, [61] = true, [62] = true, [63] = true, [73] = true, [74] = true, [75] = true, [76] = true,
    [77] = true, [78] = true }

local function load_map(name)
    local base = by_name[name]
    local function part(offset) return lump_data(base + offset) end
    local m = { vertexes = {}, lines = {}, sides = {}, sectors = {}, segs = {}, subsectors = {}, nodes = {}, things = {}, tags = {} }

    local d = part(4)
    for i = 0, #d // 4 - 1 do
        local x, y = sunpack("<i2i2", d, i * 4 + 1)
        m.vertexes[i] = { x = x, y = y }
    end
    d = part(8)
    for i = 0, #d // 26 - 1 do
        local fh, ch, fp, cp, light, special, tag = sunpack("<i2i2c8c8i2i2i2", d, i * 26 + 1)
        local s = { floorh = fh, ceilh = ch, floorpic = lump_name(fp), ceilpic = lump_name(cp),
            light = light, special = special, tag = tag, lines = {}, index = i }
        m.sectors[i] = s
        if tag ~= 0 then
            m.tags[tag] = m.tags[tag] or {}
            table.insert(m.tags[tag], s)
        end
    end
    d = part(3)
    for i = 0, #d // 30 - 1 do
        local xo, yo, up, lo, mid, sec = sunpack("<i2i2c8c8c8i2", d, i * 30 + 1)
        m.sides[i] = { xoff = xo, yoff = yo, upper = lump_name(up), lower = lump_name(lo), mid = lump_name(mid), sector = m.sectors[sec] }
    end
    d = part(2)
    for i = 0, #d // 14 - 1 do
        local v1, v2, flags, special, tag, right, left = sunpack("<I2I2i2i2i2I2I2", d, i * 14 + 1)
        local l = { v1 = m.vertexes[v1], v2 = m.vertexes[v2], flags = flags, special = special, tag = tag,
            right = m.sides[right], left = left ~= 65535 and m.sides[left] or nil }
        local a, b = l.v1, l.v2
        l.minx, l.maxx, l.miny, l.maxy = min(a.x, b.x), max(a.x, b.x), min(a.y, b.y), max(a.y, b.y)
        l.front = l.right.sector
        l.back = l.left and l.left.sector
        table.insert(l.front.lines, l)
        if l.back and l.back ~= l.front then table.insert(l.back.lines, l) end
        m.lines[i] = l
    end
    d = part(5)
    for i = 0, #d // 12 - 1 do
        local v1, v2, _, line, side, offset = sunpack("<I2I2i2I2i2i2", d, i * 12 + 1)
        local l = m.lines[line]
        local a, b = m.vertexes[v1], m.vertexes[v2]
        local sd = side == 0 and l.right or l.left
        local other = l.right
        if side == 0 then other = l.left end
        m.segs[i] = { v1 = a, v2 = b, line = l, side = sd, front = sd.sector, back = other and other.sector,
            offset = offset, len = math.sqrt((b.x - a.x) ^ 2 + (b.y - a.y) ^ 2),
            contrast = (a.y == b.y) and -1 or ((a.x == b.x) and 1 or 0) }
    end
    d = part(6)
    for i = 0, #d // 4 - 1 do
        local count, first = sunpack("<I2I2", d, i * 4 + 1)
        local segs = {}
        for k = 0, count - 1 do segs[k + 1] = m.segs[first + k] end
        m.subsectors[i] = { segs = segs, sector = segs[1].front }
    end
    d = part(7)
    for i = 0, #d // 28 - 1 do
        local x, y, dx, dy = sunpack("<i2i2i2i2", d, i * 28 + 1)
        local right, left = sunpack("<I2I2", d, i * 28 + 25)
        m.nodes[i] = { x = x, y = y, dx = dx, dy = dy, child = { [0] = right, [1] = left } }
    end
    m.node_count = #d // 28
    map = m

    d = part(1)
    for i = 0, #d // 10 - 1 do
        local x, y, angle, kind, flags = sunpack("<i2i2i2i2i2", d, i * 10 + 1)
        if kind == 1 then
            player.x, player.y, player.angle = x, y, angle * pi / 180
        elseif THING_SPRITES[kind] and flags & 16 == 0 and flags & 2 ~= 0 then
            local s = sector_at(x, y)
            table.insert(m.things, { x = x, y = y, angle = angle, sprite = THING_SPRITES[kind],
                sector = s, ceiling = CEILING_THINGS[kind] })
        end
    end
    local s = sector_at(player.x, player.y)
    player.z, player.momx, player.momy, player.momz, player.viewh, player.dvh = s.floorh, 0, 0, 0, 41, 0
    map_name = name
    music_start(name)
end

---------------------------------------------------------------- moving sectors

local movers = {}

local function neighbors(sector)
    local list = {}
    for _, l in ipairs(sector.lines) do
        local other = (l.front == sector) and l.back or l.front
        if other and other ~= sector then list[#list + 1] = other end
    end
    return list
end
local function lowest_ceiling(s)
    local h = math.huge
    for _, o in ipairs(neighbors(s)) do h = min(h, o.ceilh) end
    return h == math.huge and s.ceilh or h
end
local function lowest_floor(s)
    local h = s.floorh
    for _, o in ipairs(neighbors(s)) do h = min(h, o.floorh) end
    return h
end
local function highest_floor(s)
    local h = -32768
    for _, o in ipairs(neighbors(s)) do h = max(h, o.floorh) end
    return h == -32768 and s.floorh or h
end
local function next_floor(s)
    local h = math.huge
    for _, o in ipairs(neighbors(s)) do if o.floorh > s.floorh then h = min(h, o.floorh) end end
    return h == math.huge and s.floorh or h
end

-- One mover per sector at a time, as in Doom.
local function door(sector, kind, speed)
    local m = sector.mover
    if m then
        -- Using a door that's moving again sends it the other way.
        if m.door and kind == "raise" then
            m.dir = (m.dir == -1) and 1 or -1
            return true
        end
        return false
    end
    m = { door = true, sector = sector, kind = kind, speed = speed or 2, wait = 0 }
    if kind == "close" then m.dir = -1 else m.dir = 1 m.top = lowest_ceiling(sector) - 4 end
    sector.mover = m
    movers[#movers + 1] = m
    return true
end

local function lift(sector, speed)
    if sector.mover then return false end
    local m = { lift = true, sector = sector, low = lowest_floor(sector), high = sector.floorh, dir = -1, speed = speed or 4, wait = 0 }
    sector.mover = m
    movers[#movers + 1] = m
    return true
end

local function move_floor(sector, kind)
    if sector.mover then return false end
    local target, speed = sector.floorh, 1
    if kind == "lowest" then target = lowest_floor(sector)
    elseif kind == "highest" then target = highest_floor(sector)
    elseif kind == "turbo" then target, speed = highest_floor(sector) + 8, 4
    elseif kind == "ceiling" then target = lowest_ceiling(sector)
    elseif kind == "next" then target = next_floor(sector) end
    local m = { floor = true, sector = sector, target = target, speed = speed }
    sector.mover = m
    movers[#movers + 1] = m
    return true
end

local function player_sector() return sector_at(player.x, player.y) end

local function run_movers()
    local keep = {}
    for _, m in ipairs(movers) do
        local s, done = m.sector, false
        if m.door then
            if m.wait > 0 then
                m.wait = m.wait - 1
                if m.wait == 0 then m.dir = -1 end
            elseif m.dir == 1 then
                s.ceilh = min(m.top, s.ceilh + m.speed)
                if s.ceilh >= m.top then
                    if m.kind == "raise" then m.wait, m.dir = 150, 0 else done = true end
                end
            elseif m.dir == -1 then
                local next_h = max(s.floorh, s.ceilh - m.speed)
                if player_sector() == s and next_h < player.z + 56 then
                    m.dir = 1 -- something's in the way: back up
                    m.top = m.top or lowest_ceiling(s) - 4
                else
                    s.ceilh = next_h
                    if s.ceilh <= s.floorh then done = true end
                end
            end
        elseif m.lift then
            if m.wait > 0 then
                m.wait = m.wait - 1
                if m.wait == 0 then m.dir = 1 end
            elseif m.dir == -1 then
                s.floorh = max(m.low, s.floorh - m.speed)
                if s.floorh <= m.low then m.wait, m.dir = 105, 0 end
            elseif m.dir == 1 then
                s.floorh = min(m.high, s.floorh + m.speed)
                if s.floorh >= m.high then done = true end
            end
        elseif m.floor then
            if s.floorh < m.target then s.floorh = min(m.target, s.floorh + m.speed)
            else s.floorh = max(m.target, s.floorh - m.speed) end
            if s.floorh == m.target then done = true end
        end
        if done then s.mover = nil else keep[#keep + 1] = m end
    end
    movers = keep
end

---------------------------------------------------------------- line specials

-- [special] = { action, argument, how ("D" door on the line's back,
-- "S" switch, "W" walk over), repeatable, fast }
local SPECIALS = {
    [1] = { "door", "raise", "D", true }, [26] = { "door", "raise", "D", true }, [27] = { "door", "raise", "D", true },
    [28] = { "door", "raise", "D", true }, [117] = { "door", "raise", "D", true, true },
    [31] = { "door", "open", "D" }, [32] = { "door", "open", "D" }, [33] = { "door", "open", "D" },
    [34] = { "door", "open", "D" }, [118] = { "door", "open", "D", false, true },
    [2] = { "door", "open", "W" }, [3] = { "door", "close", "W" }, [4] = { "door", "raise", "W" },
    [86] = { "door", "open", "W", true }, [90] = { "door", "raise", "W", true }, [75] = { "door", "close", "W", true },
    [29] = { "door", "raise", "S" }, [103] = { "door", "open", "S" }, [50] = { "door", "close", "S" },
    [63] = { "door", "raise", "S", true }, [61] = { "door", "open", "S", true }, [42] = { "door", "close", "S", true },
    [111] = { "door", "raise", "S", false, true }, [112] = { "door", "open", "S", false, true },
    [10] = { "lift", nil, "W" }, [88] = { "lift", nil, "W", true }, [21] = { "lift", nil, "S" }, [62] = { "lift", nil, "S", true },
    [38] = { "floor", "lowest", "W" }, [23] = { "floor", "lowest", "S" }, [60] = { "floor", "lowest", "S", true },
    [82] = { "floor", "lowest", "W", true }, [19] = { "floor", "highest", "W" }, [102] = { "floor", "highest", "S" },
    [45] = { "floor", "highest", "S", true }, [83] = { "floor", "highest", "W", true }, [36] = { "floor", "turbo", "W" },
    [70] = { "floor", "turbo", "S", true }, [71] = { "floor", "turbo", "S" }, [98] = { "floor", "turbo", "W", true },
    [5] = { "floor", "ceiling", "W" }, [101] = { "floor", "ceiling", "S" }, [91] = { "floor", "ceiling", "W", true },
    [64] = { "floor", "ceiling", "S", true }, [18] = { "floor", "next", "S" }, [119] = { "floor", "next", "W" },
    [69] = { "floor", "next", "S", true }, [128] = { "floor", "next", "W", true },
    [11] = { "exit", nil, "S" }, [51] = { "exit", nil, "S" }, [52] = { "exit", nil, "W" }, [124] = { "exit", nil, "W" },
}

local function next_map()
    local e, m = map_name:match("^E(%d)M(%d)$")
    local name
    if e then name = string.format("E%dM%d", e, m + 1) else
        name = string.format("MAP%02d", tonumber(map_name:match("%d+")) + 1)
    end
    if not by_name[name] then name = map_name end
    movers = {}
    load_map(name)
    message, message_until = name, leveltime + 105
end

local function flip_switch(line)
    local side = line.right
    for _, key in ipairs({ "upper", "mid", "lower" }) do
        local name = side[key]
        if name:sub(1, 3) == "SW1" then side[key] = "SW2" .. name:sub(4)
        elseif name:sub(1, 3) == "SW2" then side[key] = "SW1" .. name:sub(4) end
    end
end

local function activate(line, how)
    local spec = SPECIALS[line.special]
    if not spec or spec[3] ~= how then return false end
    local action, arg, _, repeatable, fast = spec[1], spec[2], spec[3], spec[4], spec[5]
    if action == "exit" then
        if how == "S" then flip_switch(line) end
        next_map()
        return true
    end
    local sectors = (how == "D") and { line.back } or (map.tags[line.tag] or {})
    local any = false
    for _, s in ipairs(sectors) do
        local ok
        if action == "door" then ok = door(s, arg, fast and 8 or 2)
        elseif action == "lift" then ok = lift(s)
        else ok = move_floor(s, arg) end
        any = any or ok
    end
    if how == "S" and any then flip_switch(line) end
    if any and not repeatable then line.special = 0 end
    return any
end

local function line_side(l, x, y)
    return (x - l.v1.x) * (l.v2.y - l.v1.y) - (y - l.v1.y) * (l.v2.x - l.v1.x)
end

-- The nearest line a 64-unit line from the player crosses, and what's
-- past it, as P_UseLines does: a special line is used; a solid one stops.
local function use_lines()
    local x1, y1 = player.x, player.y
    local x2, y2 = x1 + cos(player.angle) * 64, y1 + sin(player.angle) * 64
    local hits = {}
    for _, l in pairs(map.lines) do
        local s1, s2 = line_side(l, x1, y1), line_side(l, x2, y2)
        if (s1 > 0) ~= (s2 > 0) then
            local ax, ay, bx, by = l.v1.x, l.v1.y, l.v2.x, l.v2.y
            local d1 = (ax - x1) * (y2 - y1) - (ay - y1) * (x2 - x1)
            local d2 = (bx - x1) * (y2 - y1) - (by - y1) * (x2 - x1)
            if (d1 > 0) ~= (d2 > 0) or d1 == 0 or d2 == 0 then
                hits[#hits + 1] = { l, s1 / (s1 - s2) }
            end
        end
    end
    table.sort(hits, function(a, b) return a[2] < b[2] end)
    for _, hit in ipairs(hits) do
        local l = hit[1]
        if l.special ~= 0 then
            local spec = SPECIALS[l.special]
            if spec and spec[3] ~= "W" and line_side(l, x1, y1) > 0 then activate(l, spec[3]) end
            return
        end
        if not l.back or min(l.front.ceilh, l.back.ceilh) <= max(l.front.floorh, l.back.floorh) then return end
    end
end

---------------------------------------------------------------- movement

local RADIUS = 16

-- Whether the player can stand at (x, y): P_CheckPosition with a box.
local function check_position(x, y)
    local s = sector_at(x, y)
    local fz, cz = s.floorh, s.ceilh
    local x0, x1, y0, y1 = x - RADIUS, x + RADIUS, y - RADIUS, y + RADIUS
    for _, l in pairs(map.lines) do
        if l.maxx > x0 and l.minx < x1 and l.maxy > y0 and l.miny < y1 then
            local a = line_side(l, x0, y0) > 0
            if a ~= (line_side(l, x1, y0) > 0) or a ~= (line_side(l, x0, y1) > 0) or a ~= (line_side(l, x1, y1) > 0) then
                if not l.back or l.flags & 1 ~= 0 then return false end
                fz = max(fz, l.front.floorh, l.back.floorh)
                cz = min(cz, l.front.ceilh, l.back.ceilh)
            end
        end
    end
    if cz - fz < 56 or cz - player.z < 56 or fz - player.z > 24 then return false end
    return true
end

local function try_move(x, y)
    if not check_position(x, y) then return false end
    local ox, oy = player.x, player.y
    player.x, player.y = x, y
    for _, l in pairs(map.lines) do
        if l.special ~= 0 and SPECIALS[l.special] and SPECIALS[l.special][3] == "W" then
            if (line_side(l, ox, oy) > 0) ~= (line_side(l, x, y) > 0) then
                local ax, ay, bx, by = l.v1.x, l.v1.y, l.v2.x, l.v2.y
                if ((ax - ox) * (y - oy) - (ay - oy) * (x - ox) > 0) ~= ((bx - ox) * (y - oy) - (by - oy) * (x - ox) > 0) then
                    activate(l, "W")
                end
            end
        end
    end
    return true
end

local turn_held = 0
local function player_tic(turn_mouse)
    local run = not input_down(WALK)
    local fwd = (input_value(FORWARD) - input_value(BACK)) * (run and 50 or 25)
    local side = (input_value(STRAFE_R) - input_value(STRAFE_L)) * (run and 40 or 24)
    local turn = input_value(TURN_L) - input_value(TURN_R)
    if turn ~= 0 then turn_held = turn_held + 1 else turn_held = 0 end
    -- Doom turns slowly for the first few tics of a key press.
    local rate = (turn_held < 6 and 320 or (run and 1280 or 640)) / 65536 * 2 * pi
    player.angle = player.angle + turn * rate + turn_mouse

    local onground = player.z <= sector_at(player.x, player.y).floorh
    if onground then
        local ca, sa = cos(player.angle), sin(player.angle)
        player.momx = player.momx + (fwd * ca + side * sa) * 2048 / 65536
        player.momy = player.momy + (fwd * sa - side * ca) * 2048 / 65536
    end

    -- Moves bigger than half a radius go in steps, sliding along walls.
    local mx, my = max(-30, min(30, player.momx)), max(-30, min(30, player.momy))
    local steps = (abs(mx) > 15 or abs(my) > 15) and 2 or 1
    for _ = 1, steps do
        local nx, ny = player.x + mx / steps, player.y + my / steps
        if not try_move(nx, ny) then
            if try_move(nx, player.y) then player.momy = 0
            elseif try_move(player.x, ny) then player.momx = 0
            else player.momx, player.momy = 0, 0 end
            break
        end
    end
    if onground then
        player.momx, player.momy = player.momx * 0.90625, player.momy * 0.90625
        if abs(player.momx) < 0.06 and abs(player.momy) < 0.06 then player.momx, player.momy = 0, 0 end
    end

    local s = sector_at(player.x, player.y)
    if player.z < s.floorh then
        -- Stepping up: the view catches up over a few tics (P_CalcHeight).
        player.viewh = player.viewh - (s.floorh - player.z)
        player.dvh = (41 - player.viewh) / 8
        player.z = s.floorh
        player.momz = 0
    elseif player.z > s.floorh then
        player.momz = player.momz - 1
        player.z = player.z + player.momz
        if player.z <= s.floorh then
            if player.momz < -8 then player.dvh = player.momz / 8 end
            player.z, player.momz = s.floorh, 0
        end
    end
    player.viewh = player.viewh + player.dvh
    if player.viewh > 41 then player.viewh, player.dvh = 41, 0 end
    if player.viewh < 20.5 then player.viewh = 20.5 if player.dvh <= 0 then player.dvh = 0.25 end end
    if player.dvh ~= 0 then player.dvh = player.dvh + 0.25 end

    local speed = player.momx * player.momx + player.momy * player.momy
    player.bob = min(16, speed / 4)
end

---------------------------------------------------------------- renderer

local fb, zb = {}, {}
local sb = {}
for i = 1, W * VIEWH do fb[i], zb[i] = 0, 0 end
for i = 1, W * BARH do sb[i] = 0 end
local ceilclip, floorclip, cdx, cdy, yslope = {}, {}, {}, {}, {}
for y = 0, VIEWH - 1 do yslope[y] = PROJ / (CY - y - 0.5) end
local open_columns
local vx, vy, vz, va, ca, sa

local function draw_plane(x, y0, y1, height, pic, lightnum)
    if pic == "F_SKY1" then
        local sky = texture("SKY1") or texture("SKY2")
        if not sky then return end
        local angle = va + atan((CX - x - 0.5) / PROJX)
        local col = sky.cols[floor(angle * 512 / pi) % sky.w + 1]
        local cm = cmaps[0]
        local idx = y0 * RW + x + 1
        for y = y0, y1 do
            fb[idx] = cm[col[(100 + y - floor(CY)) % sky.h + 1]]
            zb[idx] = 1e9
            idx = idx + RW
        end
        return
    end
    local f = flat_now[pic] or flat(pic)
    if not f then return end
    local hz = height - vz
    local dx, dy = cdx[x], cdy[x]
    local zl = zlight[lightnum]
    local idx = y0 * RW + x + 1
    for y = y0, y1 do
        local d = hz * yslope[y]
        if d > 0 then
            -- `//` instead of math.floor: no function call per pixel.
            local di = d // 16
            fb[idx] = zl[di > 127 and 127 or di][f[(-((vy + d * dy) // 1) & 63) * 64 + ((vx + d * dx) // 1 & 63) + 1]]
            zb[idx] = d
        end
        idx = idx + RW
    end
end

local function draw_wall(x, y0, y1, tex, u, texmid, z, cm)
    local col = tex.cols[floor(u) % tex.w + 1]
    local th = tex.h
    local step = z / PROJ
    local v = texmid - (CY - y0 - 0.5) * step
    local idx = y0 * RW + x + 1
    for _ = y0, y1 do
        fb[idx] = cm[col[v // 1 % th + 1]]
        zb[idx] = z
        v = v + step
        idx = idx + RW
    end
end

local function light_of(sector, contrast)
    return min(15, max(0, (sector.light >> 4) + contrast))
end

local function draw_seg(seg)
    local a, b = seg.v1, seg.v2
    -- Only the side facing the player.
    if (b.x - a.x) * (vy - a.y) - (b.y - a.y) * (vx - a.x) >= 0 then return end
    local ax, ay, bx, by = a.x - vx, a.y - vy, b.x - vx, b.y - vy
    local az, axv = ax * ca + ay * sa, ax * sa - ay * ca
    local bz, bxv = bx * ca + by * sa, bx * sa - by * ca
    if az < NEAR and bz < NEAR then return end
    local side = seg.side
    local u1 = seg.offset + side.xoff
    local u2 = u1 + seg.len
    if az < NEAR then
        local t = (NEAR - az) / (bz - az)
        axv, az, u1 = axv + (bxv - axv) * t, NEAR, u1 + (u2 - u1) * t
    elseif bz < NEAR then
        local t = (NEAR - bz) / (az - bz)
        bxv, bz, u2 = bxv + (axv - bxv) * t, NEAR, u2 + (u1 - u2) * t
    end
    local sx1, sx2 = CX + axv / az * PROJX, CX + bxv / bz * PROJX
    if sx2 <= sx1 then return end
    local xs, xe = max(0, math.ceil(sx1 - 0.5)), min(RW - 1, math.ceil(sx2 - 0.5) - 1)
    if xs > xe then return end

    local front, back, line = seg.front, seg.back, seg.line
    local fc, ff = front.ceilh - vz, front.floorh - vz
    local lightnum = light_of(front, seg.contrast)
    local plane_light = light_of(front, 0)
    local sl = scalelight[lightnum]
    local ceilpic, floorpic = front.ceilpic, front.floorpic
    local unpeg_top, unpeg_bottom = line.flags & 8 ~= 0, line.flags & 16 ~= 0
    local dbx, dbz = bxv - axv, bz - az
    local du = u2 - u1

    local mid, top, bottom, midmid, topmid, botmid
    local bc, bf, sky_both
    if back then
        bc, bf = back.ceilh - vz, back.floorh - vz
        sky_both = ceilpic == "F_SKY1" and back.ceilpic == "F_SKY1"
        if back.ceilh < front.ceilh and not sky_both then
            top = texture(side.upper)
            if top then topmid = (unpeg_top and front.ceilh or (back.ceilh + top.h)) - vz + side.yoff end
        end
        if back.floorh > front.floorh then
            bottom = texture(side.lower)
            if bottom then botmid = (unpeg_bottom and front.ceilh or back.floorh) - vz + side.yoff end
        end
    else
        mid = texture(side.mid)
        if mid then midmid = (unpeg_bottom and (front.floorh + mid.h) or front.ceilh) - vz + side.yoff end
    end

    for x = xs, xe do
        local cl, fl = ceilclip[x], floorclip[x]
        if cl + 1 < fl then
            local k = (x + 0.5 - CX) / PROJX
            local den = dbx - k * dbz
            local t = den ~= 0 and (k * az - axv) / den or 0
            t = t < 0 and 0 or (t > 1 and 1 or t)
            local z = az + t * dbz
            local u = u1 + t * du
            local scale = PROJ / z
            local yt = math.ceil(CY - fc * scale - 0.5)
            local yb = math.ceil(CY - ff * scale - 0.5) - 1
            local j = floor(2560 / z)
            local cm = sl[j > 47 and 47 or j]

            -- Ceiling above the wall, floor below it.
            local ce = min(yt - 1, fl - 1)
            if ce > cl then draw_plane(x, cl + 1, ce, front.ceilh, ceilpic, plane_light) end
            local fs = max(yb + 1, cl + 1)
            if fs < fl then draw_plane(x, fs, fl - 1, front.floorh, floorpic, plane_light) end

            if not back then
                local y0, y1 = max(yt, cl + 1), min(yb, fl - 1)
                if mid and y0 <= y1 then draw_wall(x, y0, y1, mid, u, midmid, z, cm) end
                ceilclip[x], floorclip[x] = VIEWH, -1
                open_columns = open_columns - 1
            else
                -- A plane seen from the wrong side (a ceiling below the eye)
                -- isn't drawn, and leaves its rows to what's behind (r_segs.c).
                local newcl = (fc > 0 or ceilpic == "F_SKY1") and yt - 1 or cl
                local newfl = ff < 0 and yb + 1 or fl
                if top or (back.ceilh < front.ceilh and not sky_both) then
                    local ybt = math.ceil(CY - bc * scale - 0.5) - 1
                    local y0, y1 = max(yt, cl + 1), min(ybt, fl - 1)
                    if top and y0 <= y1 then draw_wall(x, y0, y1, top, u, topmid, z, cm) end
                    newcl = ybt
                elseif sky_both and back.ceilh < front.ceilh then
                    -- Sky behind sky: the sky shows down to the back ceiling.
                    local ybt = math.ceil(CY - bc * scale - 0.5) - 1
                    local y0, y1 = max(yt, cl + 1), min(ybt, fl - 1)
                    if y0 <= y1 then draw_plane(x, y0, y1, front.ceilh, "F_SKY1", plane_light) end
                    newcl = ybt
                end
                if back.floorh > front.floorh then
                    local ybb = math.ceil(CY - bf * scale - 0.5)
                    local y0, y1 = max(ybb, cl + 1), min(yb, fl - 1)
                    if bottom and y0 <= y1 then draw_wall(x, y0, y1, bottom, u, botmid, z, cm) end
                    newfl = ybb
                end
                cl, fl = max(cl, newcl), min(fl, newfl)
                if back.ceilh <= back.floorh or cl + 1 >= fl then
                    ceilclip[x], floorclip[x] = VIEWH, -1
                    open_columns = open_columns - 1
                else
                    ceilclip[x], floorclip[x] = cl, fl
                end
            end
        end
    end
end

local function render_node(n)
    if open_columns <= 0 then return end
    if n & 0x8000 ~= 0 then
        local ss = map.subsectors[n & 0x7FFF]
        for _, seg in ipairs(ss.segs) do draw_seg(seg) end
        return
    end
    local node = map.nodes[n]
    local side = ((vy - node.y) * node.dx - (vx - node.x) * node.dy >= 0) and 1 or 0
    render_node(node.child[side])
    render_node(node.child[1 - side])
end

local function sprite_for(thing)
    local prefix, frame = thing.sprite:sub(1, 4), thing.sprite:sub(5, 5)
    local angle = math.deg(atan(thing.y - vy, thing.x - vx))
    local rot = floor(((angle - thing.angle + 202.5) % 360) / 45) + 1
    return sprite_frames[prefix .. frame .. rot] or sprite_frames[prefix .. frame .. "0"]
end

local function draw_things()
    for _, th in ipairs(map.things) do
        local dx, dy = th.x - vx, th.y - vy
        local tz = dx * ca + dy * sa
        if tz >= NEAR then
            local frame = sprite_for(th)
            local p = frame and patch(frame.name)
            if p then
                local tx = dx * sa - dy * ca
                local scale, xscale = PROJ / tz, PROJX / tz
                local x1 = CX + (tx - (frame.flip and (p.w - p.lo) or p.lo)) * xscale
                local z = th.ceiling and (th.sector.ceilh - p.to) or th.sector.floorh
                local ytop = CY - (z + p.to - vz) * scale
                local xs, xe = max(0, math.ceil(x1 - 0.5)), min(RW - 1, math.ceil(x1 + p.w * xscale - 0.5) - 1)
                local ys, ye = max(0, math.ceil(ytop - 0.5)), min(VIEWH - 1, math.ceil(ytop + p.h * scale - 0.5) - 1)
                local j = floor(2560 / tz)
                local cm = scalelight[light_of(th.sector, 0)][j > 47 and 47 or j]
                for x = xs, xe do
                    local c = floor((x + 0.5 - x1) / xscale)
                    if frame.flip then c = p.w - 1 - c end
                    local col = p.cols[c + 1]
                    if col then
                        local idx = ys * RW + x + 1
                        for y = ys, ye do
                            if tz < zb[idx] then
                                local v = col[floor((y + 0.5 - ytop) / scale) + 1]
                                if v then fb[idx] = cm[v] zb[idx] = tz end
                            end
                            idx = idx + RW
                        end
                    end
                end
            end
        end
    end
end

-- A picture drawn 1:1 at a 320-wide position, offsets applied, into
-- `buf` (`bw` pixels wide, squeezed across when that's under 320).
local function draw_patch(name, x, y, cm, buf, bw, bh)
    local p = patch(name)
    if not p then return end
    x, y = x - p.lo, y - p.to
    for px = 1, p.w do
        local sx = x + px - 1
        if sx >= 0 and sx < W then
            local bx = sx * bw // W + 1
            for py, v in pairs(p.cols[px]) do
                local sy = y + py - 1
                if sy >= 0 and sy < bh then buf[sy * bw + bx] = cm[v] end
            end
        end
    end
end

local function draw_number(n, right, y, percent)
    local s = tostring(n)
    local x = right
    for i = #s, 1, -1 do
        x = x - 14
        draw_patch("STTNUM" .. s:sub(i, i), x, y, cmaps[0], sb, W, BARH)
    end
    if percent then draw_patch("STTPRCNT", right, y, cmaps[0], sb, W, BARH) end
end

local function draw_status_bar()
    draw_patch("STBAR", 0, 0, cmaps[0], sb, W, BARH)
    draw_patch("STARMS", 104, 0, cmaps[0], sb, W, BARH)
    draw_number(50, 44, 3)
    draw_number(100, 90, 3, true)
    draw_number(0, 221, 3, true)
    draw_patch("STFST01", 143, 0, cmaps[0], sb, W, BARH)
end

local function render_view()
    vx, vy, va = player.x, player.y, player.angle
    local bob = player.bob / 2 * sin(leveltime * 2 * pi / 20)
    vz = player.z + player.viewh + bob
    vz = min(vz, sector_at(vx, vy).ceilh - 4)
    ca, sa = cos(va), sin(va)
    for x = 0, RW - 1 do
        local k = (x + 0.5 - CX) / PROJX
        ceilclip[x], floorclip[x] = -1, VIEWH
        cdx[x], cdy[x] = ca + sa * k, sa - ca * k
    end
    open_columns = RW
    render_node(map.node_count - 1)
    draw_things()
    -- The pistol, bobbing as Doom's does.
    local wb = player.bob * cos(leveltime * 2 * pi / 40)
    local wy = player.bob * abs(sin(leveltime * 2 * pi / 40))
    local p = patch("PISGA0")
    if p then
        local cm = scalelight[light_of(sector_at(vx, vy), 0)][47]
        draw_patch("PISGA0", floor(1 + wb), floor(32 + wy + CY - 100 + 0.5), cm, fb, RW, VIEWH)
    end
end

---------------------------------------------------------------- main

local status_drawn = false
local tic_time = 0
local first_map
if wad then
    for _, name in ipairs({ "E1M1", "MAP01" }) do
        if by_name[name] then first_map = name break end
    end
    if first_map then load_map(first_map) end
end

local function advance_flats()
    for _, set in ipairs(FLAT_ANIMS) do
        local frame = floor(leveltime / 8)
        for i, name in ipairs(set) do flat_now[name] = flat(set[(i - 1 + frame) % #set + 1]) end
    end
end

function render(width, height)
    if not first_map then
        -- No WAD yet: say what to do, pointing at the folder button.
        clear({ r = 24, g = 0, b = 0 })
        local red, white, grey = { r = 230, g = 40, b = 30 }, { r = 255, g = 230, b = 210 }, { r = 200, g = 160, b = 150 }
        local big = width >= 700 and 96 or 48
        local tw = text_size("DOOM", big)
        text((width - tw) / 2, height * 0.22, "DOOM", red, big)
        local line = "No, I'm not kidding."
        local lw = text_size(line, 24)
        text((width - lw) / 2, height * 0.22 + big + 8, line, white, 24)
        local how = wad and ("There's no E1M1 or MAP01 in " .. wad_name .. ".")
            or "Put DOOM1.WAD (the free shareware one), DOOM.WAD or\nfreedoom1.wad in this script's assets folder, then press F5."
        local hw = text_size(how, 12)
        text((width - hw) / 2, height * 0.22 + big + 48, how, grey, 12)
        text(8, 6, "^ The folder button up there opens it.", white, 12)
        return
    end

    if chosen_wad() ~= wad_name then message, message_until = "Press F5 to load " .. chosen_wad(), leveltime + 1 end

    set_cursor_visible(not has_focus())
    set_cursor_locked(has_focus())
    local mdx = has_focus() and (mouse_delta()) or 0
    local turn_mouse = -(mdx or 0) * 0.004

    RW = setting_bool("low detail", false, { info = "Doom's low detail: half as many columns, twice as fast." }) and W // 2 or W
    CX, PROJX = RW / 2, RW / 2

    tic_time = tic_time + min(DT, 0.2)
    while tic_time >= TIC do
        tic_time = tic_time - TIC
        leveltime = leveltime + 1
        player_tic(turn_mouse)
        turn_mouse = 0
        run_movers()
    end
    if input(USE) == "pressed" then use_lines() end
    if turn_mouse ~= 0 then player.angle = player.angle + turn_mouse end
    advance_flats()
    music_tick(DT)

    render_view()
    if not status_drawn then draw_status_bar() status_drawn = true end

    -- Doom's 320x200 is shown 4:3, letterboxed.
    local w, h = width, width * 3 / 4
    if h > height then h, w = height, height * 4 / 3 end
    local x0, y0 = (width - w) / 2, (height - h) / 2
    blit(fb, RW, VIEWH, palette, x0, y0, w, h * VIEWH / H)
    blit(sb, W, BARH, palette, x0, y0 + h * VIEWH / H, w, h * BARH / H)
    if message and leveltime < message_until then
        text((width - w) / 2 + 8, (height - h) / 2 + 8, message, { r = 255, g = 60, b = 60 }, 24)
    end
end
