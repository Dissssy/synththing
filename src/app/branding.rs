//! The wordmark (`assets/synththing-wordmark.svg`) in the menu bar, drawn
//! at the exact pixel size it's shown at, so it's sharp at any UI scale.
//! It's in the build's accent (`build_info::ACCENT`: purple for a release,
//! another color for canary, each branch build and local builds, like the
//! icon). On a
//! theme whose menu bar is too close to that (a custom one, say), it gets a
//! thin black or white halo.

use eframe::egui;
use resvg::{tiny_skia, usvg};

const WORDMARK_SVG: &str = include_str!("../../assets/synththing-wordmark.svg");

/// The wordmark's fill in the SVG, replaced with the build's accent.
const SVG_COLOR: &str = "#8B6CF6";

/// The build's accent, what the wordmark is drawn in.
fn wordmark_color() -> egui::Color32 {
    let hex = crate::build_info::ACCENT.trim_start_matches('#');
    let channel = |i: usize| hex.get(i..i + 2).and_then(|c| u8::from_str_radix(c, 16).ok()).unwrap_or(0);
    egui::Color32::from_rgb(channel(0), channel(2), channel(4))
}

/// Below this contrast ratio (WCAG's, 1 to 21) with the background, the
/// wordmark gets a halo. 3:1 is the usual floor for large text and logos.
const MIN_CONTRAST: f32 = 3.0;

/// The wordmark, parsed once, and its texture for the size and halo it was
/// last drawn with.
#[derive(Default)]
pub(super) struct Wordmark {
    tree: Option<usvg::Tree>,
    parsed: bool,
    texture: Option<((u32, Option<egui::Color32>), egui::TextureHandle)>,
}

impl Wordmark {
    /// The wordmark `height` points tall, for a menu bar filled with
    /// `background`; `None` if the SVG can't be read (the caller shows
    /// the name as text instead).
    pub(super) fn image(&mut self, ctx: &egui::Context, height: f32, background: egui::Color32) -> Option<egui::Image<'static>> {
        if !self.parsed {
            self.parsed = true;
            let svg = WORDMARK_SVG.replace(SVG_COLOR, crate::build_info::ACCENT);
            self.tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
                .map_err(|e| log::warn!("the wordmark couldn't be read: {e}"))
                .ok();
        }
        let tree = self.tree.as_ref()?;
        let pixels_per_point = ctx.pixels_per_point();
        let height_px = ((height * pixels_per_point).round() as u32).clamp(8, 512);
        let halo = (contrast(wordmark_color(), background) < MIN_CONTRAST).then(|| crate::app::on_color(background));
        let key = (height_px, halo);
        if self.texture.as_ref().is_none_or(|(drawn, _)| *drawn != key) {
            let image = render(tree, height_px, halo, pixels_per_point);
            let texture = ctx.load_texture("wordmark", image, egui::TextureOptions::LINEAR);
            self.texture = Some((key, texture));
        }
        let (_, texture) = self.texture.as_ref()?;
        let size = texture.size_vec2() / pixels_per_point;
        Some(egui::Image::from_texture(egui::load::SizedTexture::new(texture.id(), size)).alt_text("synththing"))
    }
}

/// The wordmark `height_px` pixels tall (plus room for the halo), with a
/// `halo`-colored outline about a point wide around it when given.
fn render(tree: &usvg::Tree, height_px: u32, halo: Option<egui::Color32>, pixels_per_point: f32) -> egui::ColorImage {
    let radius = if halo.is_some() { (pixels_per_point.round() as i32).max(1) } else { 0 };
    let svg = tree.size();
    let scale = height_px as f32 / svg.height();
    let pad = radius as u32;
    let (w, h) = ((svg.width() * scale).ceil() as u32 + 2 * pad, height_px + 2 * pad);
    let Some(mut pixmap) = tiny_skia::Pixmap::new(w, h) else {
        return egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]);
    };
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(pad as f32, pad as f32);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let glyph: Vec<[u8; 4]> = pixmap
        .pixels()
        .iter()
        .map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    let (w, h) = (w as i32, h as i32);
    let pixels = match halo {
        None => glyph.iter().map(|&[r, g, b, a]| egui::Color32::from_rgba_unmultiplied(r, g, b, a)).collect(),
        Some(halo) => {
            // The halo: the glyph's shape grown by `radius` (a disc), with
            // the glyph drawn over it.
            let alpha = |x: i32, y: i32| -> u8 {
                if x < 0 || y < 0 || x >= w || y >= h { 0 } else { glyph[(y * w + x) as usize][3] }
            };
            let mut out = Vec::with_capacity(glyph.len());
            for y in 0..h {
                for x in 0..w {
                    let mut grown = 0u8;
                    for dy in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx * dx + dy * dy <= radius * radius {
                                grown = grown.max(alpha(x + dx, y + dy));
                            }
                        }
                    }
                    let [r, g, b, a] = glyph[(y * w + x) as usize];
                    let (fa, ha) = (f32::from(a) / 255.0, f32::from(grown) / 255.0);
                    let out_a = fa + ha * (1.0 - fa);
                    let mix = |f: u8, back: u8| {
                        if out_a <= 0.0 { 0 } else { ((f32::from(f) * fa + f32::from(back) * ha * (1.0 - fa)) / out_a).round() as u8 }
                    };
                    out.push(egui::Color32::from_rgba_unmultiplied(
                        mix(r, halo.r()),
                        mix(g, halo.g()),
                        mix(b, halo.b()),
                        (out_a * 255.0).round() as u8,
                    ));
                }
            }
            out
        }
    };
    egui::ColorImage::new([w as usize, h as usize], pixels)
}

/// WCAG's contrast ratio between two colors: 1 (the same) to 21 (black on
/// white).
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let luminance = |c: egui::Color32| {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    };
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stock themes' menu bars don't need a halo with stable's purple;
    /// a background close to it does. (Test builds' colors get one where
    /// they need it, a light theme with canary's yellow, say.)
    #[test]
    fn halo_only_when_hard_to_read() {
        let purple = egui::Color32::from_rgb(0x8B, 0x6C, 0xF6);
        assert!((contrast(egui::Color32::BLACK, egui::Color32::WHITE) - 21.0).abs() < 0.01);
        assert!(contrast(purple, egui::Visuals::dark().panel_fill) >= MIN_CONTRAST);
        assert!(contrast(purple, egui::Visuals::light().panel_fill) >= MIN_CONTRAST);
        assert!(contrast(purple, egui::Color32::from_rgb(0x7A, 0x60, 0xD8)) < MIN_CONTRAST);
    }

    /// The wordmark is drawn in the build's accent, read right.
    #[test]
    fn the_accent_is_read() {
        let c = wordmark_color();
        assert_eq!(format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b()), crate::build_info::ACCENT.to_ascii_uppercase());
        assert!(WORDMARK_SVG.contains(SVG_COLOR), "the SVG still has the color that's replaced");
    }

    /// It renders at the height asked for, wider than tall, with a margin
    /// for the halo when there is one.
    #[test]
    fn renders_at_the_size_asked() {
        let tree = usvg::Tree::from_data(WORDMARK_SVG.as_bytes(), &usvg::Options::default()).unwrap();
        let plain = render(&tree, 40, None, 1.0);
        assert_eq!(plain.size[1], 40);
        assert!(plain.size[0] > 100);
        assert!(plain.pixels.iter().any(|p| p.a() == 255), "something's drawn");
        let haloed = render(&tree, 40, Some(egui::Color32::WHITE), 2.0);
        assert_eq!(haloed.size, [plain.size[0] + 4, 44]);
    }
}
