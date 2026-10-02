//! Built-in pixel text for scripts: the Monogram font (by Vinícius Menézio,
//! CC0, https://datagoblin.itch.io/monogram), drawn from its bitmap data
//! with pure nearest-neighbor scaling, so pixels stay pixels at any size.
//!
//! Every character is a 12-row cell (room for accents above capitals and
//! descenders below), 5 pixels wide plus 1 of spacing. A script asks for a
//! line height in pixels; 12 is the font's own size, and whole multiples
//! (24, 36, ...) keep every font pixel exactly square.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The font's own line height: one character cell, in font pixels.
pub const CELL_HEIGHT: u32 = 12;
/// Horizontal distance from one character to the next, in font pixels.
const ADVANCE: u32 = 6;
/// The blank column at the end of each advance, trimmed from the width of
/// a line so measured text ends at its last drawn pixel.
const SPACING: u32 = 1;

const FONT_JSON: &str = include_str!("../assets/fonts/monogram/monogram-bitmap.json");

/// A glyph: one bit mask per row, bit 0 the leftmost pixel.
type Glyph = [u8; CELL_HEIGHT as usize];

fn glyphs() -> &'static HashMap<char, Glyph> {
    static GLYPHS: OnceLock<HashMap<char, Glyph>> = OnceLock::new();
    GLYPHS.get_or_init(|| {
        let raw: HashMap<String, Vec<u8>> =
            serde_json::from_str(FONT_JSON).expect("embedded Monogram bitmap data is valid JSON");
        raw.into_iter()
            .filter_map(|(key, rows)| {
                let mut chars = key.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else { return None };
                let glyph: Glyph = rows.try_into().ok()?;
                Some((c, glyph))
            })
            .collect()
    })
}

/// The glyph for `c`: tabs and other blank control characters as a space,
/// anything the font doesn't have as `?`.
fn glyph(c: char) -> &'static Glyph {
    let font = glyphs();
    let c = if c.is_whitespace() { ' ' } else { c };
    font.get(&c).or_else(|| font.get(&'?')).expect("Monogram has '?'")
}

/// Width and height in pixels of `text` drawn with lines `height` pixels
/// tall. Lines split at `\n`; the width is the widest line's.
pub fn measure(text: &str, height: f32) -> (f32, f32) {
    let scale = height / CELL_HEIGHT as f32;
    let mut lines = 0usize;
    let mut widest = 0usize;
    for line in text.split('\n') {
        lines += 1;
        widest = widest.max(line.chars().count());
    }
    let columns = if widest == 0 { 0 } else { widest as u32 * ADVANCE - SPACING };
    (columns as f32 * scale, lines as f32 * height)
}

/// Lay out `text` with its top-left at `(x, y)` and lines `height` pixels
/// tall, calling `fill(x0, y0, x1, y1)` for each horizontal run of lit
/// pixels. Run edges are computed from font-pixel boundaries, so scaled
/// pixels tile exactly with no gaps or overlaps (nearest-neighbor).
pub fn layout(text: &str, x: f32, y: f32, height: f32, mut fill: impl FnMut(f32, f32, f32, f32)) {
    let scale = height / CELL_HEIGHT as f32;
    let edge = |origin: f32, font_px: u32| (origin + font_px as f32 * scale).round();
    for (line_index, line) in text.split('\n').enumerate() {
        let line_y = y + line_index as f32 * height;
        for (column, c) in line.chars().enumerate() {
            let cell_x = column as u32 * ADVANCE;
            for (row, &mask) in glyph(c).iter().enumerate() {
                let mut bits = mask as u32;
                let mut bit = 0u32;
                while bits != 0 {
                    if bits & 1 == 0 {
                        bits >>= 1;
                        bit += 1;
                        continue;
                    }
                    // A run of lit pixels, drawn as one rect.
                    let start = bit;
                    while bits & 1 == 1 {
                        bits >>= 1;
                        bit += 1;
                    }
                    fill(
                        edge(x, cell_x + start),
                        edge(line_y, row as u32),
                        edge(x, cell_x + bit),
                        edge(line_y, row as u32 + 1),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_loads_with_ascii_and_beyond() {
        for c in (' '..='~').chain("éЖ←".chars()) {
            assert!(glyphs().contains_key(&c), "missing {c:?}");
        }
        assert!(glyph('\u{1F600}') == glyph('?'));
    }

    #[test]
    fn measures_lines_in_pixels() {
        assert_eq!(measure("", 12.0), (0.0, 12.0));
        assert_eq!(measure("A", 12.0), (5.0, 12.0));
        assert_eq!(measure("AB", 12.0), (11.0, 12.0));
        assert_eq!(measure("AB\nC", 24.0), (22.0, 48.0));
    }

    #[test]
    fn layout_draws_whole_scaled_pixels_inside_the_measured_box() {
        let mut runs = Vec::new();
        layout("I", 10.0, 20.0, 24.0, |x0, y0, x1, y1| runs.push((x0, y0, x1, y1)));
        assert!(!runs.is_empty());
        let (w, h) = measure("I", 24.0);
        for &(x0, y0, x1, y1) in &runs {
            // Doubled size: every edge on an even pixel offset from the origin.
            for v in [x0 - 10.0, x1 - 10.0, y0 - 20.0, y1 - 20.0] {
                assert_eq!(v % 2.0, 0.0, "{runs:?}");
            }
            assert!(x0 >= 10.0 && x1 <= 10.0 + w && y0 >= 20.0 && y1 <= 20.0 + h);
        }
    }
}
