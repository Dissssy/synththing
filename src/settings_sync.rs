//! Settings sync (`library/sync.rs` carries it): the settings in
//! `config.json` that aren't one device's own, kept the same on every
//! device signed in as one identity that has it on.
//!
//! The config is flattened to its leaves (`library/servers`, `theme`, ...),
//! and each synced one is kept with when it last changed. Every save of
//! the config (`Config::save`) stamps the leaves that changed since the
//! last one, so nothing else needs to know about sync. `sync.json` holds
//! this device's state: its leaves, the server's as of the last sync (the
//! base a merge starts from) and the revision that was.
//!
//! Merging is per leaf, against the base: a leaf only one side changed
//! takes that side's value, and one both changed takes the newer change.
//! Leaves this device doesn't have (a newer app's settings) are carried
//! along untouched.

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
    "browse_dir",
    "record_dir",
    "layout",
    "separate_fullscreen_layout",
    "fullscreen_layout",
    "layout_presets",
    "layout_overrides",
    "active_layout",
    "fullscreen_active_layout",
    "session",
    "buffer_ms",
    "last_version",
    "skipped_update",
    "check_updates_on_launch",
    "canary_updates",
    "android_install_updates",
    "mobile_layout",
    "fullscreen_over_cutout",
    "ui_scale",
    "sync_debug",
];

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

/// This device's sync state (`sync.json`).
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
}

/// A save changed a synced setting since this was last asked.
static DIRTY: AtomicBool = AtomicBool::new(false);

fn path() -> Option<PathBuf> {
    crate::config::config_dir().ok().map(|d| d.join("sync.json"))
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
    /// The saved state, if sync is on here (for the sync key there is now).
    pub fn load() -> Option<Self> {
        let sync_key = crate::library::identity::sync_key()?;
        let state: Self = serde_json::from_str(&std::fs::read_to_string(path()?).ok()?).ok()?;
        (state.key == key_check(&sync_key)).then_some(state)
    }

    /// A new state (sync turned on), its leaves those of `config`.
    pub fn new(sync_key: &[u8; 32], config: &Value) -> Self {
        let applied = synced_leaves(config);
        let at = now();
        let fields = applied.iter().map(|(k, v)| (k.clone(), Field { value: v.clone(), changed: at })).collect();
        Self { key: key_check(sync_key), revision: 0, base: Fields::new(), fields, applied }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no config folder")?;
        std::fs::write(&path, serde_json::to_string(self).unwrap_or_default())
            .map_err(|e| format!("couldn't write {}: {e}", path.display()))
    }

    /// Stamp the leaves of `config` that changed since `applied`; whether
    /// any did.
    pub fn record(&mut self, config: &Value, at: i64) -> bool {
        let leaves = synced_leaves(config);
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
}

/// Turn sync off here (`State::load` finds nothing after this).
pub fn turn_off() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}

/// The config was saved (`Config::save`): with sync on, stamp what changed,
/// and note that it did.
pub fn record(config: &Value) {
    let Some(mut state) = State::load() else { return };
    if state.record(config, now()) {
        if let Err(e) = state.save() {
            log::warn!("settings sync: {e}");
        }
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Whether a save changed a synced setting since this was last asked.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

fn is_local(path: &str) -> bool {
    LOCAL.iter().any(|l| path == *l || path.strip_prefix(l).is_some_and(|rest| rest.starts_with('/')))
}

/// `config`'s leaves that sync, by path (a JSON pointer without its first
/// `/`): objects are walked into; everything else (arrays too) is a leaf.
pub fn synced_leaves(config: &Value) -> BTreeMap<String, Value> {
    fn walk(value: &Value, path: &str, out: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(map) if !map.is_empty() || path.is_empty() => {
                for (key, value) in map {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    let path = if path.is_empty() { key } else { format!("{path}/{key}") };
                    walk(value, &path, out);
                }
            }
            _ if !is_local(path) => {
                out.insert(path.to_string(), value.clone());
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(config, "", &mut out);
    out
}

/// `base`'s leaves merged with this device's (`local`) and the server's:
/// the result, and what it changed of `local`'s.
pub fn merge(base: &Fields, local: &Fields, server: &Fields) -> (Fields, Vec<Change>) {
    let mut merged = Fields::new();
    let mut changes = Vec::new();
    let paths: std::collections::BTreeSet<&String> = local.keys().chain(server.keys()).chain(base.keys()).collect();
    for path in paths {
        let (b, l, s) = (base.get(path).map(|f| &f.value), local.get(path), server.get(path));
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
        if let Some(field) = chosen {
            merged.insert(path.clone(), field.clone());
        }
    }
    (merged, changes)
}

/// Set `config`'s synced leaves to `fields`' (removing those `fields`
/// doesn't have), each only if the config still reads with it: what's
/// left out is a newer app's (or a value this one can't take), carried
/// along in the sync state but not here.
pub fn apply<T: serde::de::DeserializeOwned>(config: &mut Value, fields: &Fields) {
    let current = synced_leaves(config);
    let paths: std::collections::BTreeSet<&String> = current.keys().chain(fields.keys()).collect();
    for path in paths {
        let wanted = fields.get(path).map(|f| &f.value);
        if current.get(path) == wanted {
            continue;
        }
        let mut tried = config.clone();
        set_leaf(&mut tried, path, wanted.cloned());
        if serde_json::from_value::<T>(tried.clone()).is_ok() {
            *config = tried;
        }
    }
}

fn set_leaf(config: &mut Value, path: &str, value: Option<Value>) {
    let mut parts: Vec<String> = path.split('/').map(|p| p.replace("~1", "/").replace("~0", "~")).collect();
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
    match value {
        Some(value) => {
            map.insert(last, value);
        }
        None => {
            map.remove(&last);
        }
    }
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
        let leaves = synced_leaves(&config);
        assert_eq!(leaves.keys().collect::<Vec<_>>(), ["library/pinned/x~1y", "library/servers", "theme"]);
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
        let (merged, changes) = merge(&base, &phone, &server);
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
        let (merged, _) = merge(&base, &fields(&[("a", json!(1), 1), ("b", json!(1), 1)]), &base);
        assert!(!merged.contains_key("c"));
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
        apply::<Config>(&mut config, &synced);
        assert_eq!(config, json!({ "shape": "Square", "theme": "light", "from_newer": 1 }));
    }
}
