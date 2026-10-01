-- Waveform: a scrolling oscilloscope trace. Left and right are averaged to
-- mono and the most recent samples are drawn as a connected line, one column
-- per sample.
--
-- History is kept in a fixed-size ring buffer (CAPACITY slots, a write
-- cursor, a count) rather than a growing/shrinking array, inserting or
-- removing from the front of a plain Lua array is O(n) per call, and doing
-- that every frame is the easiest way to blow a 60fps budget. Pushing into a
-- ring buffer is O(1) no matter how long the visualizer has been running.

local CAPACITY = 4096
local history = {}
local count = 0
local write_pos = 1

local function push(value)
    history[write_pos] = value
    write_pos = write_pos % CAPACITY + 1
    if count < CAPACITY then
        count = count + 1
    end
end

function render(width, height, left, right)
    -- Nothing new arrives while paused (the tap only has samples that
    -- actually played), so this loop is naturally a no-op then, the
    -- buffer just stops scrolling on its own, no pause check needed here.
    for i = 1, #left do
        push((left[i] + right[i]) * 0.5)
    end

    local background = setting_color("background", { r = 16, g = 16, b = 24 })
    local line_color = setting_color("line", { r = 51, g = 204, b = 255 })

    clear(background)

    local mid = height / 2
    local amplitude = height * 0.45
    local shown = math.min(width, count)
    local prev_x, prev_y = nil, nil

    for x = 1, shown do
        -- Walk backward from the most recently pushed sample, so column
        -- `shown` (the right edge) is "now" and column 1 is the oldest shown.
        local offset = shown - x
        local index = ((write_pos - 2 - offset) % CAPACITY) + 1
        local sample = history[index]

        local clamped = math.max(-1.0, math.min(1.0, sample))
        local y = mid - clamped * amplitude

        if prev_x then
            line(prev_x, prev_y, x, y, line_color)
        end
        prev_x, prev_y = x, y
    end
end
