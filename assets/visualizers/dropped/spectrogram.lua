-- Spectrogram: a scrolling time/frequency heatmap built from the same FFT
-- data fft.lua uses, colored along a gradient from a dark background through
-- an accent color to white for the hottest peaks. Both colors, the
-- frequency resolution and the time window are editable live via the
-- Script Settings tab, so "a little blocky" is a slider away rather than an edit.
--
-- Like waveform.lua's ring buffer, history is kept in a fixed-size ring of
-- already-quantized columns rather than a growing array. Unlike a line plot
-- though, a heatmap needs one draw call per colored cell, and the pixel
-- buffer is cleared before every `render` call so every visible column has
-- to be redrawn every frame -- so column count and bin count are both kept
-- modest, equal-colored bins within a column are run-length-merged into one
-- rect, and bins at the bottom of the color scale (i.e. matching the
-- background `clear` already painted) are skipped entirely.
--
-- Writing a new column is gated on `playback().paused`, so pausing freezes
-- the picture instead of scrolling through a flat, unchanging spectrum (the
-- draw loop below still runs every frame regardless, so the frozen history
-- stays visible, not blank). There's deliberately no equivalent check for
-- looping -- nothing here resets `history` on a seek, so a loop restart just
-- keeps scrolling through it like any other moment in the song, not a wipe.

local MIN_FREQUENCY_HZ = 30.0
local MAX_FREQUENCY_HZ = 16000.0
local MIN_DB = -60.0
local MAX_DB = 0.0
local QUANT_LEVELS = 24
-- Exponential smoothing per bin between frames: higher = smoother but laggier.
local SMOOTHING = 0.45

local function lerp(a, b, t)
    return a + (b - a) * t
end

local function build_palette(bg, accent)
    local palette = {}
    for level = 0, QUANT_LEVELS do
        local v = level / QUANT_LEVELS
        if v < 0.5 then
            local t = v / 0.5
            palette[level] =
                { r = lerp(bg.r, accent.r, t), g = lerp(bg.g, accent.g, t), b = lerp(bg.b, accent.b, t) }
        else
            local t = (v - 0.5) / 0.5
            palette[level] =
                { r = lerp(accent.r, 255, t), g = lerp(accent.g, 255, t), b = lerp(accent.b, 255, t) }
        end
    end
    return palette
end

local function colors_equal(a, b)
    return a ~= nil and b ~= nil and a.r == b.r and a.g == b.g and a.b == b.b
end

local function freq_at(t)
    return MIN_FREQUENCY_HZ * (MAX_FREQUENCY_HZ / MIN_FREQUENCY_HZ) ^ t
end

local function bin_for_freq(freq, fft_size)
    return math.floor((freq / SAMPLE_RATE) * fft_size + 0.5)
end

-- Peak magnitude (0..1) within the log-spaced frequency band row `row`
-- covers; row 1 is the highest frequency, row `freq_bins` the lowest.
local function row_unit_value(spectrum, row, freq_bins)
    if #spectrum == 0 then
        return 0.0
    end
    local fft_size = #spectrum * 2
    local t_hi = 1.0 - (row - 1) / freq_bins
    local t_lo = 1.0 - row / freq_bins
    local lo = math.max(1, math.min(bin_for_freq(freq_at(t_lo), fft_size), #spectrum - 1))
    local hi = math.max(lo + 1, math.min(bin_for_freq(freq_at(t_hi), fft_size), #spectrum))

    local peak = 0.0
    for i = lo, hi do
        if spectrum[i] and spectrum[i] > peak then
            peak = spectrum[i]
        end
    end

    local db = 20.0 * math.log(math.max(peak, 1e-6), 10)
    return math.max(0.0, math.min(1.0, (db - MIN_DB) / (MAX_DB - MIN_DB)))
end

-- Ring buffer of the last `column_count` columns, each a `freq_bins`-length
-- array of quantized levels (0..QUANT_LEVELS). Rebuilt from scratch whenever
-- freq_bins/column_count change, since old columns don't match the new shape.
local history, write_pos, count, smoothed = {}, 1, 0, {}
local built_bins, built_columns = nil, nil

-- Rebuilt only when the setting colors actually change, not every frame.
local palette, palette_bg, palette_accent = nil, nil, nil

function render(width, height, left, right)
    local bg = setting_color("background", { r = 16, g = 16, b = 24 })
    local accent = setting_color("accent", { r = 51, g = 204, b = 255 })
    local freq_bins = setting_int("freq_bins", 36, 8, 96)
    local column_count = setting_int("columns", 140, 40, 400)

    if not colors_equal(bg, palette_bg) or not colors_equal(accent, palette_accent) then
        palette = build_palette(bg, accent)
        palette_bg, palette_accent = bg, accent
    end

    if freq_bins ~= built_bins or column_count ~= built_columns then
        history, write_pos, count = {}, 1, 0
        smoothed = {}
        for i = 1, freq_bins do
            smoothed[i] = 0.0
        end
        built_bins, built_columns = freq_bins, column_count
    end

    if not playback().paused then
        local left_spectrum = fft_left(left)
        local right_spectrum = fft_right(right)

        local column = {}
        for row = 1, freq_bins do
            local value = math.max(
                row_unit_value(left_spectrum, row, freq_bins),
                row_unit_value(right_spectrum, row, freq_bins)
            )
            smoothed[row] = smoothed[row] * SMOOTHING + value * (1 - SMOOTHING)
            column[row] = math.floor(smoothed[row] * QUANT_LEVELS + 0.5)
        end

        history[write_pos] = column
        write_pos = write_pos % column_count + 1
        if count < column_count then
            count = count + 1
        end
    end

    clear(bg)

    local shown = math.min(column_count, count)
    local col_w = width / column_count
    local row_h = height / freq_bins

    for c = 1, shown do
        -- Walk backward from the most recently pushed column, so column
        -- `shown` (the right edge) is "now".
        local offset = shown - c
        local index = ((write_pos - 2 - offset) % column_count) + 1
        local col = history[index]
        local x0 = (c - 1) * col_w
        local x1 = c * col_w

        -- Run-length merge consecutive rows at the same quantized level into
        -- one rect each. Looping one past freq_bins (where col[row] is nil)
        -- flushes the final run without a separate closure/call per column.
        local run_start = 1
        local run_level = col[1]
        for row = 2, freq_bins + 1 do
            local level = col[row]
            if level ~= run_level then
                if run_level > 0 then
                    rect(x0, (run_start - 1) * row_h, x1, (row - 1) * row_h, palette[run_level])
                end
                run_start = row
                run_level = level
            end
        end
    end
end
