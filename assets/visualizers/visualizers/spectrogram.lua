-- Spectrogram: a scrolling time/frequency heatmap built from the same FFT
-- data fft.lua uses, colored along a gradient from a dark background through
-- an accent color to white for the hottest peaks. Both colors, the
-- frequency resolution and the time window are editable live in the
-- Settings window, so "a little blocky" is a slider away rather than an edit.
--
-- History is kept in a fixed-size ring of already-quantized columns rather
-- than a growing array. The picture itself is kept from frame to frame
-- (set_clear_color with a = 0) and scrolled one column left each frame
-- (shift_frame), so only the new column is drawn, at the right edge; for
-- that, columns are whole pixels wide (the "columns" setting picks about how
-- many fit across). The picture's drawn afresh from the history only when it
-- has to be: the first frame, a new size, or a setting changed. Within a
-- column, equal-colored bins are run-length-merged into one rect, and bins
-- at the bottom of the color scale (matching the background) are skipped.
--
-- Writing a new column is gated on `playback().paused`, so pausing freezes
-- the picture instead of scrolling through a flat, unchanging spectrum (the
-- draw loop below still runs every frame regardless, so the frozen history
-- stays visible, not blank). There's deliberately no equivalent check for
-- looping -- nothing here resets `history` on a seek, so a loop restart just
-- keeps scrolling through it like any other moment in the song, not a wipe.
--
-- Frequency labels down the left edge (100 Hz, 1k, 10k, ...) use the same
-- log mapping as the rows. They sit over the picture, so the strip under
-- them is drawn again from the history every frame before they go on top
-- (scrolled along with the rest, they'd smear). In the mini player there are
-- fewer of them. With "onset_ticks" on, a small tick along the top marks
-- every column where onset() saw a sound start, scrolling along with the
-- picture.

local MIN_FREQUENCY_HZ = 30.0
local MAX_FREQUENCY_HZ = 16000.0
local MIN_DB = -60.0
local MAX_DB = 0.0
local QUANT_LEVELS = 24
-- Exponential smoothing per bin between frames: higher = smoother but laggier.
local SMOOTHING = 0.45

local function build_palette(bg, accent)
    local palette = {}
    for level = 0, QUANT_LEVELS do
        local v = level / QUANT_LEVELS
        if v < 0.5 then
            local t = v / 0.5
            palette[level] = mix(bg, accent, t)
        else
            local t = (v - 0.5) / 0.5
            palette[level] = mix(accent, { r = 255, g = 255, b = 255 }, t)
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

-- Peak magnitude (0..1) within the log-spaced frequency band row `row`
-- covers; row 1 is the highest frequency, row `freq_bins` the lowest.
local function row_unit_value(spectrum, row, freq_bins)
    local t_hi = 1.0 - (row - 1) / freq_bins
    local t_lo = 1.0 - row / freq_bins
    local peak = fft_band(spectrum, freq_at(t_lo), freq_at(t_hi))
    local db = 20.0 * math.log(math.max(peak, 1e-6), 10)
    return math.max(0.0, math.min(1.0, (db - MIN_DB) / (MAX_DB - MIN_DB)))
end

-- Ring buffer of the last `column_count` columns, each a `freq_bins`-length
-- array of quantized levels (0..QUANT_LEVELS). Rebuilt from scratch whenever
-- freq_bins/column_count change, since old columns don't match the new shape.
local history, write_pos, count, smoothed = {}, 1, 0, {}
local onsets = {} -- per history slot: true if a sound started that column
-- Rebuilt only when the setting colors actually change, not every frame.
local palette, palette_bg, palette_accent = nil, nil, nil
local LABEL_FREQS = { 50, 100, 200, 500, 1000, 2000, 5000, 10000 }
local MINI_LABEL_FREQS = { 100, 1000, 10000 }
-- How wide the strip under the labels is, redrawn every frame.
local LABEL_STRIP = 44
-- What the kept picture was drawn for (its size and settings); anything
-- else and it's drawn afresh.
local drawn_for = nil

-- The column `back` columns before the newest (0 is the newest), from the
-- history ring.
local function column_back(back, column_count)
    return ((write_pos - 2 - back) % column_count) + 1
end

-- Draw history column `index` between x0 and x1.
local function draw_column(index, x0, x1, row_h, freq_bins, show_onsets)
    local col = history[index]
    if not col then
        return
    end
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
    if show_onsets and onsets[index] then
        rect(x0, 0, x1, math.max(3, row_h * 0.6), { r = 255, g = 255, b = 255, a = 0.85 })
    end
end
local built_bins, built_columns = nil, nil


function render(width, height, left, right)
    local bg = setting_color("background", { r = 16, g = 16, b = 24 })
    local accent = setting_color("accent", { r = 51, g = 204, b = 255 })
    local freq_bins = setting_int("freq_bins", 36, 8, 96)
    local column_count = setting_int("columns", 140, 40, 400)
    local show_labels = setting_bool("frequency_labels", true)
    local show_onsets = setting_bool("onset_ticks", false)

    if not colors_equal(bg, palette_bg) or not colors_equal(accent, palette_accent) then
        palette = build_palette(bg, accent)
        palette_bg, palette_accent = bg, accent
    end

    if freq_bins ~= built_bins or column_count ~= built_columns then
        history, write_pos, count = {}, 1, 0
        onsets = {}
        smoothed = {}
        for i = 1, freq_bins do
            smoothed[i] = 0.0
        end
        built_bins, built_columns = freq_bins, column_count
    end

    local new_column = false
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
        onsets[write_pos] = show_onsets and onset() or false
        write_pos = write_pos % column_count + 1
        if count < column_count then
            count = count + 1
        end
        new_column = true
    end

    -- Kept from frame to frame; what shift_frame uncovers is the background.
    set_clear_color({ r = bg.r, g = bg.g, b = bg.b, a = 0 })
    local col_w = math.max(1, math.floor(width / column_count + 0.5))
    local row_h = height / freq_bins
    local mode = display_mode()
    local key = table.concat({ width, height, freq_bins, column_count, bg.r, bg.g, bg.b,
        accent.r, accent.g, accent.b, tostring(show_onsets), tostring(show_labels), mode }, ",")
    if key ~= drawn_for then
        -- Afresh: every column there is, newest at the right edge.
        drawn_for = key
        clear(bg)
        for back = 0, count - 1 do
            local x1 = width - back * col_w
            if x1 <= 0 then
                break
            end
            draw_column(column_back(back, column_count), x1 - col_w, x1, row_h, freq_bins, show_onsets)
        end
    elseif new_column then
        -- One column on: everything moves left, and the new one goes in.
        shift_frame(-col_w, 0)
        draw_column(column_back(0, column_count), width - col_w, width, row_h, freq_bins, show_onsets)
    end

    if show_labels then
        -- The strip under the labels, drawn again from the history (the
        -- last frame's labels have scrolled into it), then the labels.
        rect(0, 0, LABEL_STRIP, height, bg)
        clip(0, 0, LABEL_STRIP, height)
        for back = 0, count - 1 do
            local x1 = width - back * col_w
            if x1 <= 0 then
                break
            end
            if x1 - col_w < LABEL_STRIP then
                draw_column(column_back(back, column_count), x1 - col_w, x1, row_h, freq_bins, show_onsets)
            end
        end
        clip()

        -- Row 1 is the top (highest frequency): a frequency's height is how
        -- far up the log scale it sits.
        local span = math.log(MAX_FREQUENCY_HZ / MIN_FREQUENCY_HZ)
        for _, f in ipairs(mode == "mini" and MINI_LABEL_FREQS or LABEL_FREQS) do
            local y = math.floor((1 - math.log(f / MIN_FREQUENCY_HZ) / span) * height + 0.5)
            local label = (f >= 1000) and ((f // 1000) .. "k") or tostring(f)
            line(0, y, 6, y, { r = 255, g = 255, b = 255, a = 0.5 })
            text(8, y - FONT_HEIGHT // 2, label, { r = 220, g = 220, b = 235, a = 0.6 })
        end
    end
end
