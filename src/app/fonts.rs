//! The fonts: egui's own, with the Phosphor icons (`app::icon`) mixed in;
//! then Noto Emoji Light, the only source of emoji, so they're drawn flat and
//! the same everywhere; then Noto Sans JP and KR in Light, to match egui's
//! Ubuntu Light, for Japanese, Korean, most Chinese and CJK punctuation like
//! 『 』. Any character none of them has (other scripts, rarer Chinese
//! characters, symbols) is drawn with an installed font, found the first
//! time it's needed, so song names and tags show as written instead of as
//! boxes. Installed fonts with colour glyphs are never used.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontTweak};
use eframe::epaint::text::{FallbackRequest, FontInsert, FontProvider};
use egui_system_font_provider::SystemFontProvider;
use skrifa::{FontRef, Tag};

/// Sets egui's fonts and adds installed fonts behind them. Callers turn off
/// eframe's own `system_font_fallback`, which would add colour emoji.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(bundled());
    ctx.add_font_provider(Arc::new(MonochromeSystemFonts(SystemFontProvider::new())));
}

/// The family of the filled Phosphor icons (the regular ones draw in any
/// text), for the few drawn filled: a starred song's alien.
pub const PHOSPHOR_FILL: &str = "phosphor-fill";

/// The fonts built into the app: egui's own, the Phosphor icons, then Noto
/// Emoji and the CJK fonts after them.
fn bundled() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    egui_phosphor::add_font_bytes_as_family(&mut fonts, PHOSPHOR_FILL, egui_phosphor::Variant::Fill.font_bytes());
    let (name, bytes) = crate::bundled_fonts::EMOJI;
    // Drawn a little smaller, the way egui drew its own Noto Emoji.
    let emoji = FontData::from_static(bytes).tweak(FontTweak { scale: 0.81, ..Default::default() });
    let cjk = crate::bundled_fonts::CJK.into_iter().map(|(name, bytes)| (name.to_owned(), FontData::from_static(bytes)));
    for (name, data) in std::iter::once((name.to_owned(), emoji)).chain(cjk) {
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        fonts.font_data.insert(name, Arc::new(data));
    }
    fonts
}

/// egui's system font provider, minus fonts with colour glyphs (Segoe UI
/// Emoji, Apple Color Emoji, Noto Color Emoji): an emoji Noto Emoji lacks
/// draws as a box rather than in colour.
struct MonochromeSystemFonts(SystemFontProvider);

impl FontProvider for MonochromeSystemFonts {
    fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        let insert = self.0.font_for(request)?;
        if has_colour_glyphs(&insert.data) {
            log::debug!("not using {} for {:?}: it's a colour font", insert.name, request.cluster);
            return None;
        }
        log::info!("falling back on installed font {} for {:?}", insert.name, request.cluster);
        Some(insert)
    }
}

/// Does the font have colour glyphs: COLR layers, or CBDT or sbix bitmaps?
fn has_colour_glyphs(data: &FontData) -> bool {
    FontRef::from_index(data.bytes(), data.index)
        .is_ok_and(|font| [b"COLR", b"CBDT", b"sbix"].iter().any(|tag| font.table_data(Tag::new(tag)).is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_have_no_colour_glyphs() {
        for (name, data) in bundled().font_data {
            assert!(!has_colour_glyphs(&data), "{name}");
        }
    }

    #[test]
    fn emoji_come_before_cjk() {
        let fonts = bundled();
        let family = &fonts.families[&FontFamily::Proportional];
        let at = |name: &str| family.iter().position(|n| n == name).unwrap();
        assert!(at("NotoEmoji-Light") < at("NotoSansJP-Light"));
    }
}
