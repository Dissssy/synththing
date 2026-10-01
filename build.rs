//! Generates `$OUT_DIR/bundled_scripts.rs`: two `&[(&str, &str)]` arrays,
//! `DROPPED_SCRIPTS` and `UNDROPPED_SCRIPTS`, listing every `.lua` file in
//! `assets/visualizers/dropped` and `assets/visualizers/undropped`
//! respectively, each paired with an `include_str!` of its contents. Adding
//! or removing a bundled script is then just a file move — nothing in
//! `src/` needs editing to pick it up.
//!
//! "Dropped" scripts get auto-seeded into the user's script folder on first
//! launch; "undropped" ones (API demos etc.) only ever show up as "New"
//! templates. See `lua_visualizer.rs` for how both lists get used.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");

    let dropped = list_scripts(&manifest_dir, "dropped");
    let undropped = list_scripts(&manifest_dir, "undropped");

    let mut generated = String::new();
    write_array(&mut generated, "DROPPED_SCRIPTS", &dropped);
    write_array(&mut generated, "UNDROPPED_SCRIPTS", &undropped);

    let out_path = Path::new(&out_dir).join("bundled_scripts.rs");
    fs::write(&out_path, generated)
        .unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));

    println!("cargo:rerun-if-changed=assets/visualizers/dropped");
    println!("cargo:rerun-if-changed=assets/visualizers/undropped");
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

/// Emits `pub static {const_name}: &[(&str, &str)] = &[("name.lua",
/// include_str!("/abs/path/name.lua")), ...];` — `{:?}`-formatting each
/// string produces a properly escaped Rust string literal, backslashes in
/// Windows paths included.
fn write_array(out: &mut String, const_name: &str, scripts: &[(String, String)]) {
    out.push_str(&format!("pub static {const_name}: &[(&str, &str)] = &[\n"));
    for (name, absolute_path) in scripts {
        out.push_str(&format!("    ({name:?}, include_str!({absolute_path:?})),\n"));
    }
    out.push_str("];\n");
}
