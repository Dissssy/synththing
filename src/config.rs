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
use crate::playlist::LoopMode;

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
    /// The dock layout, which sections are open and how they're arranged.
    /// Remembered so the app reopens the way you left it; `None` (first
    /// launch) means `layout::default_layout()`.
    #[serde(default)]
    pub layout: Option<DockState<Section>>,
    /// Preference: fullscreen keeps its own layout. Off (the default), going
    /// fullscreen shows and edits the same layout as the window.
    #[serde(default)]
    pub separate_fullscreen_layout: bool,
    /// Fullscreen's own layout, only used while `separate_fullscreen_layout`
    /// is on. Kept when that's turned off, so turning it back on brings the
    /// same arrangement back. `None` means `layout::fullscreen_default()`.
    #[serde(default)]
    pub fullscreen_layout: Option<DockState<Section>>,
    /// Layouts saved under a name (View > Layout). They keep changes made
    /// while they're active.
    #[serde(default)]
    pub layout_presets: Vec<crate::layout::SavedLayout>,
    /// Built-in layouts as the user has changed them, by name (removed by
    /// Reset, or when changed back to the original).
    #[serde(default)]
    pub layout_overrides: std::collections::BTreeMap<String, DockState<Section>>,
    /// The layout (built-in or saved, by name) the window and fullscreen
    /// are showing, which changes are saved into. `None`: an arrangement of
    /// the user's own that isn't a named layout.
    #[serde(default)]
    pub active_layout: Option<String>,
    #[serde(default)]
    pub fullscreen_active_layout: Option<String>,
    /// Preference: preloaded files nothing has needed for this many seconds
    /// are unloaded. `None` means `DEFAULT_PRELOAD_EXPIRY_SECS`.
    #[serde(default)]
    pub preload_expiry_secs: Option<u32>,
    /// The version that last ran, for the What's new window after an
    /// update (`None`: a config from before it was recorded).
    #[serde(default)]
    pub last_version: Option<String>,
    /// Preference: ask before playing a very large MIDI file (`None`: yes).
    #[serde(default)]
    pub warn_heavy_midi: Option<bool>,
    /// Preference: list audio files (MP3, WAV, ...) as songs, not only MIDI.
    #[serde(default)]
    pub show_audio_files: bool,
    /// Preference: copy visualizer scripts' log() messages to the app's log.
    #[serde(default)]
    pub script_log_to_app: bool,
    /// Experimental preference: start loading a song when the pointer rests
    /// on it, so clicking it plays sooner.
    #[serde(default)]
    pub preload_on_hover: bool,
    /// Preference: look for a new release on GitHub at startup. `None`
    /// means yes.
    #[serde(default)]
    pub check_updates_on_launch: Option<bool>,
    /// A release version the user chose to skip: the startup check doesn't
    /// bring it up again (a manual check still shows it).
    #[serde(default)]
    pub skipped_update: Option<String>,
    /// The Loop and Shuffle buttons, remembered between launches.
    #[serde(default)]
    pub loop_mode: LoopMode,
    #[serde(default)]
    pub shuffle: bool,
    /// Recording size and frame rate (`None`: the default, 60).
    #[serde(default)]
    pub record_resolution: crate::recorder::Resolution,
    #[serde(default)]
    pub record_fps: Option<u32>,
    #[serde(default)]
    pub record_quality: crate::recorder::Quality,
    /// Scripts (by file name) recorded in Manual the last time, where they
    /// could play themselves (Auto, the default otherwise).
    #[serde(default)]
    pub record_manual: Vec<String>,
    /// The script editor: when edits apply, text size, wrapping (`None`:
    /// the defaults).
    #[serde(default)]
    pub editor_apply: crate::app::ApplyMode,
    #[serde(default)]
    pub editor_font_size: Option<f32>,
    #[serde(default)]
    pub editor_wrap: Option<bool>,
    /// The theme in use, by name (`None`: the default, Dark).
    #[serde(default)]
    pub theme: Option<String>,
    /// How big the UI is drawn (`None`: 100%).
    #[serde(default)]
    pub ui_scale: Option<f32>,
    /// Where recordings are saved; `None` means Videos\synththing.
    #[serde(default)]
    pub record_dir: Option<PathBuf>,
    /// The script library: which servers, their keys, the name to post
    /// under.
    #[serde(default)]
    pub library: LibrarySettings,
}

/// The script library's servers (Preferences > Library).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrarySettings {
    /// The official server is used (it can't be removed, only turned off).
    pub official: bool,
    /// Other servers, by address.
    pub servers: Vec<LibraryServer>,
    /// Each server's key (hex) as first seen, by address: a server whose
    /// key changes isn't trusted until the user says so.
    pub pinned: std::collections::BTreeMap<String, String>,
    /// The name last posted under.
    pub author_name: String,
}

impl Default for LibrarySettings {
    fn default() -> Self {
        Self { official: true, servers: Vec::new(), pinned: Default::default(), author_name: String::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LibraryServer {
    pub url: String,
    pub enabled: bool,
}

impl LibrarySettings {
    /// The servers in use, the official one first.
    pub fn enabled_servers(&self) -> Vec<String> {
        let official = self.official.then(|| crate::library::OFFICIAL_URL.to_string());
        official.into_iter().chain(self.servers.iter().filter(|s| s.enabled).map(|s| s.url.clone())).collect()
    }
}

pub const DEFAULT_PRELOAD_EXPIRY_SECS: u32 = 30;
pub const PRELOAD_EXPIRY_RANGE: std::ops::RangeInclusive<u32> = 5..=300;

impl Config {
    fn file_path() -> Result<PathBuf> {
        Ok(config_dir()?.join("soundfonts.json"))
    }

    /// Whether there's a saved config (synththing has run here before).
    pub fn exists() -> bool {
        Self::file_path().is_ok_and(|path| path.exists())
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

    pub fn preload_expiry(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.preload_expiry_secs.unwrap_or(DEFAULT_PRELOAD_EXPIRY_SECS)))
    }

    /// Where recordings are saved.
    pub fn recordings_dir(&self) -> PathBuf {
        self.record_dir.clone().unwrap_or_else(default_recordings_dir)
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

/// Where recordings go when nothing's chosen: Videos\synththing, or under
/// the home folder.
pub fn default_recordings_dir() -> PathBuf {
    let dirs = UserDirs::new();
    let base = dirs
        .as_ref()
        .and_then(|d| d.video_dir().map(Path::to_path_buf))
        .or_else(|| dirs.as_ref().map(|d| d.home_dir().to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("synththing")
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
