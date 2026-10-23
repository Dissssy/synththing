-- Now playing: a pixel-art card with the song's title and artist, how far
-- in it is, and its album art, for a stream overlay. Its frames are
-- see-through (script_options transparent), so in OBS, through the local
-- API's PNG frames (/v1/visualizer?format=png&width=500&height=190...),
-- only the card shows over the rest of the scene.
--
-- With "Album colors" on, the border, bar and shadow take on the color the
-- album art is mostly made of (album_art().color), easing over when the
-- song changes. A title too long for the card scrolls, fading out at the
-- edges: the text is drawn clipped to thin slices, each fainter toward the
-- edge. The background can be see-through too (its color's alpha), so
-- every shape is one polygon that covers each pixel once.
--
-- Everything is laid out on a grid of "card pixels" (CARD_W by CARD_H),
-- scaled by whole steps to fill the frame, so the pixel art stays crisp.

script_options({ transparent = true })

local CARD_W, CARD_H = 240, 72
local BORDER = 3
local ART = 56
-- How fast the colors ease to a new song's (approach, per second).
local COLOR_RATE = 3
-- How fast a long title scrolls, in card pixels a second, and the gap
-- before it comes round again.
local SCROLL_SPEED = 24
local SCROLL_GAP = 36
-- How wide the fade at each edge of the text is, and in how many slices.
local FADE_W, FADE_SLICES = 12, 6
-- How far the notes stick up over the card, bobbing included.
local NOTES_ROOM = 14

local NOTES = sprite_register({
    palette = { { r = 255, g = 255, b = 255 } },
    image = {
        { 0, 0, 0, 1, 1, 1, 1, 1 },
        { 0, 0, 0, 1, 1, 1, 1, 1 },
        { 0, 0, 0, 1, 0, 0, 0, 1 },
        { 0, 0, 0, 1, 0, 0, 0, 1 },
        { 0, 0, 0, 1, 0, 0, 0, 1 },
        { 0, 1, 1, 1, 0, 1, 1, 1 },
        { 1, 1, 1, 1, 1, 1, 1, 1 },
        { 0, 1, 1, 0, 0, 1, 1, 0 },
    },
})

local shown -- the accent color as drawn, easing toward the wanted one

local function clock(seconds)
    if not seconds or seconds ~= seconds or seconds < 0 then return "--:--" end
    seconds = math.floor(seconds)
    return string.format("%02d:%02d", seconds // 60, seconds % 60)
end

-- The card's shape: a rectangle with its corners cut in two pixel steps,
-- as a flat list of points for polygon().
local function stairs(x0, y0, x1, y1, u)
    return {
        x0 + 2 * u, y0, x1 - 2 * u, y0, x1 - 2 * u, y0 + u, x1 - u, y0 + u,
        x1 - u, y0 + 2 * u, x1, y0 + 2 * u, x1, y1 - 2 * u, x1 - u, y1 - 2 * u,
        x1 - u, y1 - u, x1 - 2 * u, y1 - u, x1 - 2 * u, y1, x0 + 2 * u, y1,
        x0 + 2 * u, y1 - u, x0 + u, y1 - u, x0 + u, y1 - 2 * u, x0, y1 - 2 * u,
        x0, y0 + 2 * u, x0 + u, y0 + 2 * u, x0 + u, y0 + u, x0 + 2 * u, y0 + u,
    }
end

local function chunky(x0, y0, x1, y1, u, color)
    polygon(stairs(x0, y0, x1, y1, u), color)
end

-- What's in one of the shapes but not both: the two outlines joined into
-- one polygon, which the even-odd rule leaves a hole in where they overlap.
local function either(a, b, color)
    local points = {}
    for _, list in ipairs({ a, { a[1], a[2] }, b, { b[1], b[2] } }) do
        for _, v in ipairs(list) do points[#points + 1] = v end
    end
    polygon(points, color)
end

-- `str` in the box from x0 to x1, scrolling when it's too long, with its
-- ends faded out.
local function marquee(str, x0, y, x1, h, u, color)
    local w = text_size(str, h)
    push_view()
    clip(x0, y, x1, y + h)
    if w <= x1 - x0 then
        text(x0, y, str, color, h)
        pop_view()
        return
    end
    local loop = w + SCROLL_GAP * u
    local offset = (TIME * SCROLL_SPEED * u) % loop
    local function draw(alpha)
        local c = { r = color.r, g = color.g, b = color.b, a = (color.a or 1) * alpha }
        text(x0 - offset, y, str, c, h)
        text(x0 - offset + loop, y, str, c, h)
    end
    -- The middle as it is, then slices at each end, fainter toward the edge.
    local fade = FADE_W * u
    local slice = fade / FADE_SLICES
    clip(x0 + fade, y, x1 - fade, y + h)
    draw(1)
    for i = 0, FADE_SLICES - 1 do
        local alpha = (i + 0.5) / FADE_SLICES
        clip(x0 + i * slice, y, x0 + (i + 1) * slice, y + h)
        draw(alpha)
        clip(x1 - (i + 1) * slice, y, x1 - i * slice, y + h)
        draw(alpha)
    end
    pop_view()
end

function render(width, height)
    local background = setting_color("background", { r = 58, g = 40, b = 40 },
        { group = "Colors", alpha = true, info = "Its alpha makes the card see-through, for OBS over the rest of your scene" })
    local text_color = setting_color("text", { r = 245, g = 238, b = 228 }, { group = "Colors" })
    local accent_setting = setting_color("accent", { r = 235, g = 40, b = 40 },
        { group = "Colors", info = "The border, bar and shadow, unless Album colors is on and the song has art" })
    local album_colors = setting_bool("album_colors", true,
        { group = "Colors", info = "Take the accent color from the album art" })
    local show_art = setting_bool("album_art", true, { info = "Show the album art on the right" })

    -- Whole steps of card pixels, as big as fits (smaller than 1 only when
    -- the frame is smaller than the card).
    -- Room above the card for the notes, and below and right for the shadow.
    local fit = math.min(width / (CARD_W + 6), height / (CARD_H + NOTES_ROOM + 6))
    local u = fit >= 1 and math.floor(fit) or fit
    local cw = (show_art and CARD_W or CARD_W - ART - 6) * u
    local ch = CARD_H * u
    local x0 = math.floor((width - cw - 4 * u) / 2)
    local y0 = math.floor((height - ch - (NOTES_ROOM + 4) * u) / 2) + NOTES_ROOM * u
    local x1, y1 = x0 + cw, y0 + ch

    local p = playback()
    local art = show_art and album_art(math.floor(ART * u + 0.5)) or nil
    local wanted = (album_colors and art) and art.color or accent_setting
    if not shown then shown = { r = wanted.r, g = wanted.g, b = wanted.b } end
    for _, k in ipairs({ "r", "g", "b" }) do
        shown[k] = approach(shown[k], wanted[k], COLOR_RATE)
    end
    local accent = shown
    local shadow = mix(accent, { r = 0, g = 0, b = 30 }, 0.55)
    -- Mixed with the background's color, not its see-through.
    local solid = { r = background.r, g = background.g, b = background.b }
    local muted = mix(text_color, solid, 0.4)
    local track = mix(text_color, solid, 0.6)

    -- Shadow (only where the card isn't), border, inside. The shadow is as
    -- far off as the border is wide, so the part of the card it leaves
    -- uncovered is all under the border, drawn over it.
    local card = stairs(x0, y0, x1, y1, u)
    local inside = stairs(x0 + BORDER * u, y0 + BORDER * u, x1 - BORDER * u, y1 - BORDER * u, u)
    either(stairs(x0 + BORDER * u, y0 + BORDER * u, x1 + BORDER * u, y1 + BORDER * u, u), card, shadow)
    either(card, inside, accent)
    polygon(inside, background)

    -- Two notes over the top-right corner, bobbing with the sound.
    local bob = math.sin(TIME * 3) * u + math.min(level_left(), 1) * 3 * u
    sprite(NOTES, x1 - 18 * u, y0 - 9 * u - bob, u)

    -- The art, framed, on the right.
    local text_right = x1 - 8 * u
    if show_art then
        local ax1 = x1 - 8 * u
        local ax0 = ax1 - ART * u
        local ay0 = y0 + (CARD_H - ART) / 2 * u
        chunky(ax0 - 2 * u, ay0 - 2 * u, ax1 + 2 * u, ay0 + (ART + 2) * u, u, accent)
        rect(ax0, ay0, ax1, ay0 + ART * u, shadow)
        if art then
            sprite(art.sprite, ax0 + (ART * u - art.width) / 2, ay0 + (ART * u - art.height) / 2)
        else
            local note_w = 8 * 4 * u
            sprite(NOTES, ax0 + (ART * u - note_w) / 2, ay0 + (ART * u - note_w) / 2, 4 * u)
        end
        text_right = ax0 - 8 * u
    end

    -- Title, artist, times and the bar on the left.
    local left = x0 + 9 * u
    local line = FONT_HEIGHT * u
    local loaded = p.song_name ~= nil
    local title = loaded and (p.title or p.song_name) or "Nothing playing"
    local artist = loaded and (p.artist or p.album or "") or ""
    marquee(title, left, y0 + 7 * u, text_right, line, u, text_color)
    marquee(artist, left, y0 + 21 * u, text_right, line, u, muted)

    local small = math.max(FONT_HEIGHT, math.floor(line * 0.75))
    local times_y = y0 + 37 * u
    text(left, times_y, clock(loaded and p.position), text_color, small)
    local total = clock(loaded and p.length > 0 and p.length or nil)
    text(text_right - text_size(total, small), times_y, total, text_color, small)

    local bar_y0, bar_y1 = y0 + 52 * u, y0 + 60 * u
    either(stairs(left, bar_y0, text_right, bar_y1, u), stairs(left + u, bar_y0 + u, text_right - u, bar_y1 - u, u), track)
    local fraction = (loaded and p.length > 0) and math.max(0, math.min(1, p.position / p.length)) or 0
    local fill_x = left + u + (text_right - left - 2 * u) * fraction
    if fill_x > left + 3 * u then
        chunky(left + u, bar_y0 + u, fill_x, bar_y1 - u, u, accent)
    end
    local mid = (bar_y0 + bar_y1) / 2
    circle(fill_x, mid, 5 * u, shadow)
    circle(fill_x, mid, 3.5 * u, accent)
end
