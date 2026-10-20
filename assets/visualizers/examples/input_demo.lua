-- Input demo. Not a music visualizer: a little paint toy showing off the
-- mouse, keyboard and cursor functions. Not auto-installed, create it from
-- the "New" button's template list.
--
--   left mouse     paint            right mouse   erase
--   scroll         brush size       1-5           pick a color
--   c              clear            WASD/arrows   move the square
--   fingers        paint, one stroke per finger (touchscreens)
--   tap a swatch   pick a color (touch)
--   h              toggle the system cursor (hidden by default, we draw
--                  our own crosshair instead)
--   l              toggle cursor locking (only does anything in the
--                  dedicated fullscreen: "Fullscreen visualizer")
--
-- The keys are defaults: rebind them with the Controls button.
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

-- Keyboard controls are actions: registered once with default keys, and
-- rebindable by the user with the Controls button.
local HIDE = input_register("toggle cursor", "h")
local LOCK = input_register("toggle cursor lock", "l")
local CLEAR = input_register("clear", "c")
local LEFT = input_register("left", { "a", "left" })
local RIGHT = input_register("right", { "d", "right" })
local UP = input_register("up", { "w", "up" })
local DOWN = input_register("down", { "s", "down" })
local COLORS = {}
for i = 1, #palette do
    COLORS[i] = input_register("color " .. i, tostring(i))
end

function render(width, height, left, right)
    local cell = setting_int("cell_size", 4, 1, 16)
    local speed = setting_float("square_speed", 140, 20, 400)
    local background = setting_color("background", { r = 14, g = 14, b = 20 })

    -- Cursor: hide the system one over the visualizer (we draw our own),
    -- and lock it to the screen if asked. Locking is ignored outside the
    -- dedicated fullscreen, so it's safe to just ask every frame.
    if input(HIDE) == "pressed" then hide_cursor = not hide_cursor end
    if input(LOCK) == "pressed" then
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
        if input(COLORS[i]) == "pressed" then color = i end
    end
    if input(CLEAR) == "pressed" then cells = {} end

    -- Scroll arrives smoothed, a little per frame; turn it into steps.
    local _, sy = scroll()
    scroll_acc = scroll_acc + sy
    while scroll_acc >= 30 do brush = math.min(brush + 1, 12); scroll_acc = scroll_acc - 30 end
    while scroll_acc <= -30 do brush = math.max(brush - 1, 0); scroll_acc = scroll_acc + 30 end

    -- Paint along the whole path a pointer took this frame, so a fast
    -- stroke stays a line instead of a row of dots.
    local function stroke(x1, y1, dx, dy, value)
        local steps = math.max(1, math.ceil(math.max(math.abs(dx), math.abs(dy)) / cell))
        for s = 0, steps do
            local t = s / steps
            stamp(math.floor((x1 - dx * (1 - t)) / cell), math.floor((y1 - dy * (1 - t)) / cell), value)
        end
    end

    -- Touch: every finger paints its own stroke, and tapping a swatch
    -- picks it. The first finger also works the mouse, for scripts that
    -- only know the mouse, so skip the mouse while fingers are down.
    local fingers = touches()
    for _, t in ipairs(fingers) do
        local swatch = t.y < 22 and math.floor((t.x - 4) / 18) + 1
        if t.pressed and swatch and palette[swatch] then
            color = swatch
        elseif not swatch then
            stroke(t.x, t.y, t.dx, t.dy, color)
        end
    end

    local mx, my = mouse()
    if #fingers > 0 then mx, my = nil, nil end
    if mx then
        local erase = mouse_down("right")
        if mouse_down("left") or erase then
            local dx, dy = mouse_delta()
            stroke(mx, my, dx, dy, (not erase) and color or nil)
        end
    end

    -- The square, frame-rate independent via DT.
    local vx, vy = 0, 0
    if input_down(LEFT) then vx = vx - 1 end
    if input_down(RIGHT) then vx = vx + 1 end
    if input_down(UP) then vy = vy - 1 end
    if input_down(DOWN) then vy = vy + 1 end
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

    -- Which display mode we're in: a box each, top right, one lit.
    local modes = { "window", "fullscreen", "dedicated", "mini", "pip" }
    for i, m in ipairs(modes) do
        local x = width - 6 - (#modes + 1 - i) * 14
        local lit = (m == mode) and 1.0 or 0.25
        rect(x, 6, x + 10, 16, { r = 200, g = 200, b = 255, a = lit })
    end

    -- Focus border: green when keys are coming to us.
    local edge = has_focus() and { r = 90, g = 220, b = 120 } or { r = 60, g = 60, b = 70 }
    rect(0, 0, width, 2, edge)
    rect(0, height - 2, width, height, edge)
    rect(0, 0, 2, height, edge)
    rect(width - 2, 0, width, height, edge)

    -- A ring under each finger, brighter the harder it presses (where the
    -- screen reports pressure).
    for _, t in ipairs(fingers) do
        local c = palette[color]
        circle_outline(t.x, t.y, math.max(10, (brush + 1) * cell + 6), { r = c.r, g = c.g, b = c.b, a = 0.4 + 0.6 * (t.force or 1) })
    end

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
