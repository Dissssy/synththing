//! Self-update from GitHub Releases.
//!
//! Releases are built by `.github/workflows/release.yml` whenever a version
//! tag (`vX.Y.Z`, made by `cargo release`) is pushed, and each carries the
//! Windows exe as `ASSET_NAME`. The app asks the GitHub API for the latest
//! release on a background thread, compares it with its own version, and if
//! it's newer (and the user agrees) downloads it, checks it against the
//! SHA-256 GitHub records for the asset, and swaps it in for the running
//! exe. The new version runs from the next launch (or "Restart now").
//!
//! Test builds (docs/design/release-channels.md) come from prereleases
//! that `.github/workflows/test-builds.yml` keeps up to date: `nightly`
//! (canary) and `branch-<name>`. Each is named for its build's version and
//! its notes start with `Commit: <sha>`. What a build follows is its
//! `Track`.

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
/// The release asset for this platform (named by the release workflow).
#[cfg(windows)]
pub const ASSET_NAME: &str = "synththing-windows-x86_64.exe";
/// The Linux build (experimental, see docs/LINUX.md). Releases before
/// 0.3.3 don't have one, so a Linux build finds nothing to install from
/// them rather than swapping itself for the Windows exe.
#[cfg(not(windows))]
pub const ASSET_NAME: &str = "synththing-linux-x86_64";

pub fn current_version() -> Version {
    Version::parse(crate::build_info::VERSION).expect("Cargo.toml version is valid semver")
}

/// Which builds the updater follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Track {
    /// Releases.
    Stable,
    /// Releases and the `nightly` prerelease, whichever is newer: the
    /// newest from master, and a release once that's newer.
    Canary,
    /// A branch build's own release (`branch-<name>`), followed by commit.
    Branch(String),
}

impl Track {
    /// What this build follows: a branch build, its branch; otherwise
    /// canary if the user chose it (`Config::canary_updates`).
    pub fn current(canary: bool) -> Self {
        match crate::build_info::channel() {
            crate::build_info::Channel::Branch(name) => Track::Branch(format!("branch-{name}")),
            _ if canary => Track::Canary,
            _ => Track::Stable,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    /// Its tag: `v1.2.3`, `nightly`, `branch-<name>`.
    pub tag: String,
    pub version: Version,
    /// The commit it was built from, for test builds.
    pub commit: Option<String>,
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
    /// A branch build whose release is gone: the branch was merged or
    /// deleted.
    Gone,
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

    /// Ask GitHub for the newest build on `track`, in the background.
    pub fn check(&self, track: Track) {
        if self.busy() {
            return;
        }
        self.set(UpdateState::Checking);
        let this = self.clone();
        thread::spawn(move || {
            let state = match fetch_for(&track) {
                Ok(Some(release)) if is_update(&track, &release) => {
                    log::info!("update available: v{}", release.version);
                    UpdateState::Available(release)
                }
                Ok(Some(release)) => {
                    log::info!("up to date (newest is v{})", release.version);
                    UpdateState::UpToDate
                }
                Ok(None) => {
                    log::info!("this build's release ({track:?}) is gone");
                    UpdateState::Gone
                }
                Err(e) => {
                    log::warn!("update check failed: {e:#}");
                    UpdateState::Failed(format!("Couldn't check for updates: {e:#}"))
                }
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
            log::info!("installing v{} from {}", release.version, release.asset_url);
            let state = match download_and_replace(&release, &this) {
                Ok(()) => {
                    log::info!("installed v{}, runs from the next launch", release.version);
                    UpdateState::Installed(release.version)
                }
                Err(e) => {
                    log::warn!("update install failed: {e:#}");
                    UpdateState::Failed(format!("Update failed: {e:#}"))
                }
            };
            this.set(state);
        });
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    /// Its title: a test build's version.
    #[serde(default)]
    name: Option<String>,
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
    format!("synththing/{} (+https://github.com/{REPO})", crate::build_info::VERSION)
}

/// GET `path` from the repository's API, as JSON; `None` for a 404.
fn api_get<T: serde::de::DeserializeOwned>(path: &str) -> Result<Option<T>> {
    let url = format!("https://api.github.com/repos/{REPO}/{path}");
    let response = ureq::get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", user_agent())
        .call();
    match response {
        Ok(mut response) => Ok(Some(response.body_mut().read_json().context("reading the release info")?)),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(anyhow!(e)),
    }
}

fn fetch_latest() -> Result<Release> {
    // `/releases/latest` skips drafts and prereleases already.
    let api: ApiRelease = api_get("releases/latest")?.ok_or_else(|| anyhow!("no releases published yet"))?;
    parse_release(api)
}

/// The newest build on `track`. `None`: a branch build's release is gone.
fn fetch_for(track: &Track) -> Result<Option<Release>> {
    match track {
        Track::Stable => fetch_latest().map(Some),
        Track::Canary => {
            let nightly = api_get::<ApiRelease>("releases/tags/nightly")?.map(parse_release).transpose()?;
            let latest = match fetch_latest() {
                Ok(release) => Some(release),
                Err(e) if nightly.is_some() => {
                    log::warn!("no release to compare the nightly with: {e:#}");
                    None
                }
                Err(e) => return Err(e),
            };
            Ok(newest(latest, nightly))
        }
        Track::Branch(tag) => api_get::<ApiRelease>(&format!("releases/tags/{tag}"))?.map(parse_release).transpose(),
    }
}

/// The newer of a release and a nightly. A nightly sorts below the release
/// it leads up to (`0.7.1-nightly.12 < 0.7.1`), so the release wins once
/// it's out.
fn newest(release: Option<Release>, nightly: Option<Release>) -> Option<Release> {
    match (release, nightly) {
        (Some(r), Some(n)) => Some(if n.version > r.version { n } else { r }),
        (r, n) => r.or(n),
    }
}

/// Whether `release` is an update for this build, on `track`. A branch
/// build goes by commit (a rebased branch can count fewer commits); the
/// rest by version.
fn is_update(track: &Track, release: &Release) -> bool {
    match (track, release.commit.as_deref(), crate::build_info::short_commit()) {
        (Track::Branch(_), Some(theirs), Some(ours)) => !theirs.starts_with(ours),
        _ => release.version > current_version(),
    }
}

/// Every branch's test build (the `branch-<name>` prereleases), newest
/// made first. Through every page of releases (a branch build made long ago,
/// and only updated since, is listed as old), up to `MAX_PAGES`.
pub fn branch_builds() -> Result<Vec<Release>> {
    Ok(branch_releases()?
        .into_iter()
        .filter_map(|r| {
            let tag = r.tag_name.clone();
            parse_release(r).map_err(|e| log::warn!("skipping {tag}: {e:#}")).ok()
        })
        .collect())
}

/// The channels (`branch-<name>`) GitHub has a test build for, whether
/// or not each is one this app could install.
pub fn branch_channels() -> Result<Vec<String>> {
    Ok(branch_releases()?.into_iter().map(|r| r.tag_name).collect())
}

fn branch_releases() -> Result<Vec<ApiRelease>> {
    const PER_PAGE: usize = 100;
    const MAX_PAGES: usize = 10;
    let mut all: Vec<ApiRelease> = Vec::new();
    for page in 1..=MAX_PAGES {
        let batch: Vec<ApiRelease> = api_get(&format!("releases?per_page={PER_PAGE}&page={page}"))?.unwrap_or_default();
        let last = batch.len() < PER_PAGE;
        all.extend(batch);
        if last {
            break;
        }
    }
    all.retain(|r| r.prerelease && r.tag_name.starts_with("branch-"));
    Ok(all)
}

fn parse_release(api: ApiRelease) -> Result<Release> {
    if api.draft {
        bail!("release {} is a draft", api.tag_name);
    }
    // A release is named by its tag; a test build (a prerelease) by its
    // title, its tag being its channel.
    let version_text = if api.prerelease { api.name.clone().unwrap_or_default() } else { api.tag_name.clone() };
    let version = Version::parse(version_text.trim().trim_start_matches('v'))
        .with_context(|| format!("release {} isn't named with a version ('{version_text}')", api.tag_name))?;
    let notes = api.body.unwrap_or_default();
    let commit = notes
        .lines()
        .find_map(|l| l.trim().strip_prefix("Commit:"))
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    let asset = api
        .assets
        .into_iter()
        .find(|a| a.name == ASSET_NAME)
        .ok_or_else(|| anyhow!("release {} has no {ASSET_NAME}", api.tag_name))?;
    Ok(Release {
        tag: api.tag_name,
        version,
        commit,
        notes,
        page_url: api.html_url,
        asset_url: asset.browser_download_url,
        asset_size: asset.size,
        asset_sha256: asset.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
    })
}

fn download_and_replace(release: &Release, updater: &Updater) -> Result<()> {
    let path = download(release, |downloaded| {
        updater.set(UpdateState::Downloading { release: release.clone(), downloaded });
    })?;
    let result = self_replace::self_replace(&path).context("replacing the running program");
    let _ = std::fs::remove_file(&path);
    result
}

/// Download `release`'s file for this platform to a temporary file and
/// check it (size, SHA-256), reporting the bytes so far as it goes. The
/// caller moves the file or removes it.
pub fn download(release: &Release, mut progress: impl FnMut(u64)) -> Result<PathBuf> {
    let response = ureq::get(&release.asset_url)
        .header("User-Agent", user_agent())
        .call()
        .context("downloading")?;
    // Capped at the published size (plus a little), so a misbehaving server
    // can't fill the disk.
    let mut reader = response.into_body().into_with_config().limit(release.asset_size + 1024).reader();

    let path: PathBuf = std::env::temp_dir().join(format!("synththing-update-{}-{ASSET_NAME}", release.version));
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
        progress(downloaded);
    }
    file.flush()?;
    drop(file);
    // A download isn't executable on Linux until it's marked so; swapped in
    // as it is, the next launch would fail.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making {} executable", path.display()))?;
    }

    if let Err(e) = verify(release, downloaded, &hasher.finalize()) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
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

/// The newest release, asked for now (blocking: for a server).
pub fn latest_release() -> Result<Release> {
    fetch_latest()
}

/// Download, check and swap in `release` now (blocking: for a server).
pub fn install_now(release: &Release) -> Result<()> {
    download_and_replace(release, &Updater::new())
}

/// Start the (now updated) exe again with the same arguments. The caller
/// then closes this instance.
pub fn relaunch() -> Result<()> {
    let exe = std::env::current_exe()?;
    // (It waits for this one to close: `instance.rs`.)
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(crate::instance::RELAUNCH_VAR, "1")
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(tag: &str, assets: Vec<ApiAsset>) -> ApiRelease {
        ApiRelease {
            tag_name: tag.to_string(),
            name: None,
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

    /// A test build is named for its version, and its notes say its commit.
    #[test]
    fn parses_a_test_build() {
        let mut nightly = api("nightly", vec![asset(ASSET_NAME, None)]);
        nightly.prerelease = true;
        nightly.name = Some("0.7.1-nightly.12".into());
        nightly.body = Some("Commit: 0123abcdef\n\nThe newest commit on master.".into());
        let release = parse_release(nightly).unwrap();
        assert_eq!(release.version, Version::parse("0.7.1-nightly.12").unwrap());
        assert_eq!(release.commit.as_deref(), Some("0123abcdef"));
        assert_eq!(release.tag, "nightly");
    }

    /// Canary takes the newer of the release and the nightly: the nightly
    /// until its release is out.
    #[test]
    fn canary_follows_the_newest() {
        let build = |tag: &str, version: &str| {
            let mut r = parse_release(api("v1.0.0", vec![asset(ASSET_NAME, None)])).unwrap();
            r.tag = tag.into();
            r.version = Version::parse(version).unwrap();
            r
        };
        let pick = |release: &str, nightly: &str| {
            newest(Some(build("release", release)), Some(build("nightly", nightly))).unwrap().tag
        };
        assert_eq!(pick("0.7.0", "0.7.1-nightly.3"), "nightly");
        assert_eq!(pick("0.7.1", "0.7.1-nightly.9"), "release", "the release it led up to");
        assert_eq!(pick("0.8.0", "0.7.1-nightly.9"), "release");
        assert_eq!(newest(None, Some(build("nightly", "0.7.1-nightly.1"))).unwrap().tag, "nightly");
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
