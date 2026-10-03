//! Data for Help > Credits & licenses: who made synththing (the GitHub
//! repository's contributors, fetched when the modal first opens), the
//! Monogram font, Lua, and every library compiled into the app with its
//! license text (`assets/licenses/third_party.json`, regenerated with
//! `cargo run --example gen_licenses`).

use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use serde::Deserialize;

use crate::updater::REPO;

/// Monogram's own credits file, shipped with the font.
pub const MONOGRAM_CREDITS: &str = include_str!("../assets/fonts/monogram/credits.txt");
/// Lua's copyright and license notice (from its source).
pub const LUA_LICENSE: &str = include_str!("../assets/licenses/lua.txt");
/// The starter pack's soundfonts (downloaded, not built in; see
/// `assets/starter/README.md`).
pub const GENERALUSER_LICENSE: &str = include_str!("../assets/starter/soundfonts/GeneralUser-GS LICENSE.txt");
pub const TIMGM6MB_LICENSE: &str = include_str!("../assets/starter/soundfonts/TimGM6mb LICENSE (GPL-2.0).txt");
const THIRD_PARTY_JSON: &str = include_str!("../assets/licenses/third_party.json");

#[derive(Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub license: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub repository: Option<String>,
    /// Indices into `ThirdParty::texts`.
    texts: Vec<usize>,
}

#[derive(Deserialize)]
pub struct ThirdParty {
    pub packages: Vec<Package>,
    texts: Vec<String>,
}

impl ThirdParty {
    /// The license texts that apply to `package`.
    pub fn texts_for<'a>(&'a self, package: &'a Package) -> impl Iterator<Item = &'a str> {
        package.texts.iter().filter_map(|&i| self.texts.get(i).map(String::as_str))
    }
}

/// The embedded third-party list, parsed on first use.
pub fn third_party() -> &'static ThirdParty {
    static PARSED: OnceLock<ThirdParty> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(THIRD_PARTY_JSON).expect("embedded third_party.json is valid"))
}

#[derive(Clone, Debug, Deserialize)]
pub struct Contributor {
    pub login: String,
    pub contributions: u32,
    pub html_url: String,
}

#[derive(Clone, Debug, Default)]
pub enum ContributorsState {
    #[default]
    NotFetched,
    Fetching,
    Done(Vec<Contributor>),
    Failed(String),
}

/// The repository's contributor list, fetched from GitHub in the background.
#[derive(Clone, Default)]
pub struct Contributors {
    state: Arc<Mutex<ContributorsState>>,
}

impl Contributors {
    pub fn state(&self) -> ContributorsState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Start fetching, unless it already has (or is).
    pub fn fetch_once(&self) {
        {
            let Ok(mut state) = self.state.lock() else { return };
            if !matches!(*state, ContributorsState::NotFetched) {
                return;
            }
            *state = ContributorsState::Fetching;
        }
        let state = Arc::clone(&self.state);
        thread::spawn(move || {
            let result = fetch();
            if let Err(e) = &result {
                log::warn!("couldn't fetch contributors: {e}");
            }
            if let Ok(mut state) = state.lock() {
                *state = match result {
                    Ok(list) => ContributorsState::Done(list),
                    Err(e) => ContributorsState::Failed(e),
                };
            }
        });
    }
}

fn fetch() -> Result<Vec<Contributor>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/contributors?per_page=100");
    let mut response = ureq::get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("synththing/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| e.to_string())?;
    response.body_mut().read_json().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_license_list_parses_and_every_package_has_a_text() {
        let list = third_party();
        assert!(list.packages.len() > 100);
        for package in &list.packages {
            assert!(list.texts_for(package).next().is_some(), "{} has no license text", package.name);
        }
    }

    /// The license list has to match what's actually compiled in: if
    /// dependencies change, regenerate it.
    #[test]
    fn license_list_matches_the_dependency_tree() {
        let Ok(cargo) = std::env::var("CARGO") else { return };
        let output = std::process::Command::new(cargo)
            .args(["metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("running cargo metadata");
        assert!(output.status.success());
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let in_build: std::collections::HashSet<&str> =
            metadata["resolve"]["nodes"].as_array().unwrap().iter().filter_map(|n| n["id"].as_str()).collect();
        let mut expected: Vec<(String, String)> = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| in_build.contains(p["id"].as_str().unwrap_or_default()) && p["name"] != "synththing")
            .map(|p| (p["name"].as_str().unwrap().to_string(), p["version"].as_str().unwrap().to_string()))
            .collect();
        let mut embedded: Vec<(String, String)> =
            third_party().packages.iter().map(|p| (p.name.clone(), p.version.clone())).collect();
        expected.sort();
        embedded.sort();
        assert!(
            expected == embedded,
            "assets/licenses/third_party.json is out of date with Cargo.lock: run `cargo run --example gen_licenses`"
        );
    }
}
