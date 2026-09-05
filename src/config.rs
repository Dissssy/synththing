//! Persistent list of soundfont paths, retained between runs as JSON in the
//! platform config directory (e.g. `%APPDATA%\synththing\config\soundfonts.json`).
//! The song list is session-only and lives in the app state, not here.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub soundfonts: Vec<PathBuf>,
    /// Folder the file browser opens in. Edit this in `soundfonts.json` to
    /// point somewhere else; if unset or missing, falls back to
    /// `<Downloads>/Music`, then the home directory, then the working directory.
    #[serde(default)]
    pub browse_dir: Option<PathBuf>,
}

impl Config {
    fn file_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "synththing")
            .context("could not determine a config directory for this platform")?;
        Ok(dirs.config_dir().join("soundfonts.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::file_path()?;
        if !path.exists() {
            // Seed the file so the resolved default folder is visible for editing.
            let config = Self {
                browse_dir: default_music_dir(),
                ..Self::default()
            };
            let _ = config.save();
            return Ok(config);
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&data).unwrap_or_default())
    }

    /// Folder the file browser should open in.
    pub fn browse_start_dir(&self) -> PathBuf {
        if let Some(dir) = &self.browse_dir
            && dir.is_dir()
        {
            return dir.clone();
        }
        if let Some(dir) = default_music_dir()
            && dir.is_dir()
        {
            return dir;
        }
        if let Some(dirs) = UserDirs::new() {
            return dirs.home_dir().to_path_buf();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::file_path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Add a soundfont path if not already present. Returns `true` if it was new.
    pub fn add_soundfont(&mut self, path: &Path) -> bool {
        push_unique(&mut self.soundfonts, path)
    }

    pub fn remove_soundfont(&mut self, index: usize) {
        if index < self.soundfonts.len() {
            self.soundfonts.remove(index);
        }
    }
}

/// Append `path` (canonicalised) to `list` unless it's already there. Returns
/// `true` if it was added. Shared by the persistent soundfont list and the
/// session-only song list.
pub fn push_unique(list: &mut Vec<PathBuf>, path: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if list.contains(&path) {
        return false;
    }
    list.push(path);
    true
}

/// `<Downloads>/Music`, resolving the real Downloads folder for the platform.
fn default_music_dir() -> Option<PathBuf> {
    let dirs = UserDirs::new()?;
    let downloads = dirs
        .download_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dirs.home_dir().join("Downloads"));
    Some(downloads.join("Music"))
}

/// Display name for a soundfont/MIDI path: the file stem, or the full path if
/// there is no usable stem.
pub fn nice_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
