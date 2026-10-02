-- FFT spectrum: log-spaced bars per channel. Left is red, right is cyan;
-- wherever a column is covered by both channels equally, drawing both as
-- flat-color vertical lines makes that stretch red + cyan = white, so one
-- glance shows which parts of the spectrum are mono vs. stereo-only. Bars
-- grow from the bottom of the window; a channel at full scale reaches row 0
-- (the very top).
--
-- fft_left/fft_right do the actual FFT in Rust (see spectrum.rs), a script
-- doing its own 1024-point FFT in interpreted Lua at 60fps would eat the
-- entire frame budget by itself. This script only loops per display column
-- (width iterations, not per audio sample or per FFT bin), which is cheap.
--
-- Frequency labels along the top mark where 50 Hz, 100 Hz, 1 kHz and so
-- on fall on the log scale, and two slim meters on the right show each
-- channel's loudness (level_left/level_right, the RMS of this frame's
-- samples) with a slowly falling peak marker.

local MIN_FREQUENCY_HZ = 30.0
local MAX_FREQUENCY_HZ = 16000.0
local MIN_DB = -60.0
local MAX_DB = 0.0
-- Exponential smoothing of bar height between frames: higher = smoother but
-- laggier. This is deliberately mild, not a slow decay.
local SMOOTHING = 0.55

local left_bars = {}
local right_bars = {}
local meter = { l = 0, r = 0, peak_l = 0, peak_r = 0 }
local LABEL_FREQS = { 50, 100, 200, 500, 1000, 2000, 5000, 10000 }
local METER_W = 6

local function freq_at(t)
    return MIN_FREQUENCY_HZ * (MAX_FREQUENCY_HZ / MIN_FREQUENCY_HZ) ^ t
end

local function bin_for_freq(freq, fft_size)
    return math.floor((freq / SAMPLE_RATE) * fft_size + 0.5)
end

-- Peak magnitude (as a 0..1 fraction of the MIN_DB..MAX_DB range) within the
-- log-spaced frequency bucket that column `x` of `width` covers.
local function bucket_unit_value(spectrum, x, width)
    if #spectrum == 0 then
        return 0.0
    end

    local fft_size = #spectrum * 2
    -- Skip bin 1 (DC) and clamp into range so odd window sizes can't error.
    local lo = math.max(1, math.min(bin_for_freq(freq_at((x - 1) / width), fft_size), #spectrum - 1))
    local hi = math.max(lo + 1, math.min(bin_for_freq(freq_at(x / width), fft_size), #spectrum))

    local peak = 0.0
    for i = lo, hi do
        if spectrum[i] and spectrum[i] > peak then
            peak = spectrum[i]
        end
    end

    local db = 20.0 * math.log(math.max(peak, 1e-6), 10)
    return math.max(0.0, math.min(1.0, (db - MIN_DB) / (MAX_DB - MIN_DB)))
end

function render(width, height, left, right)
    local left_spectrum = fft_left(left)
    local right_spectrum = fft_right(right)

    local background = setting_color("background", { r = 16, g = 16, b = 24 })
    local left_color = setting_color("left", { r = 255, g = 0, b = 0 })
    local right_color = setting_color("right", { r = 0, g = 255, b = 255 })
    local overlap_color = setting_color("overlap", { r = 255, g = 255, b = 255 })
    local show_labels = setting_bool("frequency_labels", true)
    local show_meters = setting_bool("level_meters", true)

    clear(background)

    -- The meters take a strip on the right; the spectrum gets the rest.
    local full_width = width
    if show_meters then width = math.max(1, width - 2 * METER_W - 6) end

    local bottom = height - 1
    for x = 1, width do
        local target_left = bucket_unit_value(left_spectrum, x, width)
        local target_right = bucket_unit_value(right_spectrum, x, width)

        left_bars[x] = (left_bars[x] or 0.0) * SMOOTHING + target_left * (1 - SMOOTHING)
        right_bars[x] = (right_bars[x] or 0.0) * SMOOTHING + target_right * (1 - SMOOTHING)

        local left_h = math.floor(left_bars[x] * bottom + 0.5)
        local right_h = math.floor(right_bars[x] * bottom + 0.5)
        local overlap = math.min(left_h, right_h)

        if overlap > 0 then
            line(x, bottom, x, bottom - overlap, overlap_color)
        end
        if left_h > right_h then
            line(x, bottom - right_h, x, bottom - left_h, left_color)
        elseif right_h > left_h then
            line(x, bottom - left_h, x, bottom - right_h, right_color)
        end
    end

    if show_labels then
        local muted = { r = 200, g = 200, b = 220, a = 0.55 }
        local span = math.log(MAX_FREQUENCY_HZ / MIN_FREQUENCY_HZ)
        for _, f in ipairs(LABEL_FREQS) do
            local x = math.floor(math.log(f / MIN_FREQUENCY_HZ) / span * width + 0.5)
            line(x, 0, x, height, { r = 255, g = 255, b = 255, a = 0.06 })
            local label = (f >= 1000) and ((f // 1000) .. "k") or tostring(f)
            text(x + 2, 2, label, muted)
        end
    end

    if show_meters then
        -- Smooth the raw per-frame levels a little; peaks fall back slowly.
        meter.l = meter.l + (level_left() - meter.l) * 0.35
        meter.r = meter.r + (level_right() - meter.r) * 0.35
        meter.peak_l = math.max(meter.l, meter.peak_l - DT * 0.4)
        meter.peak_r = math.max(meter.r, meter.peak_r - DT * 0.4)
        local function draw_meter(x, level, peak, color)
            -- RMS 0..~0.7 mapped to the full height, on the same dB scale.
            local function to_h(v)
                local db = 20 * math.log(math.max(v, 1e-6), 10)
                return math.max(0, math.min(1, (db - MIN_DB) / (MAX_DB - MIN_DB))) * bottom
            end
            rect(x, 0, x + METER_W, height, { r = 255, g = 255, b = 255, a = 0.05 })
            rect(x, bottom - to_h(level), x + METER_W, height, color)
            local py = bottom - to_h(peak)
            rect(x, py - 1, x + METER_W, py + 1, { r = 255, g = 255, b = 255, a = 0.8 })
        end
        draw_meter(full_width - 2 * METER_W - 3, meter.l, meter.peak_l, left_color)
        draw_meter(full_width - METER_W - 1, meter.r, meter.peak_r, right_color)
    end
end
