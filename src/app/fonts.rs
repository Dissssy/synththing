//! The fonts: egui's own, with the Phosphor icons (`app::icon`) mixed in;
//! then Noto Sans JP and KR in Light, to match egui's Ubuntu Light, for
//! Japanese, Korean, most Chinese and CJK punctuation like 『 』; and,
//! behind those, fonts installed on this computer for every character none
//! of them has (other scripts, rarer Chinese characters, symbols). So song
//! names and tags show as written instead of as boxes.
//!
//! Installed fonts are looked for on a background thread, since that means
//! reading every font on the system; until it's done, text is drawn with
//! the built-in fonts alone.

use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use skrifa::{FontRef, MetadataProvider, Tag};

/// Sets egui's fonts now, then again with the installed fonts behind them
/// once those are found.
pub fn install(ctx: &egui::Context) {
    let fonts = bundled();
    ctx.set_fonts(fonts.clone());
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("fonts".into()).spawn(move || {
        let fallbacks = installed_fallbacks(&fonts);
        if fallbacks.is_empty() {
            return;
        }
        log::info!("falling back on installed fonts: {}", fallbacks.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", "));
        ctx.set_fonts(with_fallbacks(fonts, fallbacks));
        ctx.request_repaint();
    });
    if let Err(e) = spawned {
        log::warn!("couldn't look for installed fonts: {e}");
    }
}

/// The fonts built into the app: egui's own, the Phosphor icons, and Noto
/// Sans JP and KR (`assets/fonts/noto-sans-cjk`) after them. JP comes
/// first, so the Chinese characters both have are drawn the Japanese way.
fn bundled() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    let cjk: [(&str, &'static [u8]); 2] = [
        ("NotoSansJP-Light", include_bytes!("../../assets/fonts/noto-sans-cjk/NotoSansJP-Light.ttf")),
        ("NotoSansKR-Light", include_bytes!("../../assets/fonts/noto-sans-cjk/NotoSansKR-Light.ttf")),
    ];
    let cjk = cjk.into_iter().map(|(name, bytes)| (name.to_owned(), FontData::from_static(bytes))).collect();
    with_fallbacks(fonts, cjk)
}

/// `fonts` with `fallbacks` after everything else, in both families: they
/// only draw what no font before them has.
fn with_fallbacks(mut fonts: FontDefinitions, fallbacks: Vec<(String, FontData)>) -> FontDefinitions {
    for (name, data) in fallbacks {
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        fonts.font_data.insert(name, Arc::new(data));
    }
    fonts
}

/// Installed fonts that between them have the characters `fonts` lack.
fn installed_fallbacks(fonts: &FontDefinitions) -> Vec<(String, FontData)> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    // fontdb doesn't know Android's font folder.
    #[cfg(target_os = "android")]
    db.load_fonts_dir("/system/fonts");
    let covered = fonts
        .font_data
        .values()
        .filter_map(|data| characters(&data.font, data.index))
        .flatten()
        .collect();
    let chosen = choose(&db, covered);
    load(&mut db, chosen)
}

/// The faces in `db` to add, in order, to fonts that have the characters
/// in `covered`, so that between them they have every character a face in
/// `db` has.
///
/// Each step takes the face with the most characters still missing, from
/// the best group that has any: regular sans-serif faces, then serif, then
/// monospaced, then bold, italic and the like, then Unifont (a
/// last-resort font with every character, drawn as pixels). So one big
/// font (a CJK one for the rarer Chinese characters, say) covers all it
/// can at once, fonts for other scripts follow, and
/// Unifont only fills in what nothing else has. Ties go to the first name
/// alphabetically, so the same fonts are chosen every launch.
fn choose(db: &fontdb::Database, mut covered: HashSet<u32>) -> Vec<fontdb::ID> {
    let mut candidates: Vec<(&fontdb::FaceInfo, Vec<u32>)> = db
        .faces()
        .filter_map(|face| Some((face, db.with_face_data(face.id, characters).flatten()?)))
        .collect();
    candidates.sort_by(|(a, _), (b, _)| (group(a), &a.post_script_name).cmp(&(group(b), &b.post_script_name)));

    let mut chosen = Vec::new();
    loop {
        for (_, chars) in &mut candidates {
            chars.retain(|c| !covered.contains(c));
        }
        candidates.retain(|(_, chars)| !chars.is_empty());
        // The first candidate is in the best group left; the most missing
        // characters in that group wins, the first name on a tie.
        let Some(best) = candidates.first().map(|(face, _)| group(face)) else {
            break;
        };
        let (i, _) = candidates
            .iter()
            .enumerate()
            .take_while(|(_, (face, _))| group(face) == best)
            .fold((0, 0), |(i, most), (j, (_, chars))| if chars.len() > most { (j, chars.len()) } else { (i, most) });
        let (face, chars) = candidates.remove(i);
        covered.extend(chars);
        chosen.push(face.id);
    }
    chosen
}

/// Which group of fonts `face` is in for `choose`, best first.
fn group(face: &fontdb::FaceInfo) -> u8 {
    let family = face.families.first().map_or(String::new(), |(name, _)| name.to_lowercase());
    let regular = face.weight == fontdb::Weight::NORMAL && face.style == fontdb::Style::Normal && face.stretch == fontdb::Stretch::Normal;
    if family.contains("unifont") {
        4
    } else if !regular {
        3
    } else if face.monospaced {
        2
    } else if family.contains("serif") && !family.contains("sans") {
        1
    } else {
        0
    }
}

/// The faces `ids` in `db`, as named font data. The font files are mapped
/// into memory rather than read, so only the parts of them that get drawn
/// are ever loaded: a CJK font is tens of megabytes.
fn load(db: &mut fontdb::Database, ids: Vec<fontdb::ID>) -> Vec<(String, FontData)> {
    ids.into_iter()
        .filter_map(|id| {
            let name = db.face(id)?.post_script_name.clone();
            // SAFETY: a font file written over while it's mapped could crash
            // the app. Installing or updating fonts replaces the files
            // rather than writing into them, which leaves the mapping as it
            // was; browsers map system fonts the same way.
            let (shared, index) = unsafe { db.make_shared_face_data(id)? };
            // egui wants the bytes for good, and the fonts are kept until
            // the app quits anyway.
            let shared: &'static _ = Box::leak(Box::new(shared));
            let bytes: &'static [u8] = (**shared).as_ref();
            Some((format!("{name} #{index}"), FontData { index, ..FontData::from_static(bytes) }))
        })
        .collect()
}

/// The characters face `index` of font file `data` can draw, leaving out
/// control characters and the private use areas (where each font, the
/// Phosphor icons among them, means something different). `None` if it
/// isn't a font egui can read, or has no outlines, only color bitmaps
/// (Noto Color Emoji, Apple Color Emoji), which egui can't draw.
fn characters(data: &[u8], index: u32) -> Option<Vec<u32>> {
    let font = FontRef::from_index(data, index).ok()?;
    if ![b"glyf", b"CFF ", b"CFF2"].iter().any(|tag| font.table_data(Tag::new(tag)).is_some()) {
        return None;
    }
    let chars = font
        .charmap()
        .mappings()
        .map(|(c, _)| c)
        .filter(|&c| char::from_u32(c).is_some_and(|c| !c.is_control()) && !(0xE000..=0xF8FF).contains(&c) && c < 0xF0000)
        .collect();
    Some(chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled_font(name: &str) -> Vec<u8> {
        bundled().font_data[name].font.to_vec()
    }

    #[test]
    fn chooses_fonts_for_missing_characters_only() {
        let mut db = fontdb::Database::new();
        db.load_font_data(bundled_font("Ubuntu-Light"));
        db.load_font_data(bundled_font("NotoEmoji-Regular"));
        // Hack's characters are all there already, so Ubuntu Light is
        // only chosen for the rest; Noto Emoji has its own.
        let hack = bundled_font("Hack");
        let covered = characters(&hack, 0).unwrap().into_iter().collect();

        let chosen = choose(&db, covered);
        let picked = load(&mut db, chosen);
        let names: Vec<_> = picked.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("Ubuntu")), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("NotoEmoji")), "{names:?}");
    }

    #[test]
    fn skips_fonts_with_nothing_new() {
        let mut db = fontdb::Database::new();
        let hack = bundled_font("Hack");
        let covered = characters(&hack, 0).unwrap().into_iter().collect();
        db.load_font_data(hack);
        assert!(choose(&db, covered).is_empty());
    }

    #[test]
    fn leaves_out_private_use_characters() {
        let phosphor = bundled_font("phosphor");
        assert!(characters(&phosphor, 0).unwrap().iter().all(|c| !(0xE000..=0xF8FF).contains(c)));
    }
}
