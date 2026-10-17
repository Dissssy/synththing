//! Branch builds installed alongside this one
//! (docs/design/release-channels.md).
//!
//! Each lives in a folder of its own, `<local data>/channels/branch-<name>/`
//! (`%LOCALAPPDATA%\synththing\data\channels\...` on Windows,
//! `~/.local/share/synththing/channels/...` on Linux): its exe, its config
//! folder (`config/`, a copy of the stable one made when it's installed,
//! never synced back) and `installed-by`, the exe that installed it, to go
//! back to. The stable app installs, opens and removes them; a branch build
//! only ever uses its own folder (`config::config_dir`), so trying one never
//! touches the stable install or its settings.
//!
//! With `--config-dir` (a test run), branch builds go in `channels/` inside
//! that folder instead, and are started with their own config folder in
//! `SYNTHTHING_CONFIG_DIR`: nothing outside it is touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;

use crate::build_info::{self, Channel};
use crate::updater::{self, Release};

/// What isn't copied from the stable config folder into a branch build's:
/// the library identity (branch builds are offline), logs, and downloads
/// shared through `config::shared_dir` instead.
const NOT_COPIED: &[&str] = &[
    "identity.key",
    "identity.rotations.json",
    "synththing.log",
    "synththing.prev.log",
    "ffmpeg",
    "soundfonts",
    "server",
    "library",
    "channels",
];

/// Where every branch build's folder is.
fn channels_dir() -> Result<PathBuf> {
    if let Some(dir) = crate::config::config_dir_override() {
        return Ok(dir.join("channels"));
    }
    let dirs = ProjectDirs::from("", "", "synththing").context("could not determine a data directory for this platform")?;
    Ok(dirs.data_local_dir().join("channels"))
}

/// A branch build's folder (`channel`: `branch-<name>`, its release's tag).
pub fn channel_dir(channel: &str) -> Result<PathBuf> {
    Ok(channels_dir()?.join(channel))
}

fn exe_path(channel: &str) -> Result<PathBuf> {
    Ok(channel_dir(channel)?.join(if cfg!(windows) { "synththing.exe" } else { "synththing" }))
}

/// This branch build's channel (`branch-<name>`), if it is one.
fn own_channel() -> Option<String> {
    match build_info::channel() {
        Channel::Branch(name) => Some(format!("branch-{name}")),
        _ => None,
    }
}

/// A branch build's config folder.
pub fn own_config_dir() -> Result<PathBuf> {
    let channel = own_channel().ok_or_else(|| anyhow!("not a branch build"))?;
    Ok(channel_dir(&channel)?.join("config"))
}

/// The exe that installed this branch build (the usual synththing), if
/// it's still there. (Its folder is the one its config folder is in.)
pub fn installed_by() -> Option<PathBuf> {
    own_channel()?;
    let file = crate::config::config_dir().ok()?.parent()?.join("installed-by");
    let path = PathBuf::from(fs::read_to_string(file).ok()?.trim());
    path.is_file().then_some(path)
}

/// Start a branch build's config folder `to` as a copy of `from`
/// (`NOT_COPIED` aside), so the tester's soundfonts, scripts and settings
/// come along. Says what it did.
fn copy_settings(from: &Path, to: &Path) -> Result<String> {
    fs::create_dir_all(to).with_context(|| format!("making {}", to.display()))?;
    if from.is_dir() {
        copy_dir(from, to, true)?;
        Ok(format!("copied the settings from {} to {}", from.display(), to.display()))
    } else {
        Ok(format!("no settings at {} to copy", from.display()))
    }
}

/// A branch build started without being installed by the app (its exe
/// downloaded from the release page, say), the first time: its config
/// folder starts as a copy of the stable one too. Returns what it did, for
/// the log (which isn't up yet: it lives in that folder).
pub fn prepare_own_config() -> Option<Result<String>> {
    if crate::config::config_dir_override().is_some() {
        return None;
    }
    let to = own_config_dir().ok()?;
    if to.exists() {
        return None;
    }
    Some(crate::config::stable_config_dir().and_then(|from| copy_settings(&from, &to)))
}

/// Copy the folder `from` into `to`, everything in it but (at the top,
/// `top`) `NOT_COPIED` and files set aside as unreadable.
fn copy_dir(from: &Path, to: &Path, top: bool) -> Result<()> {
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if (top && NOT_COPIED.contains(&name_text.as_ref())) || name_text.contains(".unreadable-") {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("making {}", target.display()))?;
            copy_dir(&entry.path(), &target, false)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// The branch builds installed here (their channels), by name.
pub fn installed() -> Vec<String> {
    let Ok(dir) = channels_dir() else { return Vec::new() };
    let mut channels: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|c| c.starts_with("branch-") && exe_path(c).is_ok_and(|p| p.is_file()))
        .collect();
    channels.sort();
    channels
}

/// Download `release` (a branch's test build) into its folder, replacing
/// any copy there, and remember which exe installed it. The first time, its
/// settings start as a copy of this app's.
fn install(release: &Release, progress: impl FnMut(u64)) -> Result<()> {
    let dir = channel_dir(&release.tag)?;
    fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let download = updater::download(release, progress)?;
    let exe = exe_path(&release.tag)?;
    // Moved, or copied when the temporary folder is on another drive.
    let placed = fs::rename(&download, &exe).or_else(|_| fs::copy(&download, &exe).map(|_| ()));
    let _ = fs::remove_file(&download);
    placed.with_context(|| format!("replacing {} (is that build running?)", exe.display()))?;
    if let Ok(me) = std::env::current_exe() {
        let _ = fs::write(dir.join("installed-by"), me.to_string_lossy().as_bytes());
    }
    let config = dir.join("config");
    if !config.exists() {
        let done = copy_settings(&crate::config::config_dir()?, &config)?;
        log::info!("the {} build: {done}", release.tag);
    }
    Ok(())
}

/// Start an installed branch build. In a test run (`--config-dir`), with
/// its config folder said outright, as it would otherwise use the real one.
pub fn launch(channel: &str) -> Result<()> {
    let exe = exe_path(channel)?;
    let dir = channel_dir(channel)?;
    let mut command = std::process::Command::new(&exe);
    command.current_dir(&dir);
    if crate::config::config_dir_override().is_some() {
        command.env(crate::config::CONFIG_DIR_VAR, dir.join("config"));
    } else {
        command.env_remove(crate::config::CONFIG_DIR_VAR);
    }
    command.spawn().with_context(|| format!("starting {}", exe.display()))?;
    Ok(())
}

/// Remove an installed branch build: its exe and its settings.
pub fn remove(channel: &str) -> Result<()> {
    let dir = channel_dir(channel)?;
    fs::remove_dir_all(&dir).with_context(|| format!("removing {} (is that build running?)", dir.display()))
}

/// The branch's name in a channel (`branch-youtube`: `youtube`).
pub fn branch_name(channel: &str) -> &str {
    channel.strip_prefix("branch-").unwrap_or(channel)
}

/// The list of branch builds GitHub has.
#[derive(Clone, Debug, Default)]
pub enum ListState {
    #[default]
    Idle,
    Loading,
    Loaded(Vec<Release>),
    Failed(String),
}

/// Installing one.
#[derive(Clone, Debug, Default)]
pub enum InstallState {
    #[default]
    Idle,
    Installing { channel: String, downloaded: u64, size: u64 },
    /// Installed and started.
    Done(String),
    Failed(String),
}

/// Fetches the list and installs, on background threads; the UI polls.
#[derive(Clone, Default)]
pub struct Branches {
    list: Arc<Mutex<ListState>>,
    install: Arc<Mutex<InstallState>>,
}

impl Branches {
    pub fn list(&self) -> ListState {
        self.list.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn install_state(&self) -> InstallState {
        self.install.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn busy(&self) -> bool {
        matches!(self.list(), ListState::Loading) || matches!(self.install_state(), InstallState::Installing { .. })
    }

    /// Ask GitHub for the branch builds, in the background.
    pub fn refresh(&self) {
        if matches!(self.list(), ListState::Loading) {
            return;
        }
        set(&self.list, ListState::Loading);
        let list = Arc::clone(&self.list);
        thread::spawn(move || {
            let state = match updater::branch_builds() {
                Ok(builds) => ListState::Loaded(builds),
                Err(e) => {
                    log::warn!("listing branch builds failed: {e:#}");
                    ListState::Failed(format!("Couldn't list the branch builds: {e:#}"))
                }
            };
            set(&list, state);
        });
    }

    /// Install `release` and start it, in the background.
    pub fn install(&self, release: Release) {
        if matches!(self.install_state(), InstallState::Installing { .. }) {
            return;
        }
        let channel = release.tag.clone();
        let size = release.size();
        set(&self.install, InstallState::Installing { channel: channel.clone(), downloaded: 0, size });
        let install_state = Arc::clone(&self.install);
        thread::spawn(move || {
            log::info!("installing the {channel} build (v{})", release.version);
            let progress = |downloaded| {
                set(&install_state, InstallState::Installing { channel: channel.clone(), downloaded, size });
            };
            let state = match install(&release, progress).and_then(|()| launch(&channel)) {
                Ok(()) => InstallState::Done(channel),
                Err(e) => {
                    log::warn!("installing the {channel} build failed: {e:#}");
                    InstallState::Failed(format!("Couldn't install it: {e:#}"))
                }
            };
            set(&install_state, state);
        });
    }
}

fn set<T>(slot: &Mutex<T>, value: T) {
    if let Ok(mut s) = slot.lock() {
        *s = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A branch build's copy of the settings leaves out the identity, logs,
    /// shared downloads and set-aside files, at the top only.
    #[test]
    fn copies_settings_but_not_the_rest() {
        let base = std::env::temp_dir().join(format!("synththing-channel-copy-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (from, to) = (base.join("from"), base.join("to"));
        for dir in ["visualizers", "ffmpeg", "playlists/library"] {
            fs::create_dir_all(from.join(dir)).unwrap();
        }
        for file in [
            "soundfonts.json",
            "identity.key",
            "synththing.log",
            "soundfonts.json.unreadable-1",
            "visualizers/mine.lua",
            "ffmpeg/ffmpeg.exe",
            "playlists/library/kept.json",
        ] {
            fs::write(from.join(file), "x").unwrap();
        }
        fs::create_dir_all(&to).unwrap();
        copy_dir(&from, &to, true).unwrap();

        for kept in ["soundfonts.json", "visualizers/mine.lua", "playlists/library/kept.json"] {
            assert!(to.join(kept).is_file(), "{kept} is copied");
        }
        for left in ["identity.key", "synththing.log", "soundfonts.json.unreadable-1", "ffmpeg"] {
            assert!(!to.join(left).exists(), "{left} isn't");
        }
        let _ = fs::remove_dir_all(&base);
    }

    /// End to end against GitHub: the first branch build listed installs
    /// into a test config folder's `channels/`, with a copy of its
    /// settings. (Needs the network and a branch build to exist; changes
    /// this process's config folder, so it runs on its own.)
    #[test]
    #[ignore = "downloads a branch build from GitHub"]
    fn installs_a_branch_build() {
        let base = std::env::temp_dir().join(format!("synththing-channel-install-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        crate::config::set_config_dir(&base).unwrap();
        fs::write(base.join("soundfonts.json"), r#"{ "soundfonts": ["kept.sf2"] }"#).unwrap();

        let builds = updater::branch_builds().unwrap();
        let build = builds.first().expect("no branch builds on GitHub");
        assert!(build.commit.is_some(), "its notes say its commit");
        let mut last = 0;
        install(build, |n| last = n).unwrap();
        assert_eq!(last, build.size());

        let dir = base.join("channels").join(&build.tag);
        assert!(exe_path(&build.tag).unwrap().starts_with(&dir));
        assert!(exe_path(&build.tag).unwrap().is_file());
        assert!(fs::read_to_string(dir.join("config").join("soundfonts.json")).unwrap().contains("kept.sf2"));
        assert!(!dir.join("config").join("channels").exists(), "not copied into itself");
        assert_eq!(installed(), std::slice::from_ref(&build.tag));
        remove(&build.tag).unwrap();
        assert!(installed().is_empty());
        let _ = fs::remove_dir_all(&base);
    }
}
