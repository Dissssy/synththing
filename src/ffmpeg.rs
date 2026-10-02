//! Finding (and, on Windows, fetching) ffmpeg, which encodes recordings
//! (`recorder.rs`).
//!
//! synththing doesn't ship ffmpeg. It uses one it downloaded earlier into
//! its config folder, or else one on the PATH. On Windows, the first time
//! you record without either, it offers to download gyan.dev's "release
//! essentials" build: the zip is checked against the SHA-256 published next
//! to it, and only `ffmpeg.exe` is kept.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

/// gyan.dev's current release build, and its checksum (both redirect to
/// the versioned files).
const DOWNLOAD_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
const CHECKSUM_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip.sha256";
/// Refuse downloads past this, so a misbehaving server can't fill the disk.
const MAX_DOWNLOAD_BYTES: u64 = 400 * 1024 * 1024;

#[cfg(windows)]
const EXE_NAME: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "ffmpeg";

/// Whether this platform gets the download offer (elsewhere: install
/// ffmpeg with the system's package manager).
pub const CAN_DOWNLOAD: bool = cfg!(windows);

/// Where a downloaded ffmpeg lives: `<config dir>/ffmpeg/ffmpeg.exe`.
pub fn managed_path() -> Option<PathBuf> {
    crate::config::config_dir().ok().map(|dir| dir.join("ffmpeg").join(EXE_NAME))
}

/// The ffmpeg to use: the downloaded one, else one on the PATH that runs.
pub fn find() -> Option<PathBuf> {
    if let Some(path) = managed_path()
        && path.is_file()
    {
        return Some(path);
    }
    let on_path = PathBuf::from(EXE_NAME);
    runs(&on_path).then_some(on_path)
}

fn runs(exe: &Path) -> bool {
    command(exe)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A `Command` for ffmpeg that doesn't flash a console window on Windows.
pub fn command(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[derive(Clone, Debug)]
pub enum DownloadState {
    Idle,
    /// Bytes so far, and the total when the server said.
    Downloading { done: u64, total: Option<u64> },
    Done(PathBuf),
    Failed(String),
}

/// Downloads ffmpeg on a background thread; the UI polls `state`.
#[derive(Clone)]
pub struct Downloader {
    state: Arc<Mutex<DownloadState>>,
}

impl Downloader {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(DownloadState::Idle)) }
    }

    pub fn state(&self) -> DownloadState {
        self.state.lock().map(|s| s.clone()).unwrap_or(DownloadState::Idle)
    }

    fn set(&self, state: DownloadState) {
        if let Ok(mut s) = self.state.lock() {
            *s = state;
        }
    }

    pub fn start(&self) {
        if matches!(self.state(), DownloadState::Downloading { .. }) {
            return;
        }
        self.set(DownloadState::Downloading { done: 0, total: None });
        let this = self.clone();
        thread::spawn(move || {
            let state = match download(&this) {
                Ok(path) => {
                    log::info!("ffmpeg downloaded to {}", path.display());
                    DownloadState::Done(path)
                }
                Err(e) => {
                    log::warn!("ffmpeg download failed: {e:#}");
                    DownloadState::Failed(format!("{e:#}"))
                }
            };
            this.set(state);
        });
    }
}

fn user_agent() -> String {
    format!("synththing/{}", env!("CARGO_PKG_VERSION"))
}

fn download(progress: &Downloader) -> Result<PathBuf> {
    let target = managed_path().ok_or_else(|| anyhow!("no config folder to keep ffmpeg in"))?;
    let expected = ureq::get(CHECKSUM_URL)
        .header("User-Agent", user_agent())
        .call()
        .context("fetching the checksum")?
        .into_body()
        .read_to_string()
        .context("reading the checksum")?;
    let expected = expected.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
    if expected.len() != 64 {
        bail!("the published checksum doesn't look like a SHA-256");
    }

    let response = ureq::get(DOWNLOAD_URL).header("User-Agent", user_agent()).call().context("downloading")?;
    let total = response.body().content_length();
    let mut reader = response.into_body().into_with_config().limit(MAX_DOWNLOAD_BYTES).reader();
    let zip_path = std::env::temp_dir().join("synththing-ffmpeg-download.zip");
    let mut file = File::create(&zip_path).with_context(|| format!("creating {}", zip_path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).context("downloading")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("saving the download")?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress.set(DownloadState::Downloading { done, total });
    }
    file.flush()?;
    drop(file);

    let result = (|| {
        let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if actual != expected {
            bail!("the download doesn't match its published checksum");
        }
        extract_exe(&zip_path, &target)?;
        if !runs(&target) {
            bail!("the downloaded ffmpeg doesn't run");
        }
        Ok(target)
    })();
    let _ = std::fs::remove_file(&zip_path);
    result
}

/// Pull `bin/ffmpeg.exe` (inside the zip's one top folder) out to `target`.
fn extract_exe(zip_path: &Path, target: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(zip_path)?).context("opening the zip")?;
    let wanted = format!("bin/{EXE_NAME}");
    let index = (0..archive.len())
        .find(|&i| archive.name_for_index(i).is_some_and(|name| name.ends_with(&wanted)))
        .ok_or_else(|| anyhow!("no {wanted} in the download"))?;
    let mut entry = archive.by_index(index)?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let partial = target.with_extension("part");
    let mut out = File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
    std::io::copy(&mut entry, &mut out).context("unpacking ffmpeg")?;
    out.flush()?;
    drop(out);
    std::fs::rename(&partial, target).with_context(|| format!("saving {}", target.display()))?;
    Ok(())
}
