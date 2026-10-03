-- Editor test: not a real visualizer, a page of things for the script
-- editor to react to. Every feature it should show is marked TRY below.
-- It runs (it draws a little), but it's deliberately untidy: the editor
-- should find exactly three problems in it (see "Problems").

-- TRY Problems: three warnings, listed in the "3 problems" menu above:
--   line 14: `spare` is never used
--   line 40: `playbak` isn't defined anywhere (did you mean `playback`?)
--   line 41: sets a global `stray` (did you mean `local stray`?)
-- Uncomment the next line for a syntax error (red) instead; the other
-- warnings hide until it parses again.
-- local broken = (1 +

local spare = 42

-- TRY Color swatches: a swatch shows before each color table; click it
-- for a picker that rewrites the numbers (spacing and order kept).
local background = { r = 14, g = 16, b = 28 }
local accent = {r=255,g=150,b=40}
local glass = { r = 120, g = 200, b = 255, a = 0.35 }
local not_a_color = { r = 1, x = 2 }     -- no swatch: not just r/g/b/a
-- { r = 255, g = 0, b = 0 }             -- no swatch: in a comment

-- TRY Go to: these show in the "Go to" list; F12 or Ctrl+click on a call
-- below jumps here.
local function ring(cx, cy, radius, color)
    circle(cx, cy, radius, color)
end

local function label(x, y, text_value)
    text(x, y, text_value, accent, FONT_HEIGHT * 2)
end

function helper_global(a, b)
    return a + b
end

-- Never called, so the problems in it don't break the run.
local function broken_on_purpose()
    local p = playbak()
    stray = p
end

-- TRY Format (Shift+Alt+F): this function is badly laid out on purpose.
local function messy(  x,y )
if x>y then return x
    else
        return y end
end

function render(width, height, left, right)
    clear(background)

    -- TRY Completions: after `p.` the list offers position, song_id, ...
    -- (it knows what playback() returns). Type `p.` on a new line below.
    local p = playback()

    -- TRY Signature help: put the cursor inside these calls' parentheses.
    ring(width / 2, height / 2, 40 + 10 * math.sin(TIME), glass)
    label(10, 10, string.format("%.1f s", p.position))
    rect(0, height - 6, width * (p.length > 0 and p.position / p.length or 0), height, accent)

    -- TRY Completions on notes: `n.` offers channel, key, velocity, source.
    for _, n in ipairs(active_notes()) do
        pixel(n.key * 4 % width, height / 3, accent)
    end

    -- TRY Hover: hover `ring`, `p` or `width` to see what they are; hover
    -- `rect` and click "Reference" to jump to it in the Scripting Reference.
    -- TRY Matching brackets: put the cursor next to a bracket.
    -- TRY Find (Ctrl+F): try finding "accent".
    log(helper_global(1, messy(2, 3)), #left + #right, not_a_color.x)
    if FRAME < 0 then
        broken_on_purpose() -- never true
    end
end
