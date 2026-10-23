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

/// The beat pulse's strength and length (`Config::beat_pulse`,
/// `beat_pulse_length`) unless they're changed.
pub const DEFAULT_BEAT_PULSE: f32 = 0.3;
pub const DEFAULT_BEAT_PULSE_LENGTH: f32 = 0.35;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub soundfonts: Vec<PathBuf>,
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
    /// Preference: how far toward white the seek bar flashes on each beat,
    /// 0 (not at all) to 1 (white) (`None`: `DEFAULT_BEAT_PULSE`).
    #[serde(default)]
    pub beat_pulse: Option<f32>,
    /// Preference: how long each beat's flash takes to fade, as a share of
    /// the beat, 0.1 to 1 (`None`: `DEFAULT_BEAT_PULSE_LENGTH`).
    #[serde(default)]
    pub beat_pulse_length: Option<f32>,
    /// Preference: closing the window hides it to the tray (`Some(true)`)
    /// or quits (`Some(false)`); `None` until the first close asks.
    #[serde(default)]
    pub close_to_tray: Option<bool>,
    /// Keys given to commands in place of their defaults, by command id
    /// (`commands::Keymap`), synced.
    #[serde(default)]
    pub keymap: crate::commands::Bindings,
    /// This device's own keys for commands, over `keymap`'s.
    #[serde(default)]
    pub local_keymap: crate::commands::Bindings,
    /// The local app API (`api.rs`): on or off, its port and the programs
    /// allowed in. Kept on this device.
    #[serde(default)]
    pub api: crate::api::Settings,
    /// Preference: which of the last session's settings to start with.
    #[serde(default)]
    pub remember: Remember,
    /// The last session's settings, saved when the app closes (each kept
    /// only while `remember` says so).
    #[serde(default)]
    pub session: Session,
    /// Preference: how much audio is made ahead of what's heard, in
    /// milliseconds (`None`: `audio::DEFAULT_BUFFER_MS`).
    #[serde(default)]
    pub buffer_ms: Option<u32>,
    /// Preference: show the channel toggles as colored chips with each
    /// channel's instrument, instead of numbered checkboxes.
    #[serde(default)]
    pub channel_chips: bool,
    /// The Songs list's columns, sorting and grouping.
    #[serde(default)]
    pub songs_view: crate::song_list::View,
    /// Preference: how songs are named from their tags, the first that
    /// works for a song (`song_title`; `None`: `song_title::DEFAULT_PATTERNS`).
    #[serde(default)]
    pub title_patterns: Option<Vec<String>>,
    /// Preference: characters grouping by a name (the Songs list's groups,
    /// rulesets' playlists) sees past, beside case, spacing, punctuation
    /// and symbols (`fields::group_key`).
    #[serde(default)]
    pub group_ignore: String,
    /// Preference: sort names with a leading "The", "A" or "An" as they
    /// are (`fields::sort_text`); by default it's passed over.
    #[serde(default)]
    pub sort_with_articles: bool,
    /// Clean-up suggestions dismissed (`cleanup::Suggestion::id`).
    #[serde(default)]
    pub cleanup_dismissed: std::collections::BTreeSet<String>,
    /// Preference: look albums up at MusicBrainz for their art and
    /// details (`lookup`).
    #[serde(default)]
    pub online_lookup: bool,
    /// The one device that looks them up (`device::Device::id`), so they're
    /// looked up once for every device.
    #[serde(default)]
    pub lookup_device: Option<String>,
    /// Preference: copy visualizer scripts' log() messages to the app's log.
    #[serde(default)]
    pub script_log_to_app: bool,
    /// See-through visualizers show what's behind them in the app (the
    /// visualizer's See-through button); off, they're shown over black.
    /// What goes out through the local API keeps its transparency either way.
    #[serde(default)]
    pub visualizer_see_through: bool,
    /// Experimental preference: start loading a song when the pointer rests
    /// on it, so clicking it plays sooner.
    #[serde(default)]
    pub preload_on_hover: bool,
    /// Preference: look for a new release on GitHub at startup. `None`
    /// means yes.
    #[serde(default)]
    pub check_updates_on_launch: Option<bool>,
    /// Preference: update to canary builds (the newest from master, and
    /// releases once they're newer) rather than releases only. `None`
    /// means yes in a canary build, no otherwise (`canary_updates()`). A
    /// bool, not an enum of channels: a build that didn't know a newer
    /// channel's name couldn't read the whole config (`read_json_or_set_aside`).
    #[serde(default)]
    pub canary_updates: Option<bool>,
    /// On Android: download and install updates in the app (it needs
    /// Install unknown apps allowed). Off, a new version is only announced.
    #[serde(default)]
    pub android_install_updates: bool,
    /// The mobile layout (`app/mobile.rs`) instead of the desktop's, on the
    /// desktop (View > Mobile layout). Android always uses it.
    #[serde(default)]
    pub mobile_layout: bool,
    /// On Android: the visualizer filling the screen draws under a notch
    /// or camera hole too (off: that strip is black).
    #[serde(default)]
    pub fullscreen_over_cutout: bool,
    /// On Android: leaving the app while a song plays shrinks it into a
    /// floating window of the visualizer, the mini player (`None`: on).
    #[serde(default)]
    pub mini_player: Option<bool>,
    /// That window's shape.
    #[serde(default)]
    pub mini_player_shape: MiniPlayerShape,
    /// A release version the user chose to skip: the startup check doesn't
    /// bring it up again (a manual check still shows it).
    #[serde(default)]
    pub skipped_update: Option<String>,
    /// Repair library has been suggested, after a song in the library
    /// couldn't be read (it's only suggested once).
    #[serde(default)]
    pub repair_offered: bool,
    /// The Loop and Shuffle buttons, remembered between launches.
    #[serde(default)]
    pub loop_mode: LoopMode,
    #[serde(default)]
    pub shuffle: bool,
    /// The Songs list's Autoplay button, remembered with Loop and Shuffle.
    #[serde(default)]
    pub autoplay: crate::song_list::Autoplay,
    /// Preference: autoplay leaves out the songs in closed groups.
    #[serde(default)]
    pub autoplay_skip_collapsed: bool,
    /// Preference: shuffle, autoplay and playlists playing on play only
    /// starred songs (one picked by hand still plays).
    #[serde(default)]
    pub only_starred: bool,
    /// Preference: where songs dropped on the window go, after the library.
    #[serde(default)]
    pub dropped_songs: DroppedSongs,
    /// Preference: how many letters (1 to 4) a tile without art shows of
    /// its name (`album_art`; `None`: 2).
    #[serde(default)]
    pub tile_letters: Option<u8>,
    /// The user's own rulesets (`smart.rs`), by ID, beside the built-in
    /// ones (`smart::builtin`).
    #[serde(default)]
    pub rulesets: std::collections::BTreeMap<String, crate::smart::Ruleset>,
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
    /// Your own themes, by name (the built-in ones are
    /// `theme::builtins()`). In the config so settings sync carries them.
    #[serde(default)]
    pub custom_themes: std::collections::BTreeMap<String, crate::theme::Theme>,
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
    /// Show what each settings sync changed (a debug toast).
    #[serde(default)]
    pub sync_debug: bool,
}

/// Which of the last session's settings the app starts with (Preferences >
/// Playback): everything but the speed unless changed.
/// The shape of Android's mini player (picture-in-picture): Android sizes
/// it, and lets the user resize it, in this shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MiniPlayerShape {
    #[default]
    Square,
    Wide,
    Tall,
}

impl MiniPlayerShape {
    pub const ALL: [Self; 3] = [Self::Square, Self::Wide, Self::Tall];

    pub fn name(self) -> &'static str {
        match self {
            Self::Square => "Square",
            Self::Wide => "Wide (16:9)",
            Self::Tall => "Tall (9:16)",
        }
    }
}

/// Where songs dropped on the window go once they're in the library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DroppedSongs {
    /// Only the library (and the playlist on screen, if one is).
    #[default]
    Import,
    /// The end of the queue.
    AddToQueue,
    /// The front of the queue.
    PlayNext,
}

impl DroppedSongs {
    pub const ALL: [Self; 3] = [Self::Import, Self::AddToQueue, Self::PlayNext];

    pub fn name(self) -> &'static str {
        match self {
            Self::Import => "Just import",
            Self::AddToQueue => "Add to the queue",
            Self::PlayNext => "Play next",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remember {
    pub volume: bool,
    pub soundfont: bool,
    pub script: bool,
    pub speed: bool,
    /// Loop, shuffle and autoplay.
    pub modes: bool,
}

impl Default for Remember {
    fn default() -> Self {
        Self { volume: true, soundfont: true, script: true, speed: false, modes: true }
    }
}

/// What the last session ended with (`Config::session`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub volume: Option<f32>,
    pub speed: Option<f64>,
    /// The selected soundfont (not a song's own).
    pub soundfont: Option<PathBuf>,
    /// The visualizer script that was showing.
    pub script: Option<PathBuf>,
}

/// The script library's servers (Preferences > Online Library).
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
    /// The official publisher's keys, oldest first, as the official server
    /// last said (empty: never rotated).
    pub official_publishers: Vec<String>,
    /// Keep in touch with the servers in use for notifications (while
    /// there's an identity): a check at launch, and a stream while open.
    pub notifications: bool,
}

impl Default for LibrarySettings {
    fn default() -> Self {
        Self {
            official: true,
            servers: Vec::new(),
            pinned: Default::default(),
            author_name: String::new(),
            official_publishers: Vec::new(),
            notifications: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LibraryServer {
    pub url: String,
    pub enabled: bool,
}

impl LibrarySettings {
    /// The servers in use, the official one first. None while the Online
    /// Library is off (a branch build's, `build_info::online`).
    pub fn enabled_servers(&self) -> Vec<String> {
        if !crate::build_info::online(crate::build_info::Online::Scripts) {
            return Vec::new();
        }
        let official = self.official.then(crate::library::official_url);
        let mut servers: Vec<String> = Vec::new();
        for url in official.into_iter().chain(self.servers.iter().filter(|s| s.enabled).map(|s| s.url.clone())) {
            if !servers.contains(&url) {
                servers.push(url);
            }
        }
        servers
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

    /// The saved config, or the defaults if there's none or it can't be
    /// read. Changes nothing on disk: for commands that never save it. The
    /// app itself loads with `load_for_app`.
    pub fn load() -> Result<Self> {
        let path = Self::file_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&data).unwrap_or_default())
    }

    /// The saved config, for the app, which saves it as it goes: one it
    /// can't read is moved aside first (`read_json_or_set_aside`), so the
    /// defaults it starts from aren't saved over it. What could be read of
    /// it is saved in its place straight away: otherwise, if nothing changes
    /// before the app is next started (Android has no closing to save on),
    /// it would start without any of it.
    pub fn load_for_app() -> Result<Self> {
        let path = Self::file_path()?;
        let config: Option<Self> = read_json_or_set_aside(&path)?;
        if let Some(config) = &config
            && !path.exists()
        {
            config.save()?;
        }
        Ok(config.unwrap_or_default())
    }

    /// Whether to update to canary builds: as chosen, or by default if this
    /// build is one. (Android's APK is in canary builds before any release
    /// has one, so a fresh install has something to update to.)
    pub fn canary_updates(&self) -> bool {
        self.canary_updates.unwrap_or(crate::build_info::channel() == crate::build_info::Channel::Nightly)
    }

    pub fn preload_expiry(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.preload_expiry_secs.unwrap_or(DEFAULT_PRELOAD_EXPIRY_SECS)))
    }

    /// Where recordings are saved.
    pub fn recordings_dir(&self) -> PathBuf {
        self.record_dir.clone().unwrap_or_else(default_recordings_dir)
    }

    /// The Songs folder from before the song library (`browse_dir`, which
    /// isn't a setting any more), for the one-time import: read from the
    /// file, since the config doesn't keep it.
    pub fn old_songs_folder() -> Option<PathBuf> {
        let text = fs::read_to_string(Self::file_path().ok()?).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("browse_dir")?.as_str().map(PathBuf::from)
    }

    /// The config as JSON that reads back (`finite_positions`).
    pub fn to_json(&self) -> Result<serde_json::Value> {
        let mut json = serde_json::to_value(self)?;
        finite_positions(&mut json);
        Ok(json)
    }

    pub fn save(&self) -> Result<()> {
        let _timing = crate::frame_timing::scope("config save");
        let path = Self::file_path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = self.to_json()?;
        crate::settings_sync::record(&json);
        let json = serde_json::to_string_pretty(&json)?;
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

/// A config folder of its own (`--config-dir`, or `SYNTHTHING_CONFIG_DIR`),
/// for test runs that mustn't touch the real one.
static CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The variable a config folder of its own is passed on in (to the
/// processes the app starts, previews say).
pub const CONFIG_DIR_VAR: &str = "SYNTHTHING_CONFIG_DIR";

/// Use `dir` as the config folder from now on (made if it isn't there).
pub fn set_config_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let dir = std::path::absolute(dir).with_context(|| format!("finding {}", dir.display()))?;
    let _ = CONFIG_DIR.set(dir);
    Ok(())
}

/// The config folder given instead of the usual one, if one was.
pub fn config_dir_override() -> Option<PathBuf> {
    CONFIG_DIR
        .get()
        .cloned()
        .or_else(|| std::env::var_os(CONFIG_DIR_VAR).filter(|v| !v.is_empty()).map(PathBuf::from))
}

/// The config directory for synththing (e.g. `%APPDATA%\synththing\config`),
/// where everything it keeps lives: preferences, scripts, soundfonts,
/// identity. `--config-dir` (or `SYNTHTHING_CONFIG_DIR`) puts it elsewhere.
/// A branch build has its own, in its channel's folder (`channels.rs`),
/// however it's started: it never touches the stable one.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = config_dir_override() {
        return Ok(dir);
    }
    if let crate::build_info::Channel::Branch(_) = crate::build_info::channel() {
        return crate::channels::own_config_dir();
    }
    stable_config_dir()
}

/// The usual config directory, the one releases and canary builds use.
pub fn stable_config_dir() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "synththing")
        .context("could not determine a config directory for this platform")?;
    Ok(dirs.config_dir().to_path_buf())
}

/// The song library of the mobile layout (`import.rs`): what's been
/// imported, in the config folder.
pub fn music_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("music"))
}

/// Where downloads with no format of synththing's own go (ffmpeg, the
/// starter soundfonts), so a branch build shares them with the stable one
/// instead of downloading its own: the stable config directory, or
/// `--config-dir`'s.
pub fn shared_dir() -> Result<PathBuf> {
    match config_dir_override() {
        Some(dir) => Ok(dir),
        None => stable_config_dir(),
    }
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

/// A JSON file synththing keeps, as `T`: `None` if it isn't there.
///
/// One that can't be read as `T` (written by a build that stores something
/// differently, a branch build or a newer one, say) is first moved aside,
/// to `<name>.unreadable-<unix time>` next to it, so what's saved next
/// doesn't overwrite it: it can be recovered, or read again by the build
/// that wrote it. What can still be read of it is used (`salvage`: every
/// top-level field that's fine on its own, so a layout with a tab this
/// build doesn't have costs the layout, not every setting); `None` if
/// nothing can. Either way the app says so (`set_aside_notices`).
/// Errors only when it can't be read at all, or can't be moved aside.
pub fn read_json_or_set_aside<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let error = match serde_json::from_str(&data) {
        Ok(value) => return Ok(Some(value)),
        Err(e) => e,
    };
    let aside = set_aside(path).with_context(|| format!("{} can't be read ({error})", path.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let salvaged = salvage::<T>(&data);
    let note = match &salvaged {
        Some((_, dropped)) => format!(
            "Part of {name} couldn't be read ({}): it was most likely saved by a different build of \
             synththing, or changed by hand. The rest was kept. The whole file was saved as {}.",
            dropped.join(", "),
            aside.display()
        ),
        None => format!(
            "{name} couldn't be read: it was most likely saved by a different build of synththing, or changed \
             by hand. synththing started without it, and saved it as {}.",
            aside.display()
        ),
    };
    log::warn!("{note} ({error})");
    if let Ok(mut notices) = SET_ASIDE.lock() {
        notices.push((note, aside));
    }
    Ok(salvaged.map(|(value, _)| value))
}

/// What `read_json_or_set_aside` set aside this run, to tell the user.
static SET_ASIDE: std::sync::Mutex<Vec<(String, PathBuf)>> = std::sync::Mutex::new(Vec::new());

/// Settings files set aside this run (`read_json_or_set_aside`): what
/// happened, in a sentence or two, and where the file went. Taken: each is
/// said once.
pub fn set_aside_notices() -> Vec<(String, PathBuf)> {
    SET_ASIDE.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default()
}

/// What can be read of `data` as `T`, a struct whose fields all have
/// defaults: every top-level field that reads on its own, the rest left at
/// their defaults. Returns it with the fields left out, or `None` if it
/// isn't a JSON object or still doesn't read.
fn salvage<T: serde::de::DeserializeOwned>(data: &str) -> Option<(T, Vec<String>)> {
    let serde_json::Value::Object(fields) = serde_json::from_str(data).ok()? else { return None };
    let mut kept = serde_json::Map::new();
    let mut dropped = Vec::new();
    for (key, value) in fields {
        let alone = serde_json::Value::Object([(key.clone(), value.clone())].into_iter().collect());
        if serde_json::from_value::<T>(alone).is_ok() {
            kept.insert(key, value);
        } else {
            dropped.push(key);
        }
    }
    let value = serde_json::from_value(serde_json::Value::Object(kept)).ok()?;
    Some((value, dropped))
}

/// Make every position (`{"x": .., "y": ..}`) in `json` finite. A rect egui
/// hasn't laid out yet (a dock layout that's never been drawn, as on Android
/// and in the mobile layout) is infinite, which JSON can't hold: serde_json
/// writes `null`, then can't read it back. They're laid out again before
/// they're used, so 0 does.
fn finite_positions(json: &mut serde_json::Value) {
    match json {
        serde_json::Value::Object(fields) if fields.len() == 2 && fields.contains_key("x") && fields.contains_key("y") => {
            for value in fields.values_mut().filter(|v| v.is_null()) {
                *value = 0.0.into();
            }
        }
        serde_json::Value::Object(fields) => fields.values_mut().for_each(finite_positions),
        serde_json::Value::Array(items) => items.iter_mut().for_each(finite_positions),
        _ => {}
    }
}

/// Move a file synththing can't read out of the way, to
/// `<name>.unreadable-<unix time>` next to it, before something new is
/// saved in its place. Returns where it went.
pub fn set_aside(path: &Path) -> Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".unreadable-{secs}"));
    let aside = path.with_file_name(name);
    fs::rename(path, &aside).with_context(|| format!("moving {} aside", path.display()))?;
    Ok(aside)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A config from before `remember` existed starts remembering everything
    /// but the speed, with nothing from a last session.
    #[test]
    fn older_configs_remember_by_default() {
        let config: Config = serde_json::from_str(r#"{ "soundfonts": [] }"#).unwrap();
        assert_eq!(config.remember, Remember { volume: true, soundfont: true, script: true, speed: false, modes: true });
        assert!(config.session.volume.is_none() && config.session.script.is_none());
        let partial: Config = serde_json::from_str(r#"{ "remember": { "speed": true } }"#).unwrap();
        assert!(partial.remember.speed && partial.remember.volume, "left out: the default");
    }

    /// A file that can't be read is moved aside rather than left to be
    /// saved over; a readable or missing one is left alone.
    #[test]
    fn unreadable_files_are_moved_aside() {
        let dir = std::env::temp_dir().join(format!("synththing-set-aside-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("soundfonts.json");

        assert!(read_json_or_set_aside::<Config>(&path).unwrap().is_none(), "missing");

        fs::write(&path, r#"{ "soundfonts": ["a.sf2"] }"#).unwrap();
        let config = read_json_or_set_aside::<Config>(&path).unwrap().unwrap();
        assert_eq!(config.soundfonts, [PathBuf::from("a.sf2")]);
        assert!(path.exists(), "a readable file stays");

        // From a build where `soundfonts` is something else, and a loop
        // mode this one doesn't have: the rest is still read.
        fs::write(&path, r#"{ "soundfonts": { "list": [] }, "loop_mode": "Sideways", "shuffle": true }"#).unwrap();
        let config = read_json_or_set_aside::<Config>(&path).unwrap().expect("what can be read is");
        assert!(config.shuffle, "kept");
        assert!(config.soundfonts.is_empty(), "left at the default");
        assert!(!path.exists(), "moved aside");
        let notices = set_aside_notices();
        assert!(notices.iter().any(|(n, _)| n.contains("soundfonts, loop_mode") || n.contains("loop_mode, soundfonts")), "{notices:?}");
        let aside: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].starts_with("soundfonts.json.unreadable-"), "{aside:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// A dock layout that's never been drawn (Android, the mobile layout)
    /// has infinite rects: saved, it still reads back.
    #[test]
    fn undrawn_layouts_read_back() {
        let config = Config { layout: Some(crate::layout::default_layout()), ..Config::default() };
        let mut json = serde_json::to_value(&config).unwrap();
        assert!(serde_json::from_value::<Config>(json.clone()).is_err(), "null positions, as serde_json writes infinities");
        finite_positions(&mut json);
        let read: Config = serde_json::from_value(json).expect("reads back");
        assert!(read.layout.is_some());
        let read: Config = serde_json::from_value(config.to_json().unwrap()).expect("as the config gives it");
        assert!(read.layout.is_some());
    }
}
