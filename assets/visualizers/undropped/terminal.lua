-- Terminal: a little command prompt you can type into. Not a music
-- visualizer; a reference for text input. Click it (or use the Fullscreen
-- visualizer) and type; Enter runs the line, Up/Down bring back earlier
-- commands, and the usual text keys work: arrows, Home/End, Backspace and
-- Delete, Ctrl to jump or delete by word, Ctrl+V to paste. Try "help".
--
-- How it works:
--   * typing_begin() starts a typing span: while it's open the app does the
--     editing, and typing_state() hands back the text, the cursor position
--     and the last key; the script draws everything itself with text().
--   * The prompt and the cursor come out of one sprite sheet (three 6x12
--     frames, the size of a font character), drawn with sprite()'s `src`.
--     The current line's prompt switches between hollow and filled once a
--     second; finished lines get the filled one.
--   * The font is monospace, so the cursor's x is just cursor * 6 font
--     pixels: no measuring needed.
--   * Command history is kept between sessions with store_get/store_set.

local CELL_W, CELL_H = 6, 12 -- one character in font pixels

-- Sprite sheet: frame 1 hollow prompt, frame 2 filled prompt, frame 3 the
-- cursor block. 1 = ink, 2 = translucent cursor.
local function frame_rows(rows)
    local image = {}
    for y = 1, CELL_H do
        image[y] = {}
        local row = rows[y] or "......"
        for x = 1, CELL_W do
            local c = row:sub(x, x)
            image[y][x] = (c == "#") and 1 or ((c == "+") and 2 or 0)
        end
    end
    return image
end
local HOLLOW = frame_rows({ nil, nil, nil, "#.....", "##....", "#.#...", "#..#..", "#.#...", "##....", "#....." })
local FILLED = frame_rows({ nil, nil, nil, "#.....", "##....", "###...", "####..", "###...", "##....", "#....." })
local BLOCK = frame_rows({ nil, nil, "++++++", "++++++", "++++++", "++++++", "++++++", "++++++", "++++++", "++++++", "++++++" })

local sheet_image = {}
for y = 1, CELL_H do
    sheet_image[y] = {}
    for _, frame in ipairs({ HOLLOW, FILLED, BLOCK }) do
        for x = 1, CELL_W do
            sheet_image[y][#sheet_image[y] + 1] = frame[y][x]
        end
    end
end

local GREEN = { r = 120, g = 255, b = 150 }
local SHEET = sprite_register({ image = sheet_image, palette = { GREEN, { r = 120, g = 255, b = 150, a = 0.45 } } })
local PROMPT_HOLLOW, PROMPT_FILLED, CURSOR = 0, 1, 2 -- frame numbers

-- Screen contents: {prompt = true/false, text = ...}, oldest first.
local lines = {
    { prompt = false, text = "synththing terminal. type 'help' and press Enter." },
}
local MAX_LINES = 400
local history = store_get("history") or {}
local recall = nil -- index into history while browsing with Up/Down
local draft = ""   -- what was typed before the focus was lost

local function say(text)
    lines[#lines + 1] = { prompt = false, text = text }
end

local commands = {}
commands.help = function()
    say("help          this list")
    say("clear         clear the screen")
    say("echo <text>   print text")
    say("time          where the song is")
    say("tempo         the song's tempo and bar")
    say("about         what this is")
end
commands.clear = function() lines = {} end
commands.echo = function(rest) say(rest) end
commands.time = function()
    local p = playback()
    if p.length > 0 then
        say(string.format("%.1f s of %.1f s%s", p.position, p.length, p.paused and " (paused)" or ""))
    else
        say("no song playing")
    end
end
commands.tempo = function()
    local b = beat()
    if b then
        local bar_number, into = bar()
        local num, den = time_signature()
        say(string.format("%.1f bpm, %d/%d, bar %d beat %d", tempo(), num, den, bar_number, math.floor(into) + 1))
    else
        say("no beats in this song (plain audio, or no song)")
    end
end
commands.about = function()
    say("a typing span demo: the app edits the text, the script draws it.")
end

local function run(line)
    lines[#lines + 1] = { prompt = true, text = line }
    local name, rest = line:match("^%s*(%S+)%s*(.-)%s*$")
    if name then
        local command = commands[name:lower()]
        if command then
            command(rest)
        else
            say("unknown command: " .. name .. " (try help)")
        end
        if history[#history] ~= line then
            history[#history + 1] = line
            while #history > 50 do table.remove(history, 1) end
            store_set("history", history)
        end
    end
    while #lines > MAX_LINES do table.remove(lines, 1) end
    recall = nil
end

function render(width, height, left, right)
    local background = setting_color("background", { r = 8, g = 14, b = 11 })
    local scale = setting_int("text_scale", 2, 1, 4)
    local th = FONT_HEIGHT * scale
    local cw = CELL_W * scale
    local margin = th / 2

    -- Keep a typing span open while we have focus.
    local state = typing_state()
    if state.done then
        run(state.text)
        draft = ""
        typing_begin("")
        state = typing_state()
    elseif state.cancelled then
        draft = state.text -- focus lost: keep what was typed for later
    end
    if has_focus() and not state.active then
        typing_begin(draft)
        state = typing_state()
    end

    -- Up/Down: walk back through earlier commands.
    if state.active and (state.key == "up" or state.key == "down") and #history > 0 then
        if state.key == "up" then
            recall = math.max(1, (recall or (#history + 1)) - 1)
        else
            recall = recall and recall + 1 or nil
            if recall and recall > #history then recall = nil end
        end
        typing_begin(recall and history[recall] or "")
        state = typing_state()
    end

    clear(background)

    -- Lay the screen out bottom-up: the current line at the bottom, older
    -- lines above it.
    local rows = math.max(1, math.floor((height - margin * 2) / th))
    local first = math.max(1, #lines + 2 - rows)
    local y = margin
    local text_x = margin + cw * 2
    for i = first, #lines do
        local line = lines[i]
        if line.prompt then
            sprite(SHEET, margin, y, { scale = scale, src = { PROMPT_FILLED * CELL_W, 0, CELL_W, CELL_H } })
            text(text_x, y, line.text, GREEN, th)
        else
            text(margin, y, line.text, { r = 170, g = 210, b = 180 }, th)
        end
        y = y + th
    end

    -- The current line: blinking prompt from the sheet, the text, the cursor.
    local frame = (math.floor(TIME) % 2 == 0) and PROMPT_HOLLOW or PROMPT_FILLED
    sprite(SHEET, margin, y, { scale = scale, src = { frame * CELL_W, 0, CELL_W, CELL_H } })
    if state.active then
        sprite(SHEET, text_x + state.cursor * cw, y, { scale = scale, src = { CURSOR * CELL_W, 0, CELL_W, CELL_H } })
        text(text_x, y, state.text, GREEN, th)
    else
        local hint = (draft ~= "") and (draft .. "  (click to keep typing)") or "(click here to type)"
        text(text_x, y, hint, { r = 90, g = 140, b = 100 }, th)
    end
end
