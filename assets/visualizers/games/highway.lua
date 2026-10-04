-- highway.lua: a Guitar Hero style game charted on the fly from any track of the song.
--
-- Songs load paused while this runs. Every track is previewed side by side, charted the
-- way you'd play it, while the song plays from just before the selected track comes in:
-- scroll to move through it, Space to pause it, and SOLO (I) to hear only the selected
-- track, to tell which is which. Pick one, a difficulty and a practice speed, then press
-- Enter or click PLAY. The game starts from the top, waiting until the first notes have
-- had time to scroll all the way down the highway. N and B change song within the playlist.
--
-- Charting: notes that start together become chords. A note's lane comes from its pitch
-- relative to the notes around it (a few seconds either side), then the melodic contour is
-- enforced on top: higher pitch moves right, lower moves left, a repeated pitch stays in
-- its lane, and big leaps jump further. Chords hang down from their top note. Long notes
-- become sustains. Drums go by kit piece instead (kick, snare, hats, toms, crashes).
-- PLAY ALL (songs with 2+ channels) hands the part around the band bar by bar; see medley_for.
-- Optionally (mute_on_miss_experimental setting), missing a note mutes the track until you hit again.
--
-- GUITAR mode (the button, or G) plays it like Guitar Hero: five frets, held, and a strum
-- (Up/Down, or a guitar controller's strum bar). Strum while holding a note's frets (lower
-- frets may stay held under a single note); a chord needs exactly its frets. HOPOs (white
-- centres: a single note close after a different one) can be hit by fretting alone while
-- the combo's going, taps (dark centres: fast runs) by fretting alone any time. Strumming
-- with nothing to hit breaks the combo, like a ghost tap. Guitar controllers show up as
-- gamepads: frets on A, B, Y, X and LB (green to orange), strum on the d-pad.

script_options({ start_paused = true })

local LANE_COLORS = {
    { r = 60,  g = 220, b = 90 },
    { r = 235, g = 60,  b = 60 },
    { r = 245, g = 215, b = 50 },
    { r = 60,  g = 140, b = 255 },
    { r = 255, g = 140, b = 30 },
}
-- keys, plus a guitar controller's frets (green, red, yellow, blue, orange)
local LANE_KEYS = {
    { "d", "1", "pad_a" }, { "f", "2", "pad_b" }, { "j", "3", "pad_y" }, { "k", "4", "pad_x" }, { "l", "5", "pad_lb" },
}
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
local SOLO = input_register("solo track", { "i" })
local GUITAR = input_register("guitar mode", { "g" })
local STRUM = input_register("strum", { "up", "down", "pad_dpad_up", "pad_dpad_down" })
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
local preview_t = 0
local solo = false      -- hear only the selected track, in the menu
local guitar = store_get("guitar") == true -- Guitar Hero mode: frets held, strummed
local fret_hit_at = -1  -- when frets alone last hit a HOPO or tap (a strum right after is forgiven)
local solo_muted = {}   -- the channels solo muted, to unmute
local seek_to, last_seek = nil, -1 -- a scroll seek waiting, and when the last one went
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

-- Solo, in the menu: every other track muted, so the selected one can be told apart.
-- Called whenever what should be muted might have changed. PLAY ALL has no one channel.
local function apply_solo()
    local t = tracks[sel_track]
    local keep = solo and state == "menu" and t and not t.medley and t.ch
    for _, ch in ipairs(solo_muted) do set_channel_enabled(ch, true) end
    solo_muted = {}
    if keep then
        for _, other in ipairs(tracks) do
            if other.ch and other.ch ~= keep and not other.medley then
                set_channel_enabled(other.ch, false)
                table.insert(solo_muted, other.ch)
            end
        end
    end
    store_set("solo_muted", #solo_muted > 0 and solo_muted or nil) -- undone after a restart
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

local function chan_label(ch)
    return ch == 9 and "DRUMS" or ("CH " .. (ch + 1))
end

local function track_name(t)
    return t.medley and "PLAY ALL" or chan_label(t.ch)
end

-- PLAY ALL ------------------------------------------------------------------
--
-- The song is cut into bars. Every channel gets a score for every bar it plays in:
--   density     notes per second against a target for the difficulty: too sparse is
--               dull, and past the target it falls off again, so a busy hi-hat bar
--               doesn't win just by being busy
--   variety     how many different pitches, and how often consecutive notes change
--   rhythm      how many different gaps between notes (in sixteenths)
--   prominence  average velocity
--   melody      how often it's the highest note sounding (the tune is usually on top)
--   entrance    a bonus for coming in after two silent bars, when you'd notice it
-- Drums score a bit lower, so they lead when nothing else is doing much.
-- Then one pass of dynamic programming picks the channel for each bar that gives the best
-- total, minus a cost per switch: cheap every 4 bars (phrase edges) and on an entrance,
-- dear in between, so parts last a while and change hands where the music does. Silent
-- channels can't be picked while anything else is playing. No randomness: the same song
-- always gives the same chart.

local MEDLEY_TARGET = { 2.0, 3.5, 5.0, 7.0 } -- notes per second a bar should ideally have

local function bar_segments(len)
    local segs = {}
    if not beat(0) then -- no beats (SMPTE timing): 2 second blocks
        local t = 0
        while t < len do
            segs[#segs + 1] = { t0 = t, t1 = math.min(len, t + 2), phrase = #segs % 4 == 0 }
            t = t + 2
        end
        return segs
    end
    local t = 0
    while t < len and #segs < 10000 do
        local num, den = time_signature(t)
        local t1 = time_at_beat(beat(t) + num * 4 / den)
        if not t1 or t1 <= t then break end
        segs[#segs + 1] = { t0 = t, t1 = math.min(t1, len), phrase = #segs % 4 == 0 }
        t = t1
    end
    return segs
end

local function medley_analysis(real_tracks, len)
    local segs = bar_segments(len)
    local buckets, feats, chans = {}, {}, {}
    for s = 1, #segs do buckets[s] = {} end
    if #segs == 0 then return { segs = segs, buckets = buckets, feats = feats, chans = chans } end
    for _, tr in ipairs(real_tracks) do
        chans[#chans + 1] = tr.ch
        local s = 1
        for _, n in ipairs(tr.notes) do
            while segs[s + 1] and n.start >= segs[s + 1].t0 do s = s + 1 end
            local b = buckets[s][tr.ch]
            if not b then
                b = {}
                buckets[s][tr.ch] = b
            end
            b[#b + 1] = n
        end
    end

    for s, seg in ipairs(segs) do
        local f = {}
        feats[s] = f
        local sixteenth = 15 / (tempo(seg.t0) or 120)
        local melodic = {}
        for ch, b in pairs(buckets[s]) do
            if ch ~= 9 then
                for _, n in ipairs(b) do melodic[#melodic + 1] = n end
            end
        end
        for ch, b in pairs(buckets[s]) do
            local count = #b
            local seen_key, seen_gap = {}, {}
            local distinct, distinct_gap, changes, vel, top = 0, 0, 0, 0, 0
            for k, n in ipairs(b) do
                if not seen_key[n.key] then
                    seen_key[n.key] = true
                    distinct = distinct + 1
                end
                vel = vel + n.velocity
                if k > 1 then
                    local prev = b[k - 1]
                    if n.key ~= prev.key then changes = changes + 1 end
                    local gap = n.start - prev.start
                    if gap > 0.03 then
                        local q = math.floor(gap / sixteenth + 0.5)
                        if not seen_gap[q] then
                            seen_gap[q] = true
                            distinct_gap = distinct_gap + 1
                        end
                    end
                end
                if ch ~= 9 then
                    local is_top = true
                    for _, o in ipairs(melodic) do
                        if o.channel ~= ch and o.key > n.key and o.start <= n.start + 0.03 and o.stop > n.start then
                            is_top = false
                            break
                        end
                    end
                    if is_top then top = top + 1 end
                end
            end
            local quiet_before = s > 2 and not buckets[s - 1][ch] and not buckets[s - 2][ch]
            f[ch] = {
                count = count,
                variety = count > 1 and (0.5 * math.min(1, distinct / 5) + 0.5 * changes / (count - 1)) or 0.2,
                rhythm = count > 2 and math.min(1, distinct_gap / 3) or 0.3,
                prominence = vel / count / 127,
                melody = ch ~= 9 and top / count or 0,
                entrance = quiet_before and count >= 3,
            }
        end
    end
    return { segs = segs, buckets = buckets, feats = feats, chans = chans }
end

-- The notes (and which channel has the part when) for one difficulty.
local function medley_for(track, diff)
    if track.by_diff[diff] then return track.by_diff[diff] end
    local m = track.analysis
    local target = MEDLEY_TARGET[diff]
    local S = #m.segs
    local result = { notes = {}, parts = {} }
    track.by_diff[diff] = result
    if S == 0 then return result end

    local best, from = {}, {}
    for s = 1, S do
        local seg, f = m.segs[s], m.feats[s]
        local dur = math.max(0.25, seg.t1 - seg.t0)
        local any = next(f) ~= nil
        best[s], from[s] = {}, {}
        for _, ch in ipairs(m.chans) do
            local x = f[ch]
            local v
            if x then
                local d = x.count / dur / target
                local dens = d < 1 and d or 1 / math.sqrt(d)
                v = dens + 0.6 * x.variety + 0.4 * x.rhythm + 0.3 * x.prominence + 0.5 * x.melody
                    + (x.entrance and 0.3 or 0)
                if ch == 9 then v = v * 0.75 end
            else
                v = any and -1000 or 0
            end
            if s == 1 then
                best[s][ch] = v
            else
                local cost = seg.phrase and 0.3 or 0.9
                if x and x.entrance then cost = cost * 0.5 end
                local bv, bc = -math.huge, nil
                for _, pc in ipairs(m.chans) do
                    local c = best[s - 1][pc] - (pc == ch and 0 or cost)
                    if c > bv then bv, bc = c, pc end
                end
                best[s][ch], from[s][ch] = bv + v, bc
            end
        end
    end

    local pick = {}
    local bv, bc = -math.huge, nil
    for _, ch in ipairs(m.chans) do
        if best[S][ch] > bv then bv, bc = best[S][ch], ch end
    end
    for s = S, 1, -1 do
        pick[s] = bc
        bc = from[s][bc]
    end
    for s = 1, S do
        local b = m.buckets[s][pick[s]]
        if b then
            local last = result.parts[#result.parts]
            if not last or last.ch ~= pick[s] then
                result.parts[#result.parts + 1] = { t = m.segs[s].t0, ch = pick[s] }
            end
            for _, n in ipairs(b) do result.notes[#result.notes + 1] = n end
        end
    end
    return result
end

local function part_at(parts, t)
    local found = parts[1]
    for _, pt in ipairs(parts) do
        if pt.t <= t then found = pt else break end
    end
    return found
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
    if #tracks >= 2 then
        local m = { ch = "all", medley = true, by_diff = {}, analysis = medley_analysis(tracks, p.length) }
        local r = medley_for(m, 2)
        if #r.notes > 0 then
            m.notes = r.notes
            m.nps = #r.notes / math.max(1, r.notes[#r.notes].start - r.notes[1].start)
            table.insert(tracks, 1, m)
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

-- For GUITAR mode: which gems strum ("strum"), which are HOPOs (a single note within a
-- third of a beat of the previous chord, on a lane that wasn't in it) and which are taps
-- (HOPOs in a fast run: four or more single notes each within a quarter beat of the last).
-- Every gem of a chord gets the chord's start index as `group`.
local function beats_at(t)
    return beat(t) or t * 2 -- (no tempo map: as if 120 bpm)
end

local function mark_kinds(gems)
    local groups = {}
    local i = 1
    while i <= #gems do
        local grp = { first = i, t = gems[i].t, lanes = {} }
        while gems[i] and gems[i].t == grp.t do
            gems[i].group = grp.first
            grp.lanes[gems[i].lane] = true
            grp.size = (grp.size or 0) + 1
            i = i + 1
        end
        grp.last = i - 1
        groups[#groups + 1] = grp
    end
    for gi, grp in ipairs(groups) do
        local prev = groups[gi - 1]
        local kind = "strum"
        if prev and grp.size == 1 and not prev.lanes[gems[grp.first].lane]
                and beats_at(grp.t) - beats_at(prev.t) <= 1 / 3 + 0.01 then
            kind = "hopo"
        end
        grp.kind = kind
        grp.gap = prev and beats_at(grp.t) - beats_at(prev.t) or math.huge
    end
    -- taps: runs of fast single notes
    local run_start = nil
    for gi = 1, #groups + 1 do
        local grp = groups[gi]
        local fast = grp and grp.size == 1 and grp.gap <= 0.25 + 0.01
        if fast and not run_start then run_start = gi end
        if not fast and run_start then
            if gi - run_start >= 3 then -- the note before the run, plus 3+ fast ones
                for k = run_start, gi - 1 do groups[k].kind = "tap" end
            end
            run_start = nil
        end
    end
    for _, grp in ipairs(groups) do
        for k = grp.first, grp.last do gems[k].kind = grp.kind end
    end
end

local function build_chart(track, diff, lanes)
    local d = DIFFS[diff]
    local notes = track.medley and medley_for(track, diff).notes or track.notes

    -- 1. notes that start together become one chord
    local groups = {}
    local i = 1
    while i <= #notes do
        local g = { t = notes[i].start, notes = {}, stop = 0 }
        g.ch = notes[i].channel
        g.drums = g.ch == 9
        while i <= #notes and notes[i].start - g.t <= 0.03 and notes[i].channel == g.ch do
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
    local prev_top, prev_lane, prev_t, prev_ch = nil, nil, -math.huge, nil
    local lo_i, hi_i = 1, 1
    for gi, g in ipairs(kept) do
        local next_t = kept[gi + 1] and kept[gi + 1].t or math.huge
        local glanes = {}

        if g.drums then
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
                if kept[j].ch == g.ch then -- in PLAY ALL, only this part's own notes
                    lo = math.min(lo, kept[j].top)
                    hi = math.max(hi, kept[j].top)
                end
            end
            local lane = hi > lo and math.floor((g.top - lo) / (hi - lo + 1) * lanes) or math.floor(lanes / 2)

            -- the contour wins over the range, unless it's a fresh phrase
            if prev_ch ~= g.ch then prev_top = nil end -- the part changed hands: start fresh
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
            prev_top, prev_lane, prev_t, prev_ch = g.top, start + span, g.t, g.ch
        end

        local tail = nil
        if not g.drums then
            local tail_end = math.min(g.stop, next_t - 0.08)
            if tail_end - g.t >= 0.4 then tail = tail_end end
        end
        table.sort(glanes)
        for _, l in ipairs(glanes) do
            gems[#gems + 1] = { t = g.t, lane = l, tail = tail, ch = g.ch }
        end
    end
    mark_kinds(gems)
    return { gems = gems, lanes = lanes, ch = track.ch }
end

-- The lanes in use: always five in GUITAR mode.
local function lane_count()
    local chosen = setting_int("lanes", 4, 3, 5) -- (read either way, so it stays in Script Settings)
    return guitar and 5 or chosen
end

local function get_chart(track, diff, lanes)
    local key = track.ch .. ":" .. diff .. ":" .. lanes
    if not charts[key] then charts[key] = build_chart(track, diff, lanes) end
    return charts[key]
end

local function best_key(ch, diff, speed)
    local key = "best:" .. tostring(song_key) .. ":" .. ch .. ":" .. diff
    if speed ~= 1 then key = key .. ":" .. math.floor(speed * 100) end -- practice runs keep their own
    if guitar then key = key .. ":guitar" end
    return key
end

-- state changes -------------------------------------------------------------

-- Where the menu's preview plays from: just before the selected track comes in.
local function preview_start()
    local t = tracks[sel_track]
    local notes = t and (t.medley and medley_for(t, sel_diff).notes or t.notes)
    return notes and notes[1] and math.max(0, notes[1].start - 1) or 0
end

-- The menu: the song plays as a preview, from just before the selected track.
local function enter_menu()
    set_muted(nil)
    give_back_playback()
    state = "menu"
    chart = nil
    seek_to = nil
    preview_t = preview_start()
    if #tracks > 0 then
        seek(preview_t)
        set_paused(false)
    end
    apply_solo()
end

local hype, acc_ema = 0.35, 0.8 -- how into it the crowd is, and a running hit rate

local function start_game()
    local pspeed = SPEEDS[speed_idx]
    local look = hw_secs_cur * pspeed
    local tr = tracks[sel_track]
    if not tr then return end
    play_track = tr
    chart = get_chart(tr, sel_diff, lane_count())
    fret_hit_at = -1
    for _, g in ipairs(chart.gems) do
        g.state, g.holding = nil, false
    end
    stats = { score = 0, combo = 0, max_combo = 0, perfect = 0, good = 0, miss = 0 }
    acc_ema = 0.8
    first_live, holding, particles, popup = 1, {}, {}, nil
    state = "preroll"
    apply_solo() -- (out of the menu: everything unmuted)
    set_muted(nil)
    borrow_playback(pspeed)
    local p = playback()
    set_paused(true)
    if p.position > 0 or p.finished then seek(0) end
    -- start the clock early enough for the first note to travel the whole highway
    local first = chart.gems[1] and chart.gems[1].t or 0
    game_time = math.min(-pspeed, first - look - 0.5 * pspeed)
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
    -- depth (in z) of one lane-width, scaled so a gem lying on the road at the hit line looks
    -- about 55% as deep as it is wide; perspective flattens it further into the distance
    L.kz = (L.bw / lanes) * 0.55 / (K / G1 * L.range)
    return L
end
local function y_of(L, z) return L.hit_y - (1 - s_of(z)) / G1 * L.range end
local function x_of(L, lanepos, z) return L.cx + (lanepos / L.lanes - 0.5) * L.bw * s_of(z) end

-- gems ---------------------------------------------------------------------
--
-- Each lane's gem has the cut its stone is best known for, built from facets in "road
-- space" (u across the lane, v along the road, both -1..1) and pushed through the same
-- perspective as the highway, so gems and their sockets lie flat on it.
--   green  emerald   emerald cut (step cut: stepped rectangular rings)
--   red    ruby      cushion brilliant
--   yellow citrine   pear brilliant, pointing up the road
--   blue   sapphire  oval brilliant
--   orange fire opal trillion brilliant

local GEM_R = 0.4                    -- lane widths
local LIGHT_U, LIGHT_V = -0.45, 0.89 -- light from the far left

local function make_cut(outline, style)
    local n = #outline / 2
    local pts = {}
    local function add_ring(k)
        local base = #pts / 2
        for i = 1, n do
            pts[#pts + 1] = outline[2 * i - 1] * k
            pts[#pts + 1] = outline[2 * i] * k
        end
        local idx = {}
        for i = 1, n do idx[i] = base + i end
        return idx
    end
    local cut = { pts = pts, facets = {} }
    cut.rim = add_ring(1.14)
    cut.outline = add_ring(1)
    local function light(i) -- how much outline edge i faces the light
        local j = i % n + 1
        local dx, dv = outline[2 * j - 1] - outline[2 * i - 1], outline[2 * j] - outline[2 * i]
        local len = math.sqrt(dx * dx + dv * dv)
        return (dv * LIGHT_U - dx * LIGHT_V) / len
    end
    local function facet(idx, shade) cut.facets[#cut.facets + 1] = { idx = idx, shade = shade } end
    local o = cut.outline
    if style == "step" then
        local mid, tab = add_ring(0.74), add_ring(0.48)
        for i = 1, n do
            local j = i % n + 1
            local l = light(i)
            facet({ o[i], o[j], mid[j], mid[i] }, 0.8 + 0.3 * l)
            facet({ mid[i], mid[j], tab[j], tab[i] }, 0.85 - 0.25 * l) -- steps alternate
        end
        cut.table = tab
        facet(tab, 1.12)
    else
        local tab = add_ring(0.52)
        for i = 1, n do
            local j = i % n + 1
            local l = 0.82 + 0.3 * light(i)
            if i % 2 == 0 then
                facet({ o[i], o[j], tab[i] }, l + 0.1)
                facet({ o[j], tab[j], tab[i] }, l - 0.12)
            else
                facet({ o[i], o[j], tab[j] }, l + 0.1)
                facet({ o[i], tab[j], tab[i] }, l - 0.12)
            end
        end
        cut.table = tab
        facet(tab, 1.1)
    end
    return cut
end

local function curve(n, fn) -- counter-clockwise outline from fn(angle) -> u, v
    local o = {}
    for k = 0, n - 1 do
        local u, v = fn(k / n * math.pi * 2)
        o[#o + 1], o[#o + 2] = u, v
    end
    return o
end

local CUTS = {
    make_cut({ 0.82, -0.38, 0.82, 0.38, 0.58, 0.62, -0.58, 0.62, -0.82, 0.38, -0.82, -0.38, -0.58, -0.62, 0.58, -0.62 },
        "step"),
    make_cut(curve(12, function(a)
        local c, sn = math.cos(a + math.pi / 12), math.sin(a + math.pi / 12)
        local r = 0.8 / (math.abs(c) ^ 4 + math.abs(sn) ^ 4) ^ 0.25
        return r * c, r * sn
    end), "brilliant"),
    make_cut(curve(12, function(t) return -1.05 * math.sin(t) * math.sin(t / 2), 0.95 * math.cos(t) end), "brilliant"),
    make_cut(curve(12, function(a) return 0.74 * math.cos(a), 0.95 * math.sin(a) end), "brilliant"),
    make_cut(curve(12, function(a)
        local tri = math.cos(math.pi / 3) / math.cos(((a) % (math.pi * 2 / 3)) - math.pi / 3)
        local r = 0.92 * (0.78 * tri + 0.22)
        return r * math.cos(a + math.pi / 2), r * math.sin(a + math.pi / 2)
    end), "brilliant"),
}

local function shade(c, f, a)
    if f <= 1 then return { r = c.r * f, g = c.g * f, b = c.b * f, a = a } end
    local t = math.min(1, f - 1)
    return { r = c.r + (255 - c.r) * t, g = c.g + (255 - c.g) * t, b = c.b + (255 - c.b) * t, a = a }
end

-- Project a cut lying on the road (lane, z) into screen points, optionally scaled.
local function project_cut(L, cut, lane, z, scale)
    local P, px, py = cut.pts, {}, {}
    local k = GEM_R * (scale or 1)
    -- true perspective squashes distant gems to slivers; ease off so they still read as gems
    local kv = k * L.kz / s_of(math.max(0, z)) ^ 0.6
    for i = 1, #P / 2 do
        local lp = lane + 0.5 + P[2 * i - 1] * k
        local zz = z + P[2 * i] * kv
        px[i], py[i] = x_of(L, lp, zz), y_of(L, zz)
    end
    return px, py
end

local function poly(px, py, idx, col)
    local f = {}
    for _, k in ipairs(idx) do
        f[#f + 1] = px[k]
        f[#f + 1] = py[k]
    end
    polygon(f, col)
end

local function loop(px, py, idx, col)
    for i = 1, #idx do
        local a, b = idx[i], idx[i % #idx + 1]
        line(px[a], py[a], px[b], py[b], col)
    end
end

local function draw_gem(L, lane, z, base, alpha, seed)
    local cut = CUTS[lane + 1]
    local px, py = project_cut(L, cut, lane, z)
    poly(px, py, cut.rim, { r = 8, g = 8, b = 12, a = alpha })
    if L.bw / L.lanes * s_of(z) < 36 then -- far away: silhouette and table are enough
        poly(px, py, cut.outline, shade(base, 0.85, alpha))
        poly(px, py, cut.table, shade(base, 1.12, alpha))
        return
    end
    for fi, f in ipairs(cut.facets) do
        local tw = math.sin(TIME * 2.5 + fi * 1.9 + seed)
        local glint = tw > 0.92 and (tw - 0.92) * 9 or 0 -- now and then a facet catches the light
        poly(px, py, f.idx, shade(base, f.shade + glint, alpha))
    end
end

-- the socket each lane's gems drop into, the same cut, at the hit line
local function draw_socket(L, lane, pressed, fl)
    local cut = CUTS[lane + 1]
    local c = LANE_COLORS[lane + 1]
    local px, py = project_cut(L, cut, lane, 0)
    if pressed then poly(px, py, cut.rim, with_a(c, 0.35)) end
    loop(px, py, cut.rim, c)
    loop(px, py, cut.outline, shade(c, 0.6, 1))
    if fl > 0 then
        local qx, qy = project_cut(L, cut, lane, 0, 1 + (1 - fl) * 0.7)
        poly(qx, qy, cut.rim, with_a(c, fl * 0.45))
        loop(qx, qy, cut.rim, shade(c, 1.5, fl))
    end
end

-- crowd ---------------------------------------------------------------------

local crowd = { w = 0, h = 0, people = {}, flashes = {} }
local SKIN = {
    { r = 240, g = 200, b = 170 }, { r = 210, g = 160, b = 120 }, { r = 170, g = 120, b = 85 },
    { r = 120, g = 80,  b = 55 }, { r = 85, g = 58, b = 40 },
}

local function hsv(h, s, v)
    h = (h % 1) * 6
    local i = math.floor(h)
    local f = h - i
    local p, q, t = v * (1 - s), v * (1 - s * f), v * (1 - s * (1 - f))
    local r, g, b = v, t, p
    if i == 1 then
        r, g, b = q, v, p
    elseif i == 2 then
        r, g, b = p, v, t
    elseif i == 3 then
        r, g, b = p, q, v
    elseif i == 4 then
        r, g, b = t, p, v
    elseif i == 5 then
        r, g, b = v, p, q
    end
    return { r = r * 255, g = g * 255, b = b * 255 }
end

local function build_crowd(width, height)
    crowd.w, crowd.h, crowd.flashes = width, height, {}
    local people = {}
    local rows = 10
    local unit = height / 34
    local top, bottom = 0.2, 0.97
    local gap = height * (bottom - top) / (rows - 1)
    for r = 1, rows do
        local t = (r - 1) / (rows - 1)
        local s = 0.4 + t * 0.7
        local y = height * (top + t * (bottom - top))
        local step = unit * 2.3 * s
        local x = -step * math.random()
        while x < width + step do
            people[#people + 1] = {
                x = x + (math.random() - 0.5) * step * 0.4,
                y = y + (math.random() - 0.5) * unit * 1.6 * s,
                body_len = gap * 1.6, -- down past the next row's shoulders, so nobody floats
                s = s * (0.85 + math.random() * 0.3),
                shirt = hsv(math.random(), 0.55, 1),
                skin = SKIN[math.random(1, #SKIN)],
                offbeat = math.random() < 0.3 and 0.5 or 0, -- some bounce on the "and"
                zeal = 0.35 + math.random() * 0.65,
                wave = math.random() * 6.28,
                lighter = math.random() < 0.25,
            }
            x = x + step
        end
    end
    crowd.people, crowd.unit = people, unit
end

local function update_hype(target)
    hype = hype + (target - hype) * math.min(1, DT * 1.5)
end

local function draw_backdrop(width, height, beat_pos)
    if crowd.w ~= width or crowd.h ~= height then build_crowd(width, height) end
    local pulse = 1 + 0.35 * hype * (1 - beat_pos % 1) ^ 2

    -- sky: dark violet fading to black
    local bands = 14
    for i = 0, bands - 1 do
        local t = i / bands
        local c = hsv(0.76 - t * 0.08, 0.7, (0.1 + 0.08 * hype) * (1 - t) * pulse)
        rect(0, height * t, width, height * (t + 1 / bands) + 1, c)
    end

    -- stage lights sweeping over the crowd
    for i = 1, 4 do
        local ang = math.sin(TIME * (0.35 + i * 0.11) + i * 1.7) * 0.6
        local x0 = width * (i - 0.5) / 4
        local len = height * 1.2
        local spread = 0.12
        local c = hsv(i * 0.23 + TIME * 0.02, 0.8, 1)
        c.a = 0.04 + 0.1 * hype * pulse
        triangle(x0, -5, x0 + math.sin(ang - spread) * len, math.cos(ang - spread) * len,
            x0 + math.sin(ang + spread) * len, math.cos(ang + spread) * len, c)
    end

    -- people, back row first
    local light = 0.3 + 0.55 * hype
    for _, pp in ipairs(crowd.people) do
        local u = crowd.unit * pp.s
        local energy = hype * pp.zeal
        local ph = (beat_pos + pp.offbeat) % 1
        local jump = (1 - ph) ^ 3 * u * 1.8 * math.max(0, energy - 0.2)
        local x = pp.x + math.sin(beat_pos * math.pi + pp.wave) * u * 0.35 * energy
        local y = pp.y - jump
        local depth = 0.55 + 0.45 * pp.s -- back rows sit in the dark
        local body = shade(pp.shirt, 0.18 + 0.22 * light * depth, 1)
        local head = shade(pp.skin, 0.25 + 0.4 * light * depth, 1)
        if energy > 0.4 then -- arms up, waving
            local wave = math.sin(TIME * 5 + pp.wave) * u * 0.5 * energy
            local reach = u * (1.6 + energy)
            local hx1, hy1 = x - u * 1.2 + wave, y - reach
            local hx2, hy2 = x + u * 1.2 + wave, y - reach
            local aw = u * 0.2
            polygon({ x - u * 0.75, y + u * 0.3, x - u * 0.35, y + u * 0.3, hx1 + aw, hy1, hx1 - aw, hy1 }, body)
            polygon({ x + u * 0.35, y + u * 0.3, x + u * 0.75, y + u * 0.3, hx2 + aw, hy2, hx2 - aw, hy2 }, body)
            circle(hx1, hy1, aw * 1.3, head)
            circle(hx2, hy2, aw * 1.3, head)
            if pp.lighter and hype > 0.7 then
                circle(hx2, hy2 - u * 0.2, math.max(1, u * 0.22), { r = 255, g = 230, b = 150, a = (hype - 0.7) * 3 })
            end
        end
        local yb = pp.y + pp.body_len
        polygon({ x - u * 0.6, y - u * 0.1, x + u * 0.6, y - u * 0.1, x + u * 0.9, y + u * 0.4,
            x + u * 0.8, yb, x - u * 0.8, yb, x - u * 0.9, y + u * 0.4 }, body)
        circle(x, y - u * 0.75, u * 0.62, head)
    end

    -- camera flashes when it's really going off
    if hype > 0.75 and math.random() < (hype - 0.75) * 30 * DT then
        local pp = crowd.people[math.random(1, #crowd.people)]
        crowd.flashes[#crowd.flashes + 1] = { x = pp.x, y = pp.y - crowd.unit * pp.s * 2, life = 0.12 }
    end
    for i = #crowd.flashes, 1, -1 do
        local f = crowd.flashes[i]
        f.life = f.life - DT
        if f.life <= 0 then
            table.remove(crowd.flashes, i)
        else
            circle(f.x, f.y, crowd.unit * 0.6, { r = 255, g = 255, b = 255, a = f.life / 0.12 })
        end
    end
end

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
    acc_ema = acc_ema * 0.92 + 0.08
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
    acc_ema = acc_ema * 0.92
    hype = math.max(0, hype - 0.04)
    popup = { text = "MISS", life = 0.5, col = { r = 255, g = 90, b = 90 } }
    if mute then set_muted(g.ch) end
end

-- Whether the frets held (`held[lane]`) play the chord starting at gem `first`: exactly its
-- frets for a chord; for a single note its fret, and none above it (lower ones may be held).
local function frets_match(gems, first, held)
    local lanes, top = {}, -1
    local k = first
    while gems[k] and gems[k].group == first do
        lanes[gems[k].lane] = true
        top = math.max(top, gems[k].lane)
        k = k + 1
    end
    local single = k - first == 1
    for lane = 0, chart.lanes - 1 do
        if lanes[lane] and not held[lane] then return false end
        if held[lane] and not lanes[lane] and (not single or lane > top) then return false end
    end
    return true
end

-- GUITAR mode: frets are held, notes are strummed (HOPOs and taps can be fretted).
local function update_guitar(p, gems, nowj, win, pspeed, L, ghost_penalty)
    local held, changed = {}, false
    for lane = 0, chart.lanes - 1 do
        held[lane] = input_down(LANE_INPUT[lane + 1])
        local st = input(LANE_INPUT[lane + 1])
        if st == "pressed" or st == "released" then changed = true end
    end
    local strum = input(STRUM) == "pressed"
    if p.paused then return end
    -- the next chord still to play, in the window
    local first = nil
    for i = first_live, #gems do
        local g = gems[i]
        if g.t > nowj + win then break end
        if not g.state and math.abs(g.t - nowj) <= win then
            first = g.group or i
            break
        end
    end
    local function play(start)
        local k = start
        while gems[k] and gems[k].group == start do
            if not gems[k].state then hit(gems[k], nowj, pspeed, L) end
            k = k + 1
        end
    end
    if first then
        local kind = gems[first].kind
        local fretted = changed and (kind == "tap" or (kind == "hopo" and stats.combo > 0))
        if fretted and frets_match(gems, first, held) then
            play(first)
            fret_hit_at = game_time
            return
        end
        if strum then
            if frets_match(gems, first, held) then
                play(first)
            elseif ghost_penalty then
                stats.combo = 0 -- strummed the wrong frets
            end
        end
    elseif strum and ghost_penalty and game_time - fret_hit_at > 0.12 * pspeed then
        stats.combo = 0 -- strummed nothing
    end
end

local function update_play(p, pspeed, L, offset, mute_on_miss, ghost_penalty)
    local gems = chart.gems
    local nowj = game_time - offset * pspeed
    local win = WIN_GOOD * pspeed

    if guitar then update_guitar(p, gems, nowj, win, pspeed, L, ghost_penalty) end
    for lane = 0, guitar and -1 or chart.lanes - 1 do
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

    local show_crowd = setting_bool("crowd", true)
    if show_crowd then
        draw_backdrop(width, height, (game_time >= 0 and beat(game_time)) or TIME * 2)
    end

    -- road
    polygon({
        x_of(L, 0, zt), y_of(L, zt), x_of(L, lanes, zt), y_of(L, zt),
        x_of(L, lanes, L.zb), height, x_of(L, 0, L.zb), height,
    }, { r = 18, g = 18, b = 28, a = show_crowd and setting_float("road_opacity", 0.85, 0.3, 1) or 1 })

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

    -- hit line and sockets
    rect(x_of(L, 0, 0), L.hit_y - 1, x_of(L, lanes, 0), L.hit_y + 1, { r = 200, g = 200, b = 230, a = 0.6 })
    for lane = 0, lanes - 1 do
        draw_socket(L, lane, input_down(LANE_INPUT[lane + 1]), flash[lane] or 0)
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
            local missed = g.state == "miss"
            draw_gem(L, g.lane, z, missed and { r = 85, g = 85, b = 95 } or c, missed and 0.6 or 1, i * 2.3)
            if guitar and not missed and (g.kind == "hopo" or g.kind == "tap") then
                local cut = CUTS[g.lane + 1]
                local px, py = project_cut(L, cut, g.lane, z, 0.55)
                poly(px, py, cut.table, g.kind == "hopo" and { r = 255, g = 255, b = 255, a = 0.9 }
                    or { r = 15, g = 15, b = 25, a = 0.9 })
            end
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
    if play_track.medley then
        local parts = medley_for(play_track, sel_diff).parts
        local now_part, next_part = part_at(parts, game_time), part_at(parts, game_time + look)
        if now_part then
            local s = "playing " .. chan_label(now_part.ch)
            if next_part and next_part.ch ~= now_part.ch then s = s .. "  then " .. chan_label(next_part.ch) end
            w = text_size(s, th)
            text(width - w - 12, 12 + th * 3, s, { r = 120, g = 200, b = 255 }, th)
        end
    end

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
        if clicked and over and sel_track ~= i then
            sel_track = i
            apply_solo()
        end
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
        local sub = #ch.gems .. " gems"
        if tr.medley then
            local pt = part_at(medley_for(tr, sel_diff).parts, preview_t)
            if pt then sub = chan_label(pt.ch) end
        end
        text(sx + 6, ytop + 4 + hsc, sub, dim, hsc)

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
    local bw, bh = math.min(th * 6, (width - 32 - 8 * 7) / 8), th * 1.5
    local total = bw * 8 + 8 * 7
    local bx = (width - total) / 2
    local by = height - th * 3.2
    for i = 1, 8 do
        local x = bx + (i - 1) * (bw + 8)
        local over = mx and mx >= x and mx < x + bw and my >= by and my < by + bh
        local label, bg
        if i <= 4 then
            label = DIFFS[i].name
            bg = i == sel_diff and { r = 90, g = 70, b = 20 } or { r = 30, g = 30, b = 44 }
        elseif i == 5 then
            label = "SPEED " .. math.floor(SPEEDS[speed_idx] * 100) .. "%"
            bg = speed_idx > 1 and { r = 30, g = 60, b = 100 } or { r = 30, g = 30, b = 44 }
        elseif i == 6 then
            label = "SOLO"
            bg = solo and { r = 110, g = 50, b = 110 } or { r = 30, g = 30, b = 44 }
        elseif i == 7 then
            label = "GUITAR"
            bg = guitar and { r = 120, g = 60, b = 20 } or { r = 30, g = 30, b = 44 }
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
            elseif i == 6 then
                solo = not solo
                apply_solo()
            elseif i == 7 then
                guitar = not guitar
                store_set("guitar", guitar)
            else
                start_game()
            end
        end
    end

    local list = playlist()
    local songs = (list and #list.entries > 1) and "   N/B song" or ""
    local hint = has_focus() and ("LEFT/RIGHT track   UP/DOWN difficulty   S speed   I solo   G guitar   SCROLL seek"
        .. "   SPACE pause" .. songs .. "   ENTER to start")
        or "click here first so the keys reach the game"
    if list and list.current then
        local where = string.format("%s  %d/%d", list.name, list.current, #list.entries)
        local w = text_size(where, th)
        text(width - w - 16, 12 + th * 0.8, where, dim, th)
    end
    centered(hint, width / 2, height - th * 1.4, dim, FONT_HEIGHT * math.max(1, math.floor(th / FONT_HEIGHT)))
end

local function draw_results(width, height, th)
    if setting_bool("crowd", true) then
        draw_backdrop(width, height, TIME * 2)
        rect(0, 0, width, height, { r = 0, g = 0, b = 0, a = 0.55 })
    end
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
    local lanes = lane_count()
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
        for _, ch in ipairs(store_get("solo_muted") or {}) do set_channel_enabled(ch, true) end
        store_set("solo_muted", nil)
        borrowed = store_get("borrowed")
    end
    if state == "init" or p.song_loads ~= last_loads then
        -- a song was loaded (the same one again counts): back to the menu, its preview
        -- playing from the selected track (solo cleared: the channels are new ones)
        local first = state == "init"
        last_loads = p.song_loads
        if p.song_id ~= song_key or first then
            song_key = p.song_id
            state = "init"
            apply_solo()
            analyze(p)
        end
        enter_menu()
    end

    local focus = has_focus()

    if state == "play" and stats.combo then
        update_hype(0.15 + 0.45 * acc_ema ^ 2 + 0.4 * math.min(1, stats.combo / 40))
    elseif state == "results" then
        local judged = stats.perfect + stats.good + stats.miss
        update_hype(judged > 0 and ((stats.perfect + stats.good) / judged) ^ 2 or 0.3)
    else
        update_hype(0.4)
    end

    -- changing song: the new one loads paused, and the song_loads check brings up its menu
    if state == "menu" or state == "results" then
        local list = playlist()
        if list and #list.entries > 1 then
            if input(NEXT_SONG) == "pressed" then next_track() end
            if input(PREV_SONG) == "pressed" then previous_track() end
        end
    end

    if state == "menu" then
        if #tracks > 0 then
            local picked = sel_track
            if input(PREV) == "pressed" then sel_track = (sel_track - 2) % #tracks + 1 end
            if input(NEXT) == "pressed" then sel_track = sel_track % #tracks + 1 end
            if input(HARDER) == "pressed" then sel_diff = math.min(4, sel_diff + 1) end
            if input(EASIER) == "pressed" then sel_diff = math.max(1, sel_diff - 1) end
            if input(SPEED) == "pressed" then speed_idx = speed_idx % #SPEEDS + 1 end
            if input(SOLO) == "pressed" then solo = not solo end
            if input(GUITAR) == "pressed" then
                guitar = not guitar
                store_set("guitar", guitar)
            end
            if picked ~= sel_track or input(SOLO) == "pressed" then apply_solo() end
            if input(PAUSE) == "pressed" then set_paused(not p.paused) end
            -- The preview is the song itself: scrolling seeks (a few times a second at most,
            -- the strips following at once), and the end goes round to the track again.
            local _, sy = scroll()
            if sy ~= 0 then
                seek_to = math.max(0, math.min((seek_to or p.position) - sy * 0.02, (p.length or 0) - 0.1))
            end
            if seek_to and TIME - last_seek > 0.08 then
                seek(seek_to)
                last_seek, seek_to = TIME, nil
            end
            if p.finished then
                seek(preview_start())
                set_paused(false)
            end
            preview_t = seek_to or p.position
            draw_menu(width, height, th, lanes, pspeed, look)
            if state == "menu" and input(START) == "pressed" then
                start_game()
            end
        else
            draw_menu(width, height, th, lanes, pspeed, look)
        end
    elseif state == "results" then
        draw_results(width, height, th)
        if input(START) == "pressed" or mouse_pressed("left") then enter_menu() end
    else
        local L = game_layout(width, height, chart.lanes)

        if input(BACK) == "pressed" then
            enter_menu()
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
