-- Input demo. Not a music visualizer: a little paint toy showing off the
-- mouse, keyboard and cursor functions. Not auto-installed, create it from
-- the "New" button's template list.
--
--   left mouse     paint            right mouse   erase
--   scroll         brush size       1-5           pick a color
--   c              clear            WASD/arrows   move the square
--   h              toggle the system cursor (hidden by default, we draw
--                  our own crosshair instead)
--   l              toggle cursor locking (only does anything in the
--                  dedicated fullscreen: "Fullscreen visualizer")
--
-- Keys only arrive while the visualizer has focus: click into it first
-- (the border turns green). Escape always gets you out, scripts never see it.

local cells = {}        -- painted cells: [cy * 65536 + cx] = palette index
local color = 1
local brush = 2         -- radius, in cells
local scroll_acc = 0
local px, py = 40, 40   -- the keyboard-driven square
local hide_cursor = true
local lock_cursor = false
local last_mode = nil

local palette = {
    { r = 255, g = 90, b = 90 },
    { r = 255, g = 200, b = 60 },
    { r = 90, g = 220, b = 120 },
    { r = 80, g = 170, b = 255 },
    { r = 230, g = 230, b = 240 },
}

-- Paint (or erase, value = nil) a round brush centered on cell cx, cy.
local function stamp(cx, cy, value)
    for dy = -brush, brush do
        for dx = -brush, brush do
            local x, y = cx + dx, cy + dy
            if x >= 0 and y >= 0 and dx * dx + dy * dy <= brush * brush then
                cells[y * 65536 + x] = value
            end
        end
    end
end

function render(width, height, left, right)
    local cell = setting_int("cell_size", 4, 1, 16)
    local speed = setting_float("square_speed", 140, 20, 400)
    local background = setting_color("background", { r = 14, g = 14, b = 20 })

    -- Cursor: hide the system one over the visualizer (we draw our own),
    -- and lock it to the screen if asked. Locking is ignored outside the
    -- dedicated fullscreen, so it's safe to just ask every frame.
    if key_pressed("h") then hide_cursor = not hide_cursor end
    if key_pressed("l") then
        lock_cursor = not lock_cursor
        if lock_cursor and display_mode() ~= "dedicated" then
            log("cursor lock only applies in the dedicated fullscreen")
        end
    end
    set_cursor_visible(not hide_cursor)
    set_cursor_locked(lock_cursor)

    local mode = display_mode()
    if mode ~= last_mode then
        log("display mode: " .. mode)
        last_mode = mode
    end

    for i = 1, #palette do
        if key_pressed(tostring(i)) then color = i end
    end
    if key_pressed("c") then cells = {} end

    -- Scroll arrives smoothed, a little per frame; turn it into steps.
    local _, sy = scroll()
    scroll_acc = scroll_acc + sy
    while scroll_acc >= 30 do brush = math.min(brush + 1, 12); scroll_acc = scroll_acc - 30 end
    while scroll_acc <= -30 do brush = math.max(brush - 1, 0); scroll_acc = scroll_acc + 30 end

    -- Paint along the whole path the pointer took this frame, so a fast
    -- stroke stays a line instead of a row of dots.
    local mx, my = mouse()
    if mx then
        local erase = mouse_down("right")
        if mouse_down("left") or erase then
            local value = (not erase) and color or nil
            local dx, dy = mouse_delta()
            local steps = math.max(1, math.ceil(math.max(math.abs(dx), math.abs(dy)) / cell))
            for s = 0, steps do
                local t = s / steps
                local x = mx - dx * (1 - t)
                local y = my - dy * (1 - t)
                stamp(math.floor(x / cell), math.floor(y / cell), value)
            end
        end
    end

    -- The square, frame-rate independent via DT.
    local vx, vy = 0, 0
    if key_down("a") or key_down("left") then vx = vx - 1 end
    if key_down("d") or key_down("right") then vx = vx + 1 end
    if key_down("w") or key_down("up") then vy = vy - 1 end
    if key_down("s") or key_down("down") then vy = vy + 1 end
    px = math.max(0, math.min(width - 10, px + vx * speed * DT))
    py = math.max(0, math.min(height - 10, py + vy * speed * DT))

    -- Draw.
    clear(background)
    for key, c in pairs(cells) do
        local cx, cy = key % 65536, key // 65536
        rect(cx * cell, cy * cell, cx * cell + cell, cy * cell + cell, palette[c])
    end
    rect(px, py, px + 10, py + 10, { r = 255, g = 255, b = 255 })

    -- Palette swatches, the picked one outlined.
    for i, c in ipairs(palette) do
        local x = 6 + (i - 1) * 18
        if i == color then rect(x - 2, 4, x + 14, 20, { r = 255, g = 255, b = 255 }) end
        rect(x, 6, x + 12, 18, c)
    end

    -- Which display mode we're in: three boxes, top right, one lit.
    for i, m in ipairs({ "window", "fullscreen", "dedicated" }) do
        local x = width - 6 - (4 - i) * 14
        local lit = (m == mode) and 1.0 or 0.25
        rect(x, 6, x + 10, 16, { r = 200, g = 200, b = 255, a = lit })
    end

    -- Focus border: green when keys are coming to us.
    local edge = has_focus() and { r = 90, g = 220, b = 120 } or { r = 60, g = 60, b = 70 }
    rect(0, 0, width, 2, edge)
    rect(0, height - 2, width, height, edge)
    rect(0, 0, 2, height, edge)
    rect(width - 2, 0, width, height, edge)

    -- Our own cursor: a crosshair in the brush color, sized like the brush.
    if mx then
        local r = math.max(4, (brush + 1) * cell)
        local c = palette[color]
        line(mx - r, my, mx - 2, my, c)
        line(mx + 2, my, mx + r, my, c)
        line(mx, my - r, mx, my - 2, c)
        line(mx, my + 2, mx, my + r, c)
    end
end
