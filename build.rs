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
//! `lua_docs.rs` and `parse_reference` below).

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The bundled script folders, in the order they're listed.
const CATEGORIES: [&str; 4] = ["visualizers", "games", "templates", "examples"];

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");

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
