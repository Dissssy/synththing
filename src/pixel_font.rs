//! Built-in pixel text for scripts: the Monogram font (by Vinícius Menézio,
//! CC0, https://datagoblin.itch.io/monogram), drawn from its bitmap data
//! with pure nearest-neighbor scaling, so pixels stay pixels at any size.
//!
//! Every character is a 12-row cell (room for accents above capitals and
//! descenders below), 5 pixels wide plus 1 of spacing. A script asks for a
//! line height in pixels; 12 is the font's own size, and whole multiples
//! (24, 36, ...) keep every font pixel exactly square.
//!
//! Characters Monogram lacks (Japanese, Korean, Chinese) come from the
//! bundled Noto fonts instead, drawn smooth at the text's real size, on
//! Monogram's baseline and in whole font pixels of width, so they line up
//! with the pixel text around them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph_rasterizer::{point, Rasterizer};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

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

/// Size of a fallback font's em, in font pixels: tall enough that its
/// characters fill the cell like Monogram's capitals and descenders do.
const FALLBACK_EM: f32 = 10.0;
/// Monogram's baseline, in font pixels from the top of the cell.
const BASELINE: u32 = 10;

fn fallback_fonts() -> &'static [FontRef<'static>] {
    static FONTS: OnceLock<Vec<FontRef<'static>>> = OnceLock::new();
    FONTS.get_or_init(|| {
        crate::bundled_fonts::CJK
            .iter()
            .filter_map(|(name, bytes)| {
                FontRef::new(bytes).inspect_err(|e| log::warn!("couldn't read the bundled {name}: {e}")).ok()
            })
            .collect()
    })
}

/// How one character is drawn.
enum Shape {
    Bitmap(&'static Glyph),
    /// From a fallback font, `advance` font pixels wide (spacing included).
    Outline { font: usize, glyph: GlyphId, advance: u32 },
}

impl Shape {
    /// `c`'s shape: tabs and other blank control characters as a space,
    /// what neither Monogram nor a fallback has as `?`.
    fn of(c: char) -> Self {
        let font = glyphs();
        let c = if c.is_whitespace() { ' ' } else { c };
        if let Some(glyph) = font.get(&c) {
            return Shape::Bitmap(glyph);
        }
        for (index, fallback) in fallback_fonts().iter().enumerate() {
            let Some(glyph) = fallback.charmap().map(c).filter(|g| g.to_u32() != 0) else { continue };
            let metrics = fallback.glyph_metrics(Size::new(FALLBACK_EM), LocationRef::default());
            let width = metrics.advance_width(glyph).unwrap_or(FALLBACK_EM);
            return Shape::Outline { font: index, glyph, advance: width.round().max(1.0) as u32 + SPACING };
        }
        Shape::Bitmap(font.get(&'?').expect("Monogram has '?'"))
    }

    fn advance(&self) -> u32 {
        match self {
            Shape::Bitmap(_) => ADVANCE,
            Shape::Outline { advance, .. } => *advance,
        }
    }
}

/// Width and height in pixels of `text` drawn with lines `height` pixels
/// tall. Lines split at `\n`; the width is the widest line's.
pub fn measure(text: &str, height: f32) -> (f32, f32) {
    let scale = height / CELL_HEIGHT as f32;
    let mut lines = 0usize;
    let mut widest = 0u32;
    for line in text.split('\n') {
        lines += 1;
        let advances: u32 = line.chars().map(|c| Shape::of(c).advance()).sum();
        widest = widest.max(advances.saturating_sub(SPACING));
    }
    (widest as f32 * scale, lines as f32 * height)
}

/// Lay out `text` with its top-left at `(x, y)` and lines `height` pixels
/// tall, calling `fill(x0, y0, x1, y1, coverage)` for each horizontal run
/// of lit pixels. Monogram's runs are whole (coverage 1), their edges on
/// font-pixel boundaries, so scaled pixels tile exactly with no gaps or
/// overlaps (nearest-neighbor); a fallback character comes as single
/// pixels, each with how much of it the character covers.
pub fn layout(text: &str, x: f32, y: f32, height: f32, mut fill: impl FnMut(f32, f32, f32, f32, f32)) {
    let scale = height / CELL_HEIGHT as f32;
    let edge = |origin: f32, font_px: u32| (origin + font_px as f32 * scale).round();
    for (line_index, line) in text.split('\n').enumerate() {
        let line_y = y + line_index as f32 * height;
        let mut cell_x = 0u32;
        for c in line.chars() {
            let shape = Shape::of(c);
            match &shape {
                Shape::Bitmap(glyph) => {
                    for (row, &mask) in glyph.iter().enumerate() {
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
                                1.0,
                            );
                        }
                    }
                }
                Shape::Outline { font, glyph, advance } => {
                    let left = edge(x, cell_x);
                    let cell = Cell {
                        left,
                        top: line_y.round(),
                        baseline: edge(line_y, BASELINE),
                        width: (edge(x, cell_x + advance) - left).max(1.0),
                        height: height.round().max(1.0),
                    };
                    for &(px, py, coverage) in rasterized(*font, *glyph, FALLBACK_EM * scale, &cell).iter() {
                        let (x0, y0) = (cell.left + px as f32, cell.top + py as f32);
                        fill(x0, y0, x0 + 1.0, y0 + 1.0, coverage);
                    }
                }
            }
            cell_x += shape.advance();
        }
    }
}

/// Where a fallback character goes, in output pixels.
struct Cell {
    left: f32,
    top: f32,
    baseline: f32,
    width: f32,
    height: f32,
}

/// A pen that rasterizes an outline into a cell, font units (y up, from
/// the origin on the baseline) turned into the cell's pixels (y down).
struct Pen {
    raster: Rasterizer,
    /// The baseline, in pixels from the top of the cell.
    baseline: f32,
    width: f32,
    height: f32,
    start: (f32, f32),
    last: (f32, f32),
}

impl Pen {
    /// Clamped into the cell, so nothing lands in a neighboring row.
    fn at(&self, x: f32, y: f32) -> ab_glyph_rasterizer::Point {
        point(x.clamp(0.0, self.width), (self.baseline - y).clamp(0.0, self.height))
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = (x, y);
        self.last = (x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (p0, p1) = (self.at(self.last.0, self.last.1), self.at(x, y));
        self.raster.draw_line(p0, p1);
        self.last = (x, y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.at(self.last.0, self.last.1), self.at(cx0, cy0), self.at(x, y));
        self.raster.draw_quad(p0, p1, p2);
        self.last = (x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (self.at(self.last.0, self.last.1), self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        self.raster.draw_cubic(p0, p1, p2, p3);
        self.last = (x, y);
    }
    fn close(&mut self) {
        if self.last != self.start {
            self.line_to(self.start.0, self.start.1);
        }
    }
}

/// A fallback character's lit pixels in its cell, with their coverage.
type Coverage = Arc<Vec<(u16, u16, f32)>>;

/// The pixels of `glyph` drawn into `cell`, kept so scrolling text isn't
/// drawn from its outline again every frame. Only where the cell is
/// matters, not where it is.
fn rasterized(font: usize, glyph: GlyphId, em_px: f32, cell: &Cell) -> Coverage {
    type Key = (usize, u32, u32, u32, u32, u32);
    static DRAWN: OnceLock<Mutex<HashMap<Key, Coverage>>> = OnceLock::new();
    /// Enough for a few screens of text; past it, the cache starts over.
    const KEPT: usize = 4096;
    let key = (
        font,
        glyph.to_u32(),
        em_px.to_bits(),
        cell.width.to_bits(),
        cell.height.to_bits(),
        (cell.baseline - cell.top).to_bits(),
    );
    let drawn = DRAWN.get_or_init(Default::default);
    if let Some(pixels) = drawn.lock().expect("glyph cache lock").get(&key) {
        return Arc::clone(pixels);
    }
    let pixels = Arc::new(draw_outline(&fallback_fonts()[font], glyph, em_px, cell));
    let mut drawn = drawn.lock().expect("glyph cache lock");
    if drawn.len() >= KEPT {
        drawn.clear();
    }
    drawn.insert(key, Arc::clone(&pixels));
    pixels
}

fn draw_outline(font: &FontRef, glyph: GlyphId, em_px: f32, cell: &Cell) -> Vec<(u16, u16, f32)> {
    let mut pixels = Vec::new();
    let Some(outline) = font.outline_glyphs().get(glyph) else { return pixels };
    let mut pen = Pen {
        raster: Rasterizer::new(cell.width as usize, cell.height as usize),
        baseline: cell.baseline - cell.top,
        width: cell.width,
        height: cell.height,
        start: (0.0, 0.0),
        last: (0.0, 0.0),
    };
    if outline.draw(DrawSettings::unhinted(Size::new(em_px), LocationRef::default()), &mut pen).is_err() {
        return pixels;
    }
    pen.raster.for_each_pixel_2d(|px, py, coverage| {
        if coverage > 0.5 / 255.0 {
            pixels.push((px as u16, py as u16, coverage.min(1.0)));
        }
    });
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_loads_with_ascii_and_beyond() {
        for c in (' '..='~').chain("éЖ←".chars()) {
            assert!(glyphs().contains_key(&c), "missing {c:?}");
        }
        assert!(matches!(Shape::of('\u{1F600}'), Shape::Bitmap(g) if g == &glyphs()[&'?']));
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
        layout("I", 10.0, 20.0, 24.0, |x0, y0, x1, y1, _| runs.push((x0, y0, x1, y1)));
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

    /// Japanese comes from the bundled Noto fonts, inside the box measured
    /// for it, wider than a Monogram character.
    #[test]
    fn cjk_falls_back_to_the_bundled_fonts() {
        let (w, h) = measure("真央", 24.0);
        assert!(w > measure("AB", 24.0).0, "{w}");
        let mut lit = 0.0;
        layout("真央", 0.0, 0.0, 24.0, |x0, y0, x1, y1, coverage| {
            assert!(x0 >= 0.0 && x1 <= w + 1.0 && y0 >= 0.0 && y1 <= h, "{x0},{y0} {x1},{y1}");
            lit += coverage;
        });
        assert!(lit > 40.0, "{lit}");
    }
}
