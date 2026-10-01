//! Persistent list of soundfont paths, retained between runs as JSON in the
//! platform config directory (e.g. `%APPDATA%\synththing\config\soundfonts.json`).
//! The song list is session-only and lives in the app state, not here.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use egui_dock::DockState;
use serde::{Deserialize, Serialize};

use crate::layout::Section;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub soundfonts: Vec<PathBuf>,
    /// Folder the file browser opens in, set via the "Set as default" button
    /// in the Songs panel (or by editing `soundfonts.json` directly). If
    /// unset or no longer a directory, falls back to the home directory, then
    /// the working directory.
    #[serde(default)]
    pub browse_dir: Option<PathBuf>,
    /// The dock layout — which sections are open and how they're arranged.
    /// Remembered so the app reopens the way you left it; `None` (first
    /// launch) means `layout::default_layout()`.
    #[serde(default)]
    pub layout: Option<DockState<Section>>,
}

impl Config {
    fn file_path() -> Result<PathBuf> {
        Ok(config_dir()?.join("soundfonts.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::file_path()?;
        if !path.exists() {
            return Ok(Self::default());
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
        if let Some(dirs) = UserDirs::new() {
            return dirs.home_dir().to_path_buf();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    /// Persist `dir` as the folder the song browser opens in from now on.
    pub fn set_browse_dir(&mut self, dir: PathBuf) -> Result<()> {
        self.browse_dir = Some(dir);
        self.save()
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

/// The platform config directory for synththing (e.g. `%APPDATA%\synththing\config`),
/// shared by the soundfont list and the Lua visualizer script folder.
pub fn config_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "synththing")
        .context("could not determine a config directory for this platform")?;
    Ok(dirs.config_dir().to_path_buf())
}

/// Where saved playlists live, one JSON file each.
pub fn playlists_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("playlists"))
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

/// Display name for a soundfont/MIDI path: the file stem, or the full path if
/// there is no usable stem.
pub fn nice_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
