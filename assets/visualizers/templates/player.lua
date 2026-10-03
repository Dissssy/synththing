-- Player: a music player drawn by a script, all mouse. Not a visualizer
-- so much as a reference for driving playback: the current playlist with
-- the playing song lit up (click one to play it), previous / play-pause /
-- next buttons, a progress bar to click to seek, and loop and shuffle.
--
-- It sees only the current playlist (the one playing, or the one open in
-- the Playlists tab) and can only pick songs from it.

local TEXT = { r = 220, g = 222, b = 230 }
local DIM = { r = 120, g = 124, b = 140 }
local ACCENT = { r = 255, g = 200, b = 70 }
local PANEL = { r = 30, g = 32, b = 42 }
local HOVER = { r = 46, g = 49, b = 64 }

local function clock(seconds)
    if not seconds then return "--:--" end
    return string.format("%d:%02d", seconds // 60, math.floor(seconds % 60))
end

-- A button: draws itself, returns true when clicked this frame.
local function button(x, y, w, h, label, lit)
    local mx, my = mouse()
    local over = mx and mx >= x and mx < x + w and my >= y and my < y + h
    rect(x, y, x + w, y + h, over and HOVER or PANEL)
    local tw = text_size(label, FONT_HEIGHT * 2)
    text(x + (w - tw) / 2, y + (h - FONT_HEIGHT * 2) / 2, label, lit and ACCENT or TEXT, FONT_HEIGHT * 2)
    return over and mouse_pressed()
end

function render(width, height, left, right)
    clear({ r = 18, g = 19, b = 26 })
    local p = playback()
    local list = playlist()
    local margin = 16
    local line = FONT_HEIGHT * 2

    -- Now playing and the progress bar (click to seek).
    text(margin, margin, p.song_name or "Nothing playing", TEXT, line)
    local bar_y = margin + line + 10
    local bar_w = width - margin * 2
    rect(margin, bar_y, margin + bar_w, bar_y + 8, PANEL)
    if p.length > 0 then
        rect(margin, bar_y, margin + bar_w * p.position / p.length, bar_y + 8, ACCENT)
        local mx, my = mouse()
        if mouse_pressed() and mx and my >= bar_y - 6 and my <= bar_y + 14 and mx >= margin and mx <= margin + bar_w then
            seek((mx - margin) / bar_w * p.length)
        end
    end
    text(margin, bar_y + 14, clock(p.position) .. " / " .. clock(p.length), DIM, FONT_HEIGHT)

    -- Transport.
    local by = bar_y + 36
    local bw, bh = 72, 40
    if button(margin, by, bw, bh, "<<") then previous_track() end
    if button(margin + bw + 8, by, bw, bh, p.paused and ">" or "||") then set_paused(not p.paused) end
    if button(margin + (bw + 8) * 2, by, bw, bh, ">>") then next_track() end
    local modes = { off = "one", one = "all", all = "off" }
    local loop_label = (p.loop_mode == "one") and "loop 1" or "loop"
    if button(margin + (bw + 8) * 3 + 16, by, bw + 24, bh, loop_label, p.loop_mode ~= "off") then
        set_loop(modes[p.loop_mode])
    end
    if button(margin + (bw + 8) * 4 + 40, by, bw + 48, bh, "shuffle", p.shuffle) then
        set_shuffle(not p.shuffle)
    end

    -- The playlist: click a song to play it.
    local top = by + bh + 20
    if not list then
        text(margin, top, "No playlist: make one in View > Playlists.", DIM, line)
        return
    end
    text(margin, top, list.name .. (list.playing and "" or "  (not playing)"), DIM, line)
    local row_h = line + 6
    local rows = math.max(1, math.floor((height - top - line - margin) / row_h))
    -- Keep the playing song in view.
    local first = math.max(1, math.min((list.current or 1) - rows // 2, #list.entries - rows + 1))
    local mx, my = mouse()
    for i = first, math.min(#list.entries, first + rows - 1) do
        local song = list.entries[i]
        local y = top + line + 4 + (i - first) * row_h
        local over = mx and my >= y and my < y + row_h and mx >= margin and mx < width - margin
        if over then rect(margin, y, width - margin, y + row_h, HOVER) end
        local color = song.missing and DIM or ((i == list.current) and ACCENT or TEXT)
        text(margin + 8, y + 3, string.format("%2d  %s", i, song.name), color, line)
        local length = clock(song.length)
        text(width - margin - 8 - text_size(length, line), y + 3, length, DIM, line)
        if over and mouse_pressed() and not song.missing then
            play_track(i)
        end
    end
end
