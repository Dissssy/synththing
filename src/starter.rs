//! The starter pack the welcome window offers: a few songs (built in) and
//! two soundfonts (downloaded). See `assets/starter/README.md` for where
//! they come from and their licenses.
//!
//! The soundfonts are fetched from the repository at a fixed commit, so the
//! files can't change under the checksums below; moving to newer files
//! means a new commit here. A file already downloaded (right size and
//! checksum) isn't fetched again.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use directories::UserDirs;
use sha2::{Digest, Sha256};

/// `assets/starter/soundfonts/` at the commit that added them.
const BASE_URL: &str =
    "https://raw.githubusercontent.com/Dissssy/synththing/57110acf83cab319a62766fa883893f6539d0609/assets/starter/soundfonts/";

pub struct StarterSoundFont {
    pub file: &'static str,
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// The soundfonts, the one to use by default first.
pub const SOUNDFONTS: [StarterSoundFont; 2] = [
    StarterSoundFont {
        file: "GeneralUser-GS.sf2",
        name: "GeneralUser GS",
        size: 32_319_396,
        sha256: "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe",
    },
    StarterSoundFont {
        file: "TimGM6mb.sf2",
        name: "TimGM6mb",
        size: 5_994_284,
        sha256: "82475b91a76de15cb28a104707d3247ba932e228bada3f47bba63c6b31aaf7a1",
    },
];

/// The songs, built in.
pub const SONGS: [(&str, &[u8]); 4] = [
    ("Frere Jacques (round).mid", include_bytes!("../assets/starter/songs/Frere Jacques (round).mid")),
    ("Ode to Joy.mid", include_bytes!("../assets/starter/songs/Ode to Joy.mid")),
    ("Canon in D.mid", include_bytes!("../assets/starter/songs/Canon in D.mid")),
    (
        "In the Hall of the Mountain King.mid",
        include_bytes!("../assets/starter/songs/In the Hall of the Mountain King.mid"),
    ),
];

/// Refuse a download larger than expected by this much (a server sending
/// something else entirely).
const SIZE_SLACK: u64 = 1024 * 1024;

/// Everything to download, in bytes.
pub fn download_size() -> u64 {
    SOUNDFONTS.iter().map(|sf| sf.size).sum()
}

/// Where the songs go: Music\synththing\Starter songs (or under the home
/// folder), somewhere easy to find.
pub fn songs_dir() -> PathBuf {
    let dirs = UserDirs::new();
    let base = dirs
        .as_ref()
        .and_then(|d| d.audio_dir().map(Path::to_path_buf))
        .or_else(|| dirs.as_ref().map(|d| d.home_dir().to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("synththing").join("Starter songs")
}

/// Where the soundfonts go: `<config dir>/soundfonts`.
pub fn soundfonts_dir() -> Result<PathBuf> {
    Ok(crate::config::config_dir()?.join("soundfonts"))
}

/// Write the songs to `songs_dir()` (leaving any already there alone), and
/// return the folder.
pub fn write_songs() -> Result<PathBuf> {
    let dir = songs_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, bytes) in SONGS {
        let path = dir.join(name);
        if !path.exists() {
            fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(dir)
}

#[derive(Clone, Debug, Default)]
pub struct DownloadState {
    /// Soundfonts ready so far, in `SOUNDFONTS` order.
    pub ready: Vec<PathBuf>,
    pub running: bool,
    /// Bytes so far, of `download_size()`.
    pub done: u64,
    pub error: Option<String>,
}

/// Downloads the soundfonts on a background thread; the UI polls `state`.
#[derive(Clone, Default)]
pub struct Downloader {
    state: Arc<Mutex<DownloadState>>,
}

impl Downloader {
    pub fn state(&self) -> DownloadState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn update(&self, change: impl FnOnce(&mut DownloadState)) {
        if let Ok(mut state) = self.state.lock() {
            change(&mut state);
        }
    }

    /// Start (or retry) downloading whatever isn't downloaded yet.
    pub fn start(&self) {
        if self.state().running {
            return;
        }
        self.update(|s| *s = DownloadState { running: true, ..DownloadState::default() });
        let this = self.clone();
        thread::spawn(move || {
            let result = (|| -> Result<()> {
                let dir = soundfonts_dir()?;
                fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
                for soundfont in &SOUNDFONTS {
                    let path = dir.join(soundfont.file);
                    if !is_downloaded(&path, soundfont) {
                        download(&this, soundfont, &path)
                            .with_context(|| format!("downloading {}", soundfont.name))?;
                    }
                    this.update(|s| {
                        s.done = s.done.max(done_through(soundfont));
                        s.ready.push(path);
                    });
                }
                Ok(())
            })();
            if let Err(e) = &result {
                log::warn!("starter pack download failed: {e:#}");
            }
            this.update(|s| {
                s.running = false;
                s.error = result.err().map(|e| format!("{e:#}"));
            });
        });
    }
}

/// Bytes in the soundfonts up to and including `soundfont`.
fn done_through(soundfont: &StarterSoundFont) -> u64 {
    let mut total = 0;
    for sf in &SOUNDFONTS {
        total += sf.size;
        if std::ptr::eq(sf, soundfont) {
            break;
        }
    }
    total
}

fn sha256_hex(hasher: Sha256) -> String {
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Already there, whole and unchanged.
fn is_downloaded(path: &Path, soundfont: &StarterSoundFont) -> bool {
    if fs::metadata(path).map(|m| m.len()).ok() != Some(soundfont.size) {
        return false;
    }
    let Ok(mut file) = File::open(path) else { return false };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    sha256_hex(hasher) == soundfont.sha256
}

fn download(progress: &Downloader, soundfont: &StarterSoundFont, target: &Path) -> Result<()> {
    let url = format!("{BASE_URL}{}", soundfont.file);
    let response = ureq::get(&url)
        .header("User-Agent", format!("synththing/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .context("connecting")?;
    let mut reader = response.into_body().into_with_config().limit(soundfont.size + SIZE_SLACK).reader();
    let partial = target.with_extension("part");
    let mut file = File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let start = done_through(soundfont) - soundfont.size;
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).context("downloading")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("saving the download")?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress.update(|s| s.done = start + done);
    }
    file.flush()?;
    drop(file);
    let result = (|| {
        if done != soundfont.size || sha256_hex(hasher) != soundfont.sha256 {
            bail!("the download doesn't match the file it should be");
        }
        fs::rename(&partial, target).with_context(|| format!("saving {}", target.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result.map_err(|e| anyhow!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checksums here are the files in the repository (which the
    /// pinned URL serves).
    #[test]
    fn checksums_match_the_repository_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/starter/soundfonts");
        for soundfont in &SOUNDFONTS {
            let path = dir.join(soundfont.file);
            assert!(is_downloaded(&path, soundfont), "{} doesn't match its size or checksum", soundfont.file);
            let other = &SOUNDFONTS[(SOUNDFONTS.iter().position(|s| s.file == soundfont.file).unwrap() + 1) % 2];
            assert!(!is_downloaded(&path, other));
        }
        assert_eq!(download_size(), 32_319_396 + 5_994_284);
        assert_eq!(done_through(&SOUNDFONTS[1]), download_size());
    }

    #[test]
    fn the_songs_are_playable_midi() {
        for (name, bytes) in SONGS {
            let notes = crate::midi_notes::NoteList::from_smf(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(notes.note_count() > 300, "{name}");
            assert!(notes.channels().len() >= 4, "{name}: {:?}", notes.channels());
        }
    }
}
