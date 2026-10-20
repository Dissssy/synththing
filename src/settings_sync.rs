//! Settings sync (`library/sync.rs` carries it): the settings in
//! `config.json` that aren't one device's own, kept the same on every
//! device signed in as one identity that has it on.
//!
//! The config is flattened to its leaves (`library/servers`, `theme`, ...),
//! and each synced one is kept with when it last changed. Every save of
//! the config (`Config::save`) stamps the leaves that changed since the
//! last one, so nothing else needs to know about sync. `sync-settings.json`
//! holds this device's state: its leaves, the server's as of the last sync
//! (the base a merge starts from) and the revision that was.
//!
//! The same state and merge carry other synced data as leaves of its own
//! (`playlist_sync.rs`: a leaf per playlist), each in its own blob and
//! `sync-<name>.json`.
//!
//! Merging is per leaf, against the base: a leaf only one side changed
//! takes that side's value, and one both changed takes the newer change.
//! Leaves this device doesn't have (a newer app's settings) are carried
//! along untouched.
//!
//! Each leaf belongs to a setting (`CATEGORIES`), and each device chooses
//! which settings it syncs: one it doesn't is neither sent nor taken
//! there, only carried along for the devices that do.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The synced data's name on the server.
pub const NAME: &str = "settings";

/// The config's leaves that are this device's own, and every leaf under
/// them: its folders, layouts and window, audio buffer, what it last
/// played, its updates and how it looks on its screen.
const LOCAL: &[&str] = &[
    "soundfonts",
    "record_dir",
    "layout",
    "separate_fullscreen_layout",
    "fullscreen_layout",
    "layout_presets",
    "layout_overrides",
    "active_layout",
    "fullscreen_active_layout",
    "library/official_publishers",
    "session/soundfont",
    "session/script",
    "buffer_ms",
    "last_version",
    "skipped_update",
    "repair_offered",
    "check_updates_on_launch",
    "canary_updates",
    "android_install_updates",
    "mobile_layout",
    "fullscreen_over_cutout",
    "ui_scale",
    "sync_debug",
];

/// A setting a device can choose not to sync: one or more leaves.
pub struct Setting {
    pub key: &'static str,
    pub name: &'static str,
    /// Its leaves: these, and every leaf under them.
    paths: &'static [&'static str],
    /// Synced unless the device says otherwise.
    pub on: bool,
}

/// Settings as Preferences groups them.
pub struct Category {
    pub name: &'static str,
    pub settings: &'static [Setting],
}

const fn setting(key: &'static str, name: &'static str, paths: &'static [&'static str]) -> Setting {
    Setting { key, name, paths, on: true }
}

/// Every setting that syncs, by category. A leaf in none of them (a newer
/// app's) is in `OTHER`.
pub const CATEGORIES: &[Category] = &[
    Category {
        name: "Appearance",
        settings: &[
            setting("theme", "Theme", &["theme", "custom_themes"]),
            setting("beat_pulse", "Beat pulse", &["beat_pulse", "beat_pulse_length"]),
            setting("song_names", "Song names", &["title_patterns"]),
        ],
    },
    Category {
        name: "Playback",
        settings: &[
            setting("remember", "What's remembered between launches", &["remember"]),
            setting("modes", "Loop, Shuffle and Autoplay", &["loop_mode", "shuffle", "autoplay"]),
            setting("heavy_midi", "Warning before a very large MIDI file", &["warn_heavy_midi"]),
            setting("channel_chips", "Channel chips", &["channel_chips"]),
            Setting { key: "volume", name: "Last volume (taken at the next launch)", paths: &["session/volume"], on: false },
            Setting { key: "speed", name: "Last speed (taken at the next launch)", paths: &["session/speed"], on: false },
        ],
    },
    Category {
        name: "Songs",
        settings: &[
            setting("audio_files", "Show audio files", &["show_audio_files"]),
            setting("songs_view", "Columns, sorting and grouping", &["songs_view"]),
            setting("preload", "Preload songs on hover", &["preload_on_hover"]),
            setting("autoplay_collapsed", "Autoplay skips closed groups", &["autoplay_skip_collapsed"]),
            setting("unload", "Unload preloaded files after", &["preload_expiry_secs"]),
            PLAYLISTS,
            SONGS,
            EVERY_SONG,
        ],
    },
    Category {
        name: "Visualizers",
        settings: &[
            setting("mini_player", "Mini player", &["mini_player"]),
            setting("mini_player_shape", "Mini player shape", &["mini_player_shape"]),
            setting("script_logs", "Copy script logs to the app log", &["script_log_to_app"]),
        ],
    },
    Category {
        name: "Script editor",
        settings: &[
            setting("editor_apply", "Apply", &["editor_apply"]),
            setting("editor_font_size", "Text size", &["editor_font_size"]),
            setting("editor_wrap", "Wrap", &["editor_wrap"]),
        ],
    },
    Category {
        name: "Recording",
        settings: &[
            setting("record_resolution", "Resolution", &["record_resolution"]),
            setting("record_fps", "Frame rate", &["record_fps"]),
            setting("record_quality", "Quality", &["record_quality"]),
            setting("record_manual", "Scripts recorded in Manual", &["record_manual"]),
        ],
    },
    Category {
        name: "Online Library",
        settings: &[
            setting("servers", "Servers", &["library/official", "library/servers"]),
            setting("server_keys", "Servers' keys", &["library/pinned"]),
            setting("author_name", "Name to post under", &["library/author_name"]),
            setting("notifications", "Notifications", &["library/notifications"]),
        ],
    },
    Category { name: "App", settings: &[setting("tray", "Keep running in the tray", &["close_to_tray"]), OTHER] },
];

/// Leaves no setting lists: a newer app's, or ones not listed yet.
const OTHER: Setting = setting("other", "Anything else", &[]);

/// The playlists (`playlist_sync.rs`): no settings of their own, but
/// chosen with them.
pub const PLAYLISTS: Setting = setting("playlists", "Playlists", &[]);

/// Fetching songs from the other devices (`song_sync.rs`): those in
/// playlists, or with `EVERY_SONG`, every one in the library.
pub const SONGS: Setting = setting("songs", "Songs from my other devices", &[]);
pub const EVERY_SONG: Setting =
    Setting { key: "every_song", name: "Every song, not only those in playlists", paths: &[], on: false };

fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

/// The setting the leaf at `path` belongs to.
pub fn setting_of(path: &str) -> &'static Setting {
    CATEGORIES
        .iter()
        .flat_map(|c| c.settings)
        .find(|s| s.paths.iter().any(|p| under(path, p)))
        .unwrap_or(&OTHER)
}

/// Which settings a device has turned on or off, where it differs from
/// their defaults.
pub type Choices = BTreeMap<String, bool>;

/// Whether `setting` syncs on a device with these choices.
pub fn syncs(choices: &Choices, setting: &Setting) -> bool {
    choices.get(setting.key).copied().unwrap_or(setting.on)
}

/// A leaf's value and when it last changed (Unix seconds).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub value: Value,
    pub changed: i64,
}

/// Leaves by path.
pub type Fields = BTreeMap<String, Field>;

/// What a sync changed here, for the debug toast.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: String,
    /// Changed here too: which won.
    pub conflict: Option<Winner>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Winner {
    ThisDevice,
    Server,
}

/// This device's state for one synced blob (`sync-<name>.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Which sync key it's for (`key_check`): a state for another one (signed
    /// out, or another identity) is off.
    pub key: String,
    /// The server's revision as of the last sync.
    pub revision: i64,
    /// The server's leaves as of the last sync.
    pub base: Fields,
    /// This device's leaves.
    pub fields: Fields,
    /// The config's synced leaves as of the last save or sync: what a save
    /// is compared with (not `fields`, which can hold values this app
    /// can't take).
    pub applied: BTreeMap<String, Value>,
    /// The settings this device syncs, where they differ from their
    /// defaults (the settings' state only).
    #[serde(default)]
    pub choices: Choices,
}

/// A save changed a synced setting since this was last asked.
static DIRTY: AtomicBool = AtomicBool::new(false);

fn path(name: &str) -> Option<PathBuf> {
    crate::config::config_dir().ok().map(|d| d.join(format!("sync-{name}.json")))
}

/// A short check of a sync key, to tell which one a state is for (not the
/// key itself, which stays in `sync.key`).
fn key_check(sync_key: &[u8; 32]) -> String {
    use sha2::Digest;
    crate::library::hex(&sha2::Sha256::digest(sync_key)[..8])
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl State {
    /// The saved state for the blob `name`, if it syncs here (for the sync
    /// key there is now).
    pub fn load(name: &str) -> Option<Self> {
        let _timing = crate::frame_timing::scope("sync state load");
        let sync_key = crate::library::identity::sync_key()?;
        let state: Self = serde_json::from_str(&std::fs::read_to_string(path(name)?).ok()?).ok()?;
        (state.key == key_check(&sync_key)).then_some(state)
    }

    /// A new settings state (sync turned on), its leaves those of `config`.
    pub fn new(sync_key: &[u8; 32], config: &Value) -> Self {
        Self::of(sync_key, synced_leaves(config, &Choices::new()))
    }

    /// A new state whose leaves are `leaves`, all changed now.
    pub fn of(sync_key: &[u8; 32], applied: BTreeMap<String, Value>) -> Self {
        let at = now();
        let fields = applied.iter().map(|(k, v)| (k.clone(), Field { value: v.clone(), changed: at })).collect();
        Self { key: key_check(sync_key), revision: 0, base: Fields::new(), fields, applied, choices: Choices::new() }
    }

    pub fn save(&self, name: &str) -> Result<(), String> {
        let _timing = crate::frame_timing::scope("sync state save");
        let path = path(name).ok_or("no config folder")?;
        std::fs::write(&path, serde_json::to_string(self).unwrap_or_default())
            .map_err(|e| format!("couldn't write {}: {e}", path.display()))
    }

    /// Stamp the synced leaves of `config` that changed since `applied`;
    /// whether any did.
    pub fn record(&mut self, config: &Value, at: i64) -> bool {
        let leaves = synced_leaves(config, &self.choices);
        self.record_leaves(leaves, at)
    }

    /// Stamp the leaves that changed since `applied`; whether any did.
    pub fn record_leaves(&mut self, leaves: BTreeMap<String, Value>, at: i64) -> bool {
        let mut changed = false;
        for (path, value) in &leaves {
            if self.applied.get(path) != Some(value) {
                self.fields.insert(path.clone(), Field { value: value.clone(), changed: at });
                changed = true;
            }
        }
        for path in self.applied.keys().filter(|p| !leaves.contains_key(*p)) {
            if self.fields.remove(path).is_some() {
                changed = true;
            }
        }
        self.applied = leaves;
        changed
    }

    /// Sync `chosen` here or not. Turned on, it takes the server's (as of
    /// the last sync) where there are any, as turning sync on does.
    pub fn choose(&mut self, chosen: &Setting, on: bool, config: &Value) {
        if on == chosen.on {
            self.choices.remove(chosen.key);
        } else {
            self.choices.insert(chosen.key.to_string(), on);
        }
        if !on {
            return;
        }
        let at = now();
        for (path, value) in synced_leaves(config, &self.choices).into_iter().filter(|(p, _)| setting_of(p).key == chosen.key) {
            let field = self.base.get(&path).cloned().unwrap_or(Field { value: value.clone(), changed: at });
            self.fields.insert(path.clone(), field);
            self.applied.insert(path, value);
        }
    }
}

/// Stop syncing the blob `name` here (`State::load` finds nothing after
/// this); what the server has stays.
pub fn stop(name: &str) {
    if let Some(path) = path(name) {
        let _ = std::fs::remove_file(path);
    }
}


/// The config was saved (`Config::save`): with sync on, stamp what changed,
/// and note that it did.
pub fn record(config: &Value) {
    let Some(mut state) = State::load(NAME) else { return };
    if state.record(config, now()) {
        if let Err(e) = state.save(NAME) {
            log::warn!("settings sync: {e}");
        }
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Whether a save changed a synced setting since this was last asked.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

/// Whether the leaf at `path` syncs on a device with these choices.
fn synced(path: &str, choices: &Choices) -> bool {
    !LOCAL.iter().any(|l| under(path, l)) && syncs(choices, setting_of(path))
}

/// `config`'s leaves that sync here, by path (a JSON pointer without its
/// first `/`): objects are walked into; everything else (arrays too) is a
/// leaf.
pub fn synced_leaves(config: &Value, choices: &Choices) -> BTreeMap<String, Value> {
    fn walk(value: &Value, path: &str, choices: &Choices, out: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(map) if !map.is_empty() || path.is_empty() => {
                for (key, value) in map {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    let path = if path.is_empty() { key } else { format!("{path}/{key}") };
                    walk(value, &path, choices, out);
                }
            }
            _ if synced(path, choices) => {
                out.insert(path.to_string(), value.clone());
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(config, "", choices, &mut out);
    out
}

/// `base`'s leaves merged with this device's (`local`) and the server's:
/// the result, and what it changed of `local`'s. A leaf this device doesn't
/// sync (`choices`) keeps the server's.
pub fn merge(base: &Fields, local: &Fields, server: &Fields, choices: &Choices) -> (Fields, Vec<Change>) {
    merge_where(base, local, server, |path| syncs(choices, setting_of(path)))
}

/// `merge`, for leaves that sync here where `syncs` says.
pub fn merge_where(base: &Fields, local: &Fields, server: &Fields, syncs: impl Fn(&str) -> bool) -> (Fields, Vec<Change>) {
    let mut merged = Fields::new();
    let mut changes = Vec::new();
    let paths: std::collections::BTreeSet<&String> = local.keys().chain(server.keys()).chain(base.keys()).collect();
    for path in paths {
        let (b, l, s) = (base.get(path).map(|f| &f.value), local.get(path), server.get(path));
        if !syncs(path) {
            if let Some(field) = s {
                merged.insert(path.clone(), field.clone());
            }
            continue;
        }
        let local_changed = l.map(|f| &f.value) != b;
        let server_changed = s.map(|f| &f.value) != b;
        let (chosen, conflict) = match (local_changed, server_changed) {
            (_, false) => (l, None),
            (false, true) => (s, None),
            (true, true) if l.map(|f| &f.value) == s.map(|f| &f.value) => (s, None),
            (true, true) => {
                let local_newer = l.map_or(0, |f| f.changed) > s.map_or(0, |f| f.changed);
                if local_newer { (l, Some(Winner::ThisDevice)) } else { (s, Some(Winner::Server)) }
            }
        };
        if chosen.map(|f| &f.value) != l.map(|f| &f.value) || conflict.is_some() {
            changes.push(Change { path: path.clone(), conflict });
        }
        // The same value as the server's is the server's leaf, stamp and
        // all: two devices that set it apart would otherwise write their
        // own stamps over each other's forever.
        let chosen = if chosen.map(|f| &f.value) == s.map(|f| &f.value) { s } else { chosen };
        if let Some(field) = chosen {
            merged.insert(path.clone(), field.clone());
        }
    }
    (merged, changes)
}

/// Set `config`'s synced leaves to `fields`' (removing those `fields`
/// doesn't have). All at once if the config reads with them (a new
/// custom theme is many leaves, none of which reads alone); else each
/// only if the config still reads with it: what's left out is a newer
/// app's (or a value this one can't take), carried along in the sync
/// state but not here.
pub fn apply<T: serde::de::DeserializeOwned>(config: &mut Value, fields: &Fields, choices: &Choices) {
    let current = synced_leaves(config, choices);
    let paths: std::collections::BTreeSet<&String> =
        current.keys().chain(fields.keys().filter(|p| synced(p, choices))).collect();
    let differing: Vec<(&String, Option<&Value>)> = paths
        .into_iter()
        .map(|path| (path, fields.get(path).map(|f| &f.value)))
        .filter(|(path, wanted)| current.get(*path) != *wanted)
        .collect();
    let mut all = config.clone();
    for (path, wanted) in &differing {
        set_leaf(&mut all, path, wanted.cloned());
    }
    if serde_json::from_value::<T>(all.clone()).is_ok() {
        *config = all;
        return;
    }
    for (path, wanted) in differing {
        let mut tried = config.clone();
        set_leaf(&mut tried, path, wanted.cloned());
        if serde_json::from_value::<T>(tried.clone()).is_ok() {
            *config = tried;
        }
    }
}

/// Set the leaf at `path` to `value`, or remove it (`None`), with the
/// objects it leaves empty: an empty object is a leaf of its own
/// (`synced_leaves`), so one that's only there for removed leaves isn't
/// wanted.
fn set_leaf(config: &mut Value, path: &str, value: Option<Value>) {
    let parts: Vec<String> = path.split('/').map(|p| p.replace("~1", "/").replace("~0", "~")).collect();
    match value {
        Some(value) => insert_leaf(config, &parts, value),
        None => {
            remove_leaf(config, &parts);
        }
    }
}

/// Remove the leaf at `parts`; whether that emptied its object.
fn remove_leaf(node: &mut Value, parts: &[String]) -> bool {
    let (Value::Object(map), Some((first, rest))) = (node, parts.split_first()) else { return false };
    let removed = if rest.is_empty() {
        map.remove(first).is_some()
    } else {
        let emptied = map.get_mut(first).is_some_and(|child| remove_leaf(child, rest));
        emptied && map.remove(first).is_some()
    };
    removed && map.is_empty()
}

fn insert_leaf(config: &mut Value, parts: &[String], value: Value) {
    let mut parts = parts.to_vec();
    let Some(last) = parts.pop() else { return };
    let mut node = config;
    for part in parts {
        if !node.is_object() {
            *node = Value::Object(Default::default());
        }
        let Value::Object(map) = node else { return };
        node = map.entry(part).or_insert_with(|| Value::Object(Default::default()));
    }
    if !node.is_object() {
        *node = Value::Object(Default::default());
    }
    let Value::Object(map) = node else { return };
    map.insert(last, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(pairs: &[(&str, Value, i64)]) -> Fields {
        pairs.iter().map(|(p, v, t)| (p.to_string(), Field { value: v.clone(), changed: *t })).collect()
    }

    #[test]
    fn leaves_leave_out_the_devices_own() {
        let config = json!({ "theme": "dark", "layout": { "a": 1 }, "library": { "servers": [1], "pinned": { "x/y": "k" } } });
        let leaves = synced_leaves(&config, &Choices::new());
        assert_eq!(leaves.keys().collect::<Vec<_>>(), ["library/pinned/x~1y", "library/servers", "theme"]);
        let no_library = Choices::from([("servers".to_string(), false), ("server_keys".to_string(), false)]);
        assert_eq!(synced_leaves(&config, &no_library).keys().collect::<Vec<_>>(), ["theme"]);
        let session = json!({ "session": { "volume": 0.5, "script": "C:/x.lua" } });
        assert!(synced_leaves(&session, &Choices::new()).is_empty(), "volume and speed are off by default");
        let with_volume = Choices::from([("volume".to_string(), true)]);
        assert_eq!(synced_leaves(&session, &with_volume).keys().collect::<Vec<_>>(), ["session/volume"]);
    }

    #[test]
    fn saves_stamp_what_changed() {
        let mut state = State::new(&[1; 32], &json!({ "theme": "dark", "shuffle": false }));
        assert!(!state.record(&json!({ "theme": "dark", "shuffle": false }), 100));
        assert!(state.record(&json!({ "theme": "light", "shuffle": false }), 100));
        assert_eq!(state.fields["theme"], Field { value: json!("light"), changed: 100 });
        assert_ne!(state.fields["shuffle"].changed, 100);
    }

    #[test]
    fn merging() {
        // The desktop set a, then b; the phone, offline, set a in between.
        let base = fields(&[("a", json!(1), 1), ("b", json!(1), 1), ("c", json!(1), 1)]);
        let server = fields(&[("a", json!(2), 10), ("b", json!(2), 30), ("c", json!(1), 1)]);
        let phone = fields(&[("a", json!(3), 20), ("b", json!(1), 1), ("c", json!(1), 1), ("new", json!(true), 20)]);
        let (merged, changes) = merge(&base, &phone, &server, &Choices::new());
        assert_eq!(merged["a"].value, json!(3), "both changed it: the newer change wins");
        assert_eq!(merged["b"].value, json!(2), "only the server changed it");
        assert_eq!(merged["new"].value, json!(true), "only the phone has it");
        assert_eq!(
            changes,
            [
                Change { path: "a".into(), conflict: Some(Winner::ThisDevice) },
                Change { path: "b".into(), conflict: None },
            ]
        );
        // Removed on one side.
        let (merged, _) = merge(&base, &fields(&[("a", json!(1), 1), ("b", json!(1), 1)]), &base, &Choices::new());
        assert!(!merged.contains_key("c"));
    }

    #[test]
    fn same_values_settle_on_the_servers_stamps() {
        // Both devices set a to 2, at different times: nothing to write.
        let base = fields(&[("a", json!(1), 1)]);
        let server = fields(&[("a", json!(2), 10)]);
        let phone = fields(&[("a", json!(2), 20)]);
        let (merged, changes) = merge(&base, &phone, &server, &Choices::new());
        assert_eq!(merged, server);
        assert!(changes.is_empty());
        // Neither changed it since the last sync, but their stamps differ.
        let (merged, changes) = merge(&server, &phone, &server, &Choices::new());
        assert_eq!(merged, server);
        assert!(changes.is_empty());
    }

    #[test]
    fn categories_turned_off_are_carried_along() {
        let off = Choices::from([("theme".to_string(), false)]);
        let base = fields(&[("theme", json!("dark"), 1), ("shuffle", json!(false), 1)]);
        let server = fields(&[("theme", json!("light"), 5), ("shuffle", json!(false), 1)]);
        // (Saving dropped the theme from what this device syncs.)
        let local = fields(&[("shuffle", json!(true), 6)]);
        let (merged, changes) = merge(&base, &local, &server, &off);
        assert_eq!(merged["theme"].value, json!("light"), "the server's, carried along");
        assert_eq!(merged["shuffle"].value, json!(true));
        assert_eq!(changes, []);
        let mut config = json!({ "theme": "dark", "shuffle": true });
        apply::<serde_json::Value>(&mut config, &merged, &off);
        assert_eq!(config["theme"], json!("dark"), "not taken here");

        // Turned back on: the server's, as of the last sync.
        let mut state = State::new(&[1; 32], &config);
        state.choices = off;
        state.base = merged.clone();
        state.choose(setting_of("theme"), true, &config);
        assert!(state.choices.is_empty());
        assert_eq!(state.fields["theme"].value, json!("light"));
        assert!(!state.record(&config, 100), "the config's theme isn't a change made here");
    }

    #[test]
    fn applying_skips_what_doesnt_read() {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Config {
            #[serde(default)]
            shape: Option<Shape>,
            #[serde(default)]
            theme: Option<String>,
        }
        #[derive(Deserialize)]
        enum Shape {
            Square,
        }
        let mut config = json!({ "shape": "Square", "theme": "dark" });
        let synced = fields(&[("shape", json!("Hexagon"), 5), ("theme", json!("light"), 5), ("from_newer", json!(1), 5)]);
        apply::<Config>(&mut config, &synced, &Choices::new());
        assert_eq!(config, json!({ "shape": "Square", "theme": "light", "from_newer": 1 }));
    }

    #[test]
    fn custom_themes_come_and_go_whole() {
        use crate::config::Config;
        let leaves = |config: &Config| -> Fields {
            synced_leaves(&serde_json::to_value(config).unwrap(), &Choices::new())
                .into_iter()
                .map(|(path, value)| (path, Field { value, changed: 5 }))
                .collect()
        };
        let mut other = Config::default();
        let mut mine = crate::theme::builtins().remove(0);
        mine.name = "Mine".into();
        other.custom_themes.insert(mine.name.clone(), mine.clone());
        other.theme = Some(mine.name.clone());

        let mut config = serde_json::to_value(Config::default()).unwrap();
        apply::<Config>(&mut config, &leaves(&other), &Choices::new());
        let applied: Config = serde_json::from_value(config.clone()).unwrap();
        assert_eq!(applied.custom_themes.get("Mine"), Some(&mine));
        assert_eq!(applied.theme.as_deref(), Some("Mine"));

        apply::<Config>(&mut config, &leaves(&Config::default()), &Choices::new());
        let applied: Config = serde_json::from_value(config.clone()).unwrap();
        assert!(applied.custom_themes.is_empty());
        assert_eq!(config["custom_themes"], json!({}), "left as the other device has it");
    }
}
