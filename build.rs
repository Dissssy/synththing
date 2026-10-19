//! Generates `$OUT_DIR/bundled_scripts.rs`: `BUNDLED_SCRIPTS`, a
//! `&[(&str, &str, &str)]` of (category, file name, contents) for every
//! `.lua` file in `assets/visualizers/<category>/`, the categories being
//! `visualizers`, `games`, `templates` and `examples`. Adding or removing a
//! bundled script is then just a file move, nothing in `src/` needs
//! editing to pick it up.
//!
//! Visualizers and games are added to the user's script folder; templates
//! and examples are only offered by "New". See `lua_visualizer.rs`.
//!
//! Also generates `$OUT_DIR/scripting_reference.rs` from
//! `docs/scripting-reference.md`, the in-app Scripting Reference (see
//! `lua_docs.rs` and `parse_reference` below), and the app icon from
//! `assets/synththing-icon.svg`: `$OUT_DIR/synththing.ico`, embedded in the
//! Windows exe, `$OUT_DIR/icon_256.rgba` for the window (`desktop.rs`) and
//! `$OUT_DIR/icon_32.rgba` for the tray (`tray.rs`).
//!
//! And stamps the build (`src/build_info.rs`): `SYNTHTHING_VERSION`,
//! `SYNTHTHING_CHANNEL` and `SYNTHTHING_COMMIT`, from what CI sets for a
//! canary or branch build (`stamp_build` below), and `SYNTHTHING_ACCENT`,
//! the color the icon and the wordmark are drawn in for its channel
//! (`accent` below).

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

// (Only the id and the name are used here.)
#[allow(dead_code)]
#[path = "src/android_id.rs"]
mod android_id;

/// The bundled script folders, in the order they're listed.
const CATEGORIES: [&str; 4] = ["visualizers", "games", "templates", "examples"];

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");

    // The main thread's stack: 16 MB, as the threads that parse Lua get
    // (script_host, offline, the server). full_moon's parser recurses
    // deeply, and the main thread parses too (bundled scripts at startup,
    // the editor's analysis, publishing); Windows gives it only 1 MB, which
    // a debug build overflowed at startup. (Linux gives it 8 MB already.)
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:16777216"),
            _ => println!("cargo:rustc-link-arg-bins=-Wl,--stack,16777216"),
        }
    }

    let mut generated = String::from("pub static BUNDLED_SCRIPTS: &[(&str, &str, &str)] = &[\n");
    for category in CATEGORIES {
        for (name, absolute_path) in list_scripts(&manifest_dir, category) {
            // With Unix line endings, whatever the checkout has (git on
            // Windows may give CRLF): a bundled script is then byte for byte
            // what's published from it, so its hash finds its version.
            let contents = fs::read_to_string(&absolute_path)
                .unwrap_or_else(|e| panic!("reading {absolute_path}: {e}"))
                .replace("\r\n", "\n");
            println!("cargo:rerun-if-changed={absolute_path}");
            generated.push_str(&format!("    ({category:?}, {name:?}, {contents:?}),\n"));
        }
        println!("cargo:rerun-if-changed=assets/visualizers/{category}");
    }
    generated.push_str("];\n");

    let out_path = Path::new(&out_dir).join("bundled_scripts.rs");
    fs::write(&out_path, generated)
        .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));

    generate_reference(&manifest_dir, &out_dir);
    let channel = stamp_build();
    let accent = accent(&channel);
    println!("cargo:rustc-env=SYNTHTHING_ACCENT={accent}");
    generate_icon(&manifest_dir, &out_dir, &accent);
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        write_android_build(&out_dir, &channel, &accent);
    }
}

/// What the APK takes from this build, which Gradle packages after it: the
/// channel, the app's id and launcher name for it (`src/android_id.rs`) and
/// the icon's accent. In a file beside the library
/// (`target/<target>/<profile>/synththing-android.properties`, which
/// android/app/build.gradle.kts reads): `OUT_DIR` is three folders below.
fn write_android_build(out_dir: &str, channel: &str, accent: &str) {
    let profile_dir = Path::new(out_dir).ancestors().nth(3).expect("OUT_DIR is target/<target>/<profile>/build/<crate>/out");
    let path = profile_dir.join("synththing-android.properties");
    let properties = format!(
        "channel={channel}\napplicationId={}\nlabel={}\naccent={accent}\n",
        android_id::application_id(channel),
        android_id::label(channel),
    );
    fs::write(&path, properties).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}

// --- build stamp ---------------------------------------------------------

/// The version, channel and commit `src/build_info.rs` reports, from what
/// CI sets (docs/design/release-channels.md):
///
/// - `SYNTHTHING_CHANNEL`: `nightly`, or `branch-<name>`. Unset (or
///   `stable`): a release, or a build made anywhere else, which is
///   Cargo.toml's version.
/// - `SYNTHTHING_BUILD_NUMBER`: for a canary or branch build, the commits
///   since the release tag. The version is then the next patch version as
///   a prerelease, `0.7.1-nightly.12` or `0.7.1-branch.3`: after the release
///   it's built on, before the next.
/// - `SYNTHTHING_COMMIT`: the commit it's built from, if known.
///
/// Returns the channel (`stable`, `nightly`, `branch-<name>`).
fn stamp_build() -> String {
    for var in ["SYNTHTHING_CHANNEL", "SYNTHTHING_BUILD_NUMBER", "SYNTHTHING_COMMIT"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let package = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION not set");
    let channel = env::var("SYNTHTHING_CHANNEL").unwrap_or_default();
    let channel = if channel.is_empty() { "stable".to_string() } else { channel };
    let version = if channel == "stable" {
        package
    } else {
        let kind = match channel.as_str() {
            "nightly" => "nightly",
            c if c.strip_prefix("branch-").is_some_and(|name| !name.is_empty()) => "branch",
            c => panic!("SYNTHTHING_CHANNEL is {c:?}: it's nightly, branch-<name> or stable"),
        };
        let number: u64 = env::var("SYNTHTHING_BUILD_NUMBER")
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("a {channel} build needs SYNTHTHING_BUILD_NUMBER (commits since the release)"));
        let parts: Vec<u64> = package.split('.').map(|p| p.parse().expect("Cargo.toml version is plain x.y.z")).collect();
        let [major, minor, patch] = parts[..] else { panic!("Cargo.toml version is plain x.y.z") };
        format!("{major}.{minor}.{}-{kind}.{number}", patch + 1)
    };
    let commit = env::var("SYNTHTHING_COMMIT").unwrap_or_default();
    println!("cargo:rustc-env=SYNTHTHING_VERSION={version}");
    println!("cargo:rustc-env=SYNTHTHING_CHANNEL={channel}");
    println!("cargo:rustc-env=SYNTHTHING_COMMIT={}", commit.trim());
    channel
}

/// The color a build's icon and wordmark are drawn in, so which build is
/// which shows at a glance (in the taskbar, the tray, the menu bar): the
/// purple for a release, canary yellow for canary, and for a branch build a
/// hue of its own, from its name (it keeps it through its updates), at
/// stable's saturation and brightness, never near the other two. Built
/// anywhere but GitHub Actions (which sets `GITHUB_ACTIONS`), off-white,
/// whatever the channel: a local build, debug or release, never passes for
/// one users get.
fn accent(channel: &str) -> String {
    println!("cargo:rerun-if-env-changed=GITHUB_ACTIONS");
    if env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return "#E6E1D6".to_string();
    }
    match channel {
        "stable" => STABLE_ACCENT.to_string(),
        "nightly" => "#F2C230".to_string(),
        branch => {
            // FNV-1a, so a branch's color is the same from any toolchain.
            let hash = branch.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
            // The hues, leaving out canary's yellow (30-60) and stable's
            // purple (230-280): 280 to pick from.
            let pick = (hash % 280) as f32;
            let hue = if pick < 30.0 {
                pick
            } else if pick < 200.0 {
                pick + 30.0
            } else {
                pick + 80.0
            };
            hsv_hex(hue, 0.6, 0.96)
        }
    }
}

/// `#RRGGBB` for a hue (degrees), saturation and value (0 to 1).
fn hsv_hex(hue: f32, saturation: f32, value: f32) -> String {
    let c = value * saturation;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    let byte = |v: f32| ((v + m) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", byte(r), byte(g), byte(b))
}

// --- app icon ------------------------------------------------------------

/// The sizes in the .ico: what Windows picks from for the title bar, the
/// taskbar, Explorer's views and Alt+Tab, at every display scale.
const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// The purple in the icon and wordmark SVGs: what `accent` replaces.
const STABLE_ACCENT: &str = "#8B6CF6";

/// The icon and the wordmark in `accent` (`#RRGGBB`) instead of their
/// purple.
fn generate_icon(manifest_dir: &str, out_dir: &str, accent: &str) {
    let svg_path = Path::new(manifest_dir).join("assets").join("synththing-icon.svg");
    println!("cargo:rerun-if-changed=assets/synththing-icon.svg");
    let svg = fs::read_to_string(&svg_path).unwrap_or_else(|e| panic!("reading {}: {e}", svg_path.display()));
    assert!(svg.contains(STABLE_ACCENT), "{} has no {STABLE_ACCENT} to recolor", svg_path.display());
    let svg = svg.replace(STABLE_ACCENT, accent).into_bytes();
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default())
        .unwrap_or_else(|e| panic!("{}: {e}", svg_path.display()));

    // Each size as a PNG; an .ico can hold PNGs (Windows Vista on).
    let mut images: Vec<(u32, Vec<u8>)> = Vec::new();
    for size in ICON_SIZES {
        let rgba = render_square(&tree, size);
        if size == 256 || size == 32 {
            let raw = Path::new(out_dir).join(format!("icon_{size}.rgba"));
            fs::write(&raw, &rgba).unwrap_or_else(|e| panic!("writing {}: {e}", raw.display()));
        }
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, size, size);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer.write_image_data(&rgba).expect("PNG data");
        }
        images.push((size, png_bytes));
    }

    // ICONDIR, then one ICONDIRENTRY per image, then the images.
    let mut ico = Vec::new();
    ico.extend(0u16.to_le_bytes());
    ico.extend(1u16.to_le_bytes()); // an icon (not a cursor)
    ico.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let side = if *size >= 256 { 0 } else { *size as u8 }; // 0 means 256
        ico.extend([side, side, 0, 0]);
        ico.extend(1u16.to_le_bytes()); // color planes
        ico.extend(32u16.to_le_bytes()); // bits per pixel
        ico.extend((data.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        ico.extend(data);
    }
    let ico_path = Path::new(out_dir).join("synththing.ico");
    fs::write(&ico_path, &ico).unwrap_or_else(|e| panic!("writing {}: {e}", ico_path.display()));

    // Into the exe, for Explorer and the taskbar. Without Windows' resource
    // compiler (no Windows SDK) the build still works, with a plain icon.
    #[cfg(windows)]
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(ico_path.to_str().expect("OUT_DIR is valid UTF-8"));
        if let Err(e) = resource.compile() {
            println!("cargo:warning=the app icon isn't in the exe (no resource compiler?): {e}");
        }
    }
}

/// The SVG drawn into a `size` x `size` square, as straight (not
/// premultiplied) RGBA.
fn render_square(tree: &resvg::usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("a nonzero size");
    let svg_size = tree.size();
    let scale = size as f32 / svg_size.width().max(svg_size.height());
    resvg::render(tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// `(file name, absolute path)` for every `.lua` file directly inside
/// `<manifest_dir>/assets/visualizers/<subdir>`, sorted by name.
fn list_scripts(manifest_dir: &str, subdir: &str) -> Vec<(String, String)> {
    let dir = Path::new(manifest_dir).join("assets").join("visualizers").join(subdir);
    let read_dir = fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));

    let mut scripts: Vec<(String, String)> = read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("lua") {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            let absolute = path
                .canonicalize()
                .unwrap_or_else(|e| panic!("resolving {}: {e}", path.display()))
                .to_str()?
                .to_string();
            Some((name, absolute))
        })
        .collect();
    scripts.sort();
    scripts
}


// --- scripting reference -------------------------------------------------

enum Block {
    P(String),
    Code(String),
    Bullets(Vec<String>),
}

fn generate_reference(manifest_dir: &str, out_dir: &str) {
    let path = Path::new(manifest_dir).join("docs").join("scripting-reference.md");
    println!("cargo:rerun-if-changed=docs/scripting-reference.md");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let sections = parse_reference(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

    // `{:?}` on a string is a correctly escaped Rust string literal.
    let mut out = String::new();
    writeln!(out, "pub static SECTIONS: &[Section] = &[").unwrap();
    for (title, blocks) in &sections {
        writeln!(out, "    Section {{ title: {title:?}, blocks: &[").unwrap();
        for block in blocks {
            match block {
                Block::P(text) => writeln!(out, "        Block::P({text:?}),").unwrap(),
                Block::Code(code) => writeln!(out, "        Block::Code({code:?}),").unwrap(),
                Block::Bullets(items) => writeln!(out, "        Block::Bullets(&{items:?}),").unwrap(),
            }
        }
        writeln!(out, "    ] }},").unwrap();
    }
    writeln!(out, "];").unwrap();

    let out_path = Path::new(out_dir).join("scripting_reference.rs");
    fs::write(&out_path, out).unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
}

/// The small Markdown subset the reference uses: `## ` sections (the `# `
/// title is skipped), paragraphs (lines joined with spaces), ``` fenced
/// code (verbatim), `- ` bullets (an indented or unprefixed next line
/// continues the bullet), and `<!-- -->` comments (skipped). Inline
/// `code` is left in the text; the app renders it.
fn parse_reference(text: &str) -> Result<Vec<(String, Vec<Block>)>, String> {
    let mut sections: Vec<(String, Vec<Block>)> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut bullets: Vec<String> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let mut in_comment = false;

    fn flush(
        sections: &mut [(String, Vec<Block>)],
        paragraph: &mut Vec<String>,
        bullets: &mut Vec<String>,
        line_no: usize,
    ) -> Result<(), String> {
        if paragraph.is_empty() && bullets.is_empty() {
            return Ok(());
        }
        let Some((_, blocks)) = sections.last_mut() else {
            return Err(format!("line {line_no}: text before the first `## ` heading"));
        };
        if !paragraph.is_empty() {
            blocks.push(Block::P(paragraph.join(" ")));
            paragraph.clear();
        }
        if !bullets.is_empty() {
            blocks.push(Block::Bullets(std::mem::take(bullets)));
        }
        Ok(())
    }

    for (i, line) in text.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = line.trim();

        if let Some(lines) = &mut code {
            if trimmed.starts_with("```") {
                let block = Block::Code(lines.join("\n"));
                code = None;
                match sections.last_mut() {
                    Some((_, blocks)) => blocks.push(block),
                    None => return Err(format!("line {line_no}: code before the first `## ` heading")),
                }
            } else {
                lines.push(line.to_string());
            }
            continue;
        }
        if in_comment {
            in_comment = !trimmed.contains("-->");
            continue;
        }
        if trimmed.starts_with("<!--") {
            in_comment = !trimmed.contains("-->");
            continue;
        }

        if trimmed.starts_with("```") {
            flush(&mut sections, &mut paragraph, &mut bullets, line_no)?;
            code = Some(Vec::new());
        } else if let Some(title) = trimmed.strip_prefix("## ") {
            flush(&mut sections, &mut paragraph, &mut bullets, line_no)?;
            sections.push((title.trim().to_string(), Vec::new()));
        } else if trimmed.starts_with("# ") || trimmed.is_empty() {
            flush(&mut sections, &mut paragraph, &mut bullets, line_no)?;
        } else if let Some(item) = trimmed.strip_prefix("- ") {
            if !paragraph.is_empty() {
                flush(&mut sections, &mut paragraph, &mut Vec::new(), line_no)?;
            }
            bullets.push(item.trim().to_string());
        } else if let Some(last) = bullets.last_mut().filter(|_| paragraph.is_empty()) {
            last.push(' ');
            last.push_str(trimmed);
        } else {
            paragraph.push(trimmed.to_string());
        }
    }
    if code.is_some() {
        return Err("unterminated ``` code block".to_string());
    }
    if in_comment {
        return Err("unterminated <!-- comment".to_string());
    }
    flush(&mut sections, &mut paragraph, &mut bullets, text.lines().count())?;
    if sections.is_empty() {
        return Err("no `## ` sections".to_string());
    }
    Ok(sections)
}
