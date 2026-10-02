//! Self-update from GitHub Releases.
//!
//! Releases are built by `.github/workflows/release.yml` whenever a version
//! tag (`vX.Y.Z`, made by `cargo release`) is pushed, and each carries the
//! Windows exe as `ASSET_NAME`. The app asks the GitHub API for the latest
//! release on a background thread, compares it with its own version, and if
//! it's newer (and the user agrees) downloads it, checks it against the
//! SHA-256 GitHub records for the asset, and swaps it in for the running
//! exe. The new version runs from the next launch (or "Restart now").

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// `owner/name` of the GitHub repository releases come from.
pub const REPO: &str = "Dissssy/synththing";
/// The release asset holding the Windows exe (named by the release workflow).
pub const ASSET_NAME: &str = "synththing-windows-x86_64.exe";

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml version is valid semver")
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub notes: String,
    pub page_url: String,
    asset_url: String,
    asset_size: u64,
    /// Hex SHA-256 of the asset, when GitHub provides one.
    asset_sha256: Option<String>,
}

impl Release {
    pub fn size(&self) -> u64 {
        self.asset_size
    }
}

#[derive(Clone, Debug)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading { release: Release, downloaded: u64 },
    /// Installed on disk; runs from the next launch.
    Installed(Version),
    Failed(String),
}

/// Runs checks and installs on a background thread; the UI polls `state`.
#[derive(Clone)]
pub struct Updater {
    state: Arc<Mutex<UpdateState>>,
}

impl Updater {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(UpdateState::Idle)) }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().map(|s| s.clone()).unwrap_or(UpdateState::Idle)
    }

    fn set(&self, state: UpdateState) {
        if let Ok(mut s) = self.state.lock() {
            *s = state;
        }
    }

    fn busy(&self) -> bool {
        matches!(self.state(), UpdateState::Checking | UpdateState::Downloading { .. })
    }

    /// Ask GitHub for the latest release, in the background.
    pub fn check(&self) {
        if self.busy() {
            return;
        }
        self.set(UpdateState::Checking);
        let this = self.clone();
        thread::spawn(move || {
            let state = match fetch_latest() {
                Ok(release) if release.version > current_version() => UpdateState::Available(release),
                Ok(_) => UpdateState::UpToDate,
                Err(e) => UpdateState::Failed(format!("Couldn't check for updates: {e:#}")),
            };
            this.set(state);
        });
    }

    /// Download `release`, verify it, and replace the running exe with it,
    /// in the background.
    pub fn install(&self, release: Release) {
        if self.busy() {
            return;
        }
        self.set(UpdateState::Downloading { release: release.clone(), downloaded: 0 });
        let this = self.clone();
        thread::spawn(move || {
            let state = match download_and_replace(&release, &this) {
                Ok(()) => UpdateState::Installed(release.version),
                Err(e) => UpdateState::Failed(format!("Update failed: {e:#}")),
            };
            this.set(state);
        });
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// e.g. "sha256:ab12..."; absent on assets uploaded before GitHub
    /// started recording digests.
    #[serde(default)]
    digest: Option<String>,
}

fn user_agent() -> String {
    format!("synththing/{} (+https://github.com/{REPO})", env!("CARGO_PKG_VERSION"))
}

fn fetch_latest() -> Result<Release> {
    // `/releases/latest` skips drafts and prereleases already.
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut response = ureq::get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", user_agent())
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => anyhow!("no releases published yet"),
            other => anyhow!(other),
        })?;
    let api: ApiRelease = response.body_mut().read_json().context("reading the release info")?;
    parse_release(api)
}

fn parse_release(api: ApiRelease) -> Result<Release> {
    if api.draft || api.prerelease {
        bail!("latest release is a draft or prerelease");
    }
    let version = Version::parse(api.tag_name.trim_start_matches('v'))
        .with_context(|| format!("release tag '{}' isn't a version", api.tag_name))?;
    let asset = api
        .assets
        .into_iter()
        .find(|a| a.name == ASSET_NAME)
        .ok_or_else(|| anyhow!("release {} has no {ASSET_NAME}", api.tag_name))?;
    Ok(Release {
        version,
        notes: api.body.unwrap_or_default(),
        page_url: api.html_url,
        asset_url: asset.browser_download_url,
        asset_size: asset.size,
        asset_sha256: asset.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
    })
}

fn download_and_replace(release: &Release, updater: &Updater) -> Result<()> {
    let response = ureq::get(&release.asset_url)
        .header("User-Agent", user_agent())
        .call()
        .context("downloading")?;
    // Capped at the published size (plus a little), so a misbehaving server
    // can't fill the disk.
    let mut reader = response.into_body().into_with_config().limit(release.asset_size + 1024).reader();

    let path: PathBuf = std::env::temp_dir().join(format!("synththing-update-{}.exe", release.version));
    let mut file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut downloaded = 0u64;
    loop {
        let n = reader.read(&mut buf).context("downloading")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("saving the download")?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;
        updater.set(UpdateState::Downloading { release: release.clone(), downloaded });
    }
    file.flush()?;
    drop(file);

    let result = verify(release, downloaded, &hasher.finalize())
        .and_then(|()| self_replace::self_replace(&path).context("replacing the running program"));
    let _ = std::fs::remove_file(&path);
    result
}

fn verify(release: &Release, downloaded: u64, sha256: &[u8]) -> Result<()> {
    if downloaded != release.asset_size {
        bail!("download was {downloaded} bytes, expected {}", release.asset_size);
    }
    if let Some(expected) = &release.asset_sha256 {
        let actual: String = sha256.iter().map(|b| format!("{b:02x}")).collect();
        if &actual != expected {
            bail!("download doesn't match its published checksum");
        }
    }
    Ok(())
}

/// Start the (now updated) exe again with the same arguments. The caller
/// then closes this instance.
pub fn relaunch() -> Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(tag: &str, assets: Vec<ApiAsset>) -> ApiRelease {
        ApiRelease {
            tag_name: tag.to_string(),
            body: Some("notes".to_string()),
            html_url: "https://example.invalid/r".to_string(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    fn asset(name: &str, digest: Option<&str>) -> ApiAsset {
        ApiAsset {
            name: name.to_string(),
            browser_download_url: "https://example.invalid/a".to_string(),
            size: 3,
            digest: digest.map(str::to_string),
        }
    }

    #[test]
    fn parses_a_release_and_its_exe() {
        let release = parse_release(api("v1.2.3", vec![asset("other.zip", None), asset(ASSET_NAME, Some("sha256:ABCD"))])).unwrap();
        assert_eq!(release.version, Version::new(1, 2, 3));
        assert_eq!(release.asset_sha256.as_deref(), Some("abcd"));
        assert!(parse_release(api("v1.2.3", vec![asset("other.zip", None)])).is_err());
        assert!(parse_release(api("nightly", vec![asset(ASSET_NAME, None)])).is_err());
    }

    #[test]
    fn verification_checks_size_and_checksum() {
        let mut release = parse_release(api("v1.0.0", vec![asset(ASSET_NAME, None)])).unwrap();
        let digest = Sha256::digest(b"abc");
        assert!(verify(&release, 3, &digest).is_ok());
        assert!(verify(&release, 2, &digest).is_err());
        release.asset_sha256 = Some(digest.iter().map(|b| format!("{b:02x}")).collect());
        assert!(verify(&release, 3, &digest).is_ok());
        release.asset_sha256 = Some("00".repeat(32));
        assert!(verify(&release, 3, &digest).is_err());
    }
}
