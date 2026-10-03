-- Gamepad demo: a controller drawn on screen that lights up as you use
-- yours. Not a music visualizer; a reference for controller input.
--
-- Every button is an action registered with a controller default (and a
-- keyboard one, to show they're interchangeable): rebind any of them under
-- Controls in Script Settings. A button glows while held, flashes white
-- the frame it's pressed. The sticks and triggers also show their exact
-- analogue position, read with pad_axis(); the stick's direction buttons
-- (pad_lstick_left and so on) light up past halfway.
--
-- Controllers count while synththing's window has focus; keys only while
-- the visualizer does (click it).

-- name, controller default, keyboard default
local BUTTONS = {
    { "a", "pad_a", "j" },
    { "b", "pad_b", "k" },
    { "x", "pad_x", "u" },
    { "y", "pad_y", "i" },
    { "lb", "pad_lb", "q" },
    { "rb", "pad_rb", "e" },
    { "lt", "pad_lt", "1" },
    { "rt", "pad_rt", "3" },
    { "back", "pad_back", "tab" },
    { "start", "pad_start", "enter" },
    { "guide", "pad_guide", "g" },
    { "dpad up", "pad_dpad_up", "up" },
    { "dpad down", "pad_dpad_down", "down" },
    { "dpad left", "pad_dpad_left", "left" },
    { "dpad right", "pad_dpad_right", "right" },
    { "left stick click", "pad_lstick_click", "z" },
    { "right stick click", "pad_rstick_click", "c" },
    { "left stick up", "pad_lstick_up", "w" },
    { "left stick down", "pad_lstick_down", "s" },
    { "left stick left", "pad_lstick_left", "a" },
    { "left stick right", "pad_lstick_right", "d" },
}
local action = {}
for _, b in ipairs(BUTTONS) do
    action[b[1]] = input_register(b[1], { b[2], b[3] })
end

local BODY = { r = 52, g = 56, b = 70 }
local EDGE = { r = 90, g = 96, b = 116 }
local IDLE = { r = 30, g = 32, b = 40 }
local HELD = { r = 255, g = 196, b = 60 }
local FLASH = { r = 255, g = 255, b = 255 }
local LABEL = { r = 200, g = 204, b = 220 }
local DIM = { r = 120, g = 124, b = 140 }
local FACE = {
    a = { r = 90, g = 200, b = 90 },
    b = { r = 230, g = 80, b = 80 },
    x = { r = 80, g = 140, b = 240 },
    y = { r = 240, g = 200, b = 60 },
}

local recent = {} -- the last few presses, newest first

-- The color for an action: white the frame it's pressed, its glow while
-- held, idle otherwise.
local function state_color(name, glow)
    local s = input(action[name])
    if s == "pressed" then return FLASH end
    if s == "held" then return glow or HELD end
    return IDLE
end

local function label(x, y, str, scale)
    local w = text_size(str, FONT_HEIGHT * scale)
    text(x - w / 2, y - FONT_HEIGHT * scale / 2, str, LABEL, FONT_HEIGHT * scale)
end

local function round_button(name, x, y, r, str, scale, glow)
    circle(x, y, r + 2, EDGE)
    circle(x, y, r, state_color(name, glow))
    if str then label(x, y, str, scale) end
end

local function pill(name, x0, y0, x1, y1, str, scale)
    rect(x0 - 2, y0 - 2, x1 + 2, y1 + 2, EDGE)
    rect(x0, y0, x1, y1, state_color(name))
    label((x0 + x1) / 2, (y0 + y1) / 2, str, scale)
end

-- A stick: its well, a dot at the stick's analogue position, and the
-- click button as the dot's color.
local function stick(cx, cy, r, ax, ay, click_name, readout_y)
    circle(cx, cy, r + 3, EDGE)
    circle(cx, cy, r, IDLE)
    circle(cx, cy, r * 0.5, { r = 40, g = 43, b = 54 })
    local x, y = pad_axis(ax), pad_axis(ay)
    line(cx, cy, cx + x * r, cy + y * r, DIM)
    circle(cx + x * r * 0.75, cy + y * r * 0.75, r * 0.38, state_color(click_name, { r = 150, g = 160, b = 190 }))
    local readout = string.format("%+.2f %+.2f", x, y)
    label(cx, readout_y, readout, 1)
end

-- A trigger: an outline, filled from the bottom as far as it's pulled.
local function trigger(name, axis, x0, y0, w, h, str, scale)
    rect(x0 - 2, y0 - 2, x0 + w + 2, y0 + h + 2, EDGE)
    rect(x0, y0, x0 + w, y0 + h, IDLE)
    local amount = pad_axis(axis)
    rect(x0, y0 + h * (1 - amount), x0 + w, y0 + h, state_color(name) == IDLE and DIM or HELD)
    label(x0 + w / 2, y0 + h / 2, str, scale)
    label(x0 + w / 2, y0 - 12, string.format("%.2f", amount), 1)
end

function render(width, height, left, right)
    clear({ r = 18, g = 19, b = 26 })

    -- Remember presses for the log.
    for _, b in ipairs(BUTTONS) do
        if input(action[b[1]]) == "pressed" then
            table.insert(recent, 1, b[1])
            if #recent > 8 then table.remove(recent) end
        end
    end

    -- Fit a 640 x 400 drawing into the window.
    local s = math.min(width / 700, height / 460)
    local ox, oy = (width - 640 * s) / 2, (height - 400 * s) / 2 + 20 * s
    local function P(x, y) return ox + x * s, oy + y * s end
    local ts = math.max(1, math.floor(s * 1.4 + 0.5))

    -- Triggers and bumpers.
    local x, y = P(110, 0)
    trigger("lt", "lt", x, y, 70 * s, 40 * s, "LT", ts)
    x, y = P(460, 0)
    trigger("rt", "rt", x, y, 70 * s, 40 * s, "RT", ts)
    local x0, y0 = P(100, 52)
    local x1, y1 = P(200, 72)
    pill("lb", x0, y0, x1, y1, "LB", ts)
    x0, y0 = P(440, 52)
    x1, y1 = P(540, 72)
    pill("rb", x0, y0, x1, y1, "RB", ts)

    -- Body: two grips and a middle, all the outlines first so the fills
    -- cover where they overlap.
    local g1x, g1y = P(150, 230)
    local g2x, g2y = P(490, 230)
    x0, y0 = P(150, 110)
    x1, y1 = P(490, 300)
    circle(g1x, g1y, 130 * s, EDGE)
    circle(g2x, g2y, 130 * s, EDGE)
    rect(x0, y0 - 2, x1, y1 + 2, EDGE)
    circle(g1x, g1y, 126 * s, BODY)
    circle(g2x, g2y, 126 * s, BODY)
    rect(x0, y0, x1, y1, BODY)

    -- Face buttons, in their own colors.
    local fx, fy = P(490, 180)
    local d, r = 38 * s, 17 * s
    round_button("y", fx, fy - d, r, "Y", ts, FACE.y)
    round_button("x", fx - d, fy, r, "X", ts, FACE.x)
    round_button("b", fx + d, fy, r, "B", ts, FACE.b)
    round_button("a", fx, fy + d, r, "A", ts, FACE.a)

    -- D-pad.
    local dx, dy = P(230, 260)
    local arm, half = 30 * s, 13 * s
    pill("dpad up", dx - half, dy - arm - half, dx + half, dy - half, "", ts)
    pill("dpad down", dx - half, dy + half, dx + half, dy + arm + half, "", ts)
    pill("dpad left", dx - arm - half, dy - half, dx - half, dy + half, "", ts)
    pill("dpad right", dx + half, dy - half, dx + arm + half, dy + half, "", ts)

    -- Sticks: the left one also lights its directions (actions bound to
    -- pad_lstick_*), drawn as little arrows around it.
    local lx, ly = P(150, 180)
    local reach = 54 * s
    stick(lx, ly, 42 * s, "lstick_x", "lstick_y", "left stick click", ly + reach + 18 * s)
    for _, dir in ipairs({ { "left stick up", 0, -1 }, { "left stick down", 0, 1 }, { "left stick left", -1, 0 }, { "left stick right", 1, 0 } }) do
        circle(lx + dir[2] * reach, ly + dir[3] * reach, 5 * s, state_color(dir[1]))
    end
    local rx, ry = P(410, 260)
    stick(rx, ry, 36 * s, "rstick_x", "rstick_y", "right stick click", ry + 36 * s + 16 * s)

    -- Middle buttons.
    local mx, my = P(320, 150)
    round_button("guide", mx, my, 18 * s, "G", ts)
    round_button("back", mx - 52 * s, my + 30 * s, 11 * s, nil, ts)
    round_button("start", mx + 52 * s, my + 30 * s, 11 * s, nil, ts)
    label(mx - 52 * s, my + 52 * s, "back", 1)
    label(mx + 52 * s, my + 52 * s, "start", 1)

    -- Who's connected, and the recent presses.
    local pads = gamepads()
    local status = #pads == 0 and "No controller connected (the keyboard keys work too: click here first)"
        or ("Connected: " .. table.concat(pads, ", "))
    text(10, 8, status, #pads == 0 and DIM or LABEL, FONT_HEIGHT * ts)
    text(10, height - 10 - FONT_HEIGHT * ts, "Pressed: " .. table.concat(recent, ", "), DIM, FONT_HEIGHT * ts)
end
