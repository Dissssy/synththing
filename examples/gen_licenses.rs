//! Regenerates `assets/licenses/third_party.json`: every library compiled
//! into synththing for Windows (from `cargo metadata`), with its declared
//! license, authors, repository and license texts. The app shows these
//! under Help > Credits & licenses.
//!
//! License texts are the files each crate ships (LICENSE*, COPYING*,
//! NOTICE*, ...). A crate that ships none gets the standard text of each
//! license its declared expression names, from `assets/licenses/spdx/`
//! (SPDX's license-list-data), marked as the standard text. Identical texts
//! are stored once.
//!
//! Run after changing dependencies (a test fails until you do):
//!
//!     cargo run --example gen_licenses

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// The platform release builds are made for; dependencies only used on
/// other platforms aren't in the binary, so they aren't listed.
const TARGET: &str = "x86_64-pc-windows-msvc";

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--filter-platform", TARGET])
        .current_dir(root)
        .output()
        .expect("running cargo metadata");
    assert!(output.status.success(), "cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr));
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("cargo metadata output is JSON");

    let in_build: HashSet<&str> = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .filter_map(|n| n["id"].as_str())
        .collect();

    let mut texts: Vec<String> = Vec::new();
    let mut text_index: HashMap<String, usize> = HashMap::new();
    let mut intern = |text: String| -> usize {
        *text_index.entry(text.clone()).or_insert_with(|| {
            texts.push(text);
            texts.len() - 1
        })
    };

    // Sorted by name then version, so the file diffs cleanly.
    let mut packages: BTreeMap<(String, String), Value> = BTreeMap::new();
    for package in metadata["packages"].as_array().expect("packages") {
        let id = package["id"].as_str().unwrap_or_default();
        let name = package["name"].as_str().unwrap_or_default().to_string();
        if !in_build.contains(id) || name == "synththing" {
            continue;
        }
        let version = package["version"].as_str().unwrap_or_default().to_string();
        let license = package["license"].as_str().unwrap_or("(not stated)").to_string();
        let dir = Path::new(package["manifest_path"].as_str().unwrap_or_default())
            .parent()
            .expect("manifest has a directory")
            .to_path_buf();
        let mut found = shipped_license_files(&dir);
        if found.is_empty() {
            found = standard_texts(root, &license);
        }
        let indices: Vec<usize> = found.into_iter().map(&mut intern).collect();
        packages.insert(
            (name.clone(), version.clone()),
            json!({
                "name": name,
                "version": version,
                "license": license,
                "authors": package["authors"],
                "repository": package["repository"],
                "texts": indices,
            }),
        );
    }

    let list: Vec<Value> = packages.into_values().collect();
    let document = json!({ "packages": list, "texts": texts });
    let out = root.join("assets/licenses/third_party.json");
    fs::write(&out, serde_json::to_string_pretty(&document).unwrap() + "\n").expect("writing third_party.json");
    println!("wrote {} packages ({} distinct license texts) to {}", list.len(), texts.len(), out.display());
}

/// Every license/notice file at the top of a crate's directory, each
/// headed with its file name.
fn shipped_license_files(dir: &Path) -> Vec<String> {
    let Ok(read_dir) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<_> = read_dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    let n = n.to_ascii_uppercase();
                    ["LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT"]
                        .iter()
                        .any(|prefix| n.starts_with(prefix))
                })
        })
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|p| {
            let text = fs::read_to_string(p).ok()?;
            let name = p.file_name()?.to_string_lossy().into_owned();
            Some(format!("== {name} ==\n{}", text.trim()))
        })
        .collect()
}

/// The standard text of each license named in an SPDX expression like
/// "(MIT OR Apache-2.0) AND OFL-1.1", from assets/licenses/spdx.
fn standard_texts(root: &Path, expression: &str) -> Vec<String> {
    let ids: Vec<&str> = expression
        .split(|c: char| c.is_whitespace() || "()/".contains(c))
        .filter(|t| !t.is_empty() && !["OR", "AND", "WITH"].contains(t))
        .collect();
    ids.iter()
        .map(|id| {
            let path = root.join("assets/licenses/spdx").join(format!("{id}.txt"));
            let text = fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!("no standard text for {id}: add assets/licenses/spdx/{id}.txt (from github.com/spdx/license-list-data)")
            });
            format!("== {id} (standard text; the crate ships no license file) ==\n{}", text.trim())
        })
        .collect()
}
