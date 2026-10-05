-- Settings API demo. Not a music visualizer, it exercises every setting
-- type the host supports and logs what it's doing, as a reference for
-- writing your own settings-driven script. The Settings button above the
-- visualizer shows the widgets, in the groups given below; the log output
-- is in the Debug window (the Debug button, under Log).
--
-- Not auto-installed like the real visualizers, create it from the "New"
-- button's template list when you want to poke at it.

-- Presets: named sets of setting values, picked at the top of the Settings
-- window (with Defaults, and any the user saved). Each sets the settings it
-- names and leaves the rest; values are written as the settings take them.
settings_preset("Warm", { tint = { r = 255, g = 140, b = 60 }, count = 8, label = "toasty", fruits = { "Cherry", "Date" } })
settings_preset("Minimal", { count = 2, gain = 0.2 })

local frame = 0

function render(width, height, left, right)
    frame = frame + 1

    -- bool / int / float / color / string: each call both declares the
    -- setting (default + range, first call only) and returns its current
    -- live value, call it every frame, it's cheap, and a slider drag in
    -- the Settings window shows up on the very next frame. The optional
    -- last argument puts it in a group (a category in that window; none
    -- means General) and gives it a longer explanation, shown on hover.
    local enabled = setting_bool("enabled", true, { info = "Off skips drawing anything but the background." })
    local count = setting_int("count", 5, 0, 10, { group = "Bars", info = "How many bars to draw." })
    local gain = setting_float("gain", 0.5, 0.0, 1.0, { group = "Bars" })
    local tint = setting_color("tint", { r = 51, g = 204, b = 255 }, { group = "Bars" })
    local label = setting_string("label", "hello", { group = "Text" })

    -- selection: pick up to `max_selections` of a fixed option list. Here,
    -- at most 2 of 4 fruits, picking a 3rd evicts whichever was picked
    -- longest ago, so there's no "disabled checkbox" state to design around.
    local fruits = setting_selection(
        "fruits",
        { "Apple", "Banana", "Cherry", "Date" },
        { "Apple" },
        2,
        { group = "Text", info = "Up to two; picking a third drops the oldest pick." }
    )

    clear({ r = 10, g = 10, b = 14 })

    if not enabled then
        log("enabled=false - skipping the rest of render()")
        return
    end

    -- A little visual feedback so the settings aren't *only* readable in the
    -- log: `count` bars, tinted by `tint`, scaled by `gain`.
    local bar_w = width / math.max(count, 1)
    for i = 1, count do
        local h = height * gain
        rect((i - 1) * bar_w + 1, height - h, i * bar_w - 1, height, tint)
    end

    -- playback() + DT: transport state and frame timing, the same things a
    -- real visualizer uses to e.g. pause a scrolling buffer or animate at a
    -- constant speed regardless of frame rate.
    local p = playback()

    -- log() dedupes identical consecutive messages (shown as "message (xN)")
    -- instead of flooding the pane, safe to call every single frame.
    log(string.format(
        "label=%q count=%d gain=%.2f tint={%d,%d,%d} fruits={%s}",
        label, count, gain, tint.r, tint.g, tint.b, table.concat(fruits, ", ")
    ))
    log(string.format(
        "playback: pos=%.1fs/%.1fs paused=%s loop=%s  dt=%.4fs",
        p.position, p.length, tostring(p.paused), tostring(p.loop_enabled), DT
    ))

    if frame == 1 then
        log("first frame rendered")
    end
end
