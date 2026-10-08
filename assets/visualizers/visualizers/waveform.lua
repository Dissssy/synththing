-- Waveform: a scrolling oscilloscope trace. Left and right are averaged to
-- mono and the most recent stretch of sound is drawn as a connected line
-- across the window.
--
-- The app keeps the last few seconds of samples itself: history_left and
-- history_right hand back as many as there are columns to draw, spread over
-- the time shown, so there's no ring buffer to keep here, and no more
-- numbers cross into Lua each frame than get drawn. While paused nothing new
-- arrives, so the trace just holds still.
--
-- Trails, like an old phosphor scope, come from set_clear_color: instead of
-- wiping each frame, the background is blended over the last one, so old
-- traces fade into it.

function render(width, height, left, right)
    local background = setting_color("background", { r = 16, g = 16, b = 24 })
    local line_color = setting_color("line", { r = 51, g = 204, b = 255 })
    local seconds = setting_float("seconds", 0.025, 0.002, 4,
        { info = "How much sound is across the window, in seconds" })
    local thickness = setting_float("thickness", 1, 1, 12, { info = "How wide the trace is, in pixels" })
    local trail = setting_float("trail", 0, 0, 0.98,
        { info = "How long old traces linger, fading into the background: 0 for none" })

    -- trail is how much of the last frame is left at 60 fps; raised to
    -- DT * 60 it fades as fast at any frame rate.
    local left_over = trail ^ (math.max(DT, 1 / 60) * 60)
    set_clear_color({ r = background.r, g = background.g, b = background.b, a = 1 - left_over })

    -- One point per column, or one per sample when the time shown is
    -- shorter than the window is wide.
    local count = math.max(2, math.min(width, math.floor(seconds * SAMPLE_RATE + 0.5)))
    local l = history_left(seconds, count)
    local r = history_right(seconds, count)

    local mid = height / 2
    local amplitude = height * 0.45
    local step = (width - 1) / (#l - 1)
    local prev_x, prev_y = nil, nil

    for i = 1, #l do
        local sample = (l[i] + r[i]) * 0.5
        local clamped = math.max(-1.0, math.min(1.0, sample))
        local x = 1 + (i - 1) * step
        local y = mid - clamped * amplitude

        if prev_x then
            line(prev_x, prev_y, x, y, line_color, thickness)
        end
        prev_x, prev_y = x, y
    end
end
