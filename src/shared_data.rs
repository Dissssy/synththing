//! Shared data: the link between visualizer scripts (`shared_get`/
//! `shared_set`) and programs on the local API (`/v1/shared`,
//! docs/API.md), a small key-value store both read and write, so they can
//! pass each other values without touching files.
//!
//! Values are JSON (a script's tables become objects or arrays). It's kept
//! in memory only, and belongs to the script running: it's emptied when
//! another is picked or the script restarts (`clear`).

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::{Map, Value};

/// The longest key, in bytes.
pub const LONGEST_KEY: usize = 128;
/// The most it holds, keys and values as JSON, in bytes.
pub const LARGEST: usize = 1 << 20;

#[derive(Default)]
struct Store {
    values: Map<String, Value>,
    /// Its size as `LARGEST` counts it.
    size: usize,
    /// The keys changed since `changes` last asked.
    changed: BTreeSet<String>,
}

fn store() -> MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn size_of(key: &str, value: &Value) -> usize {
    key.len() + value.to_string().len()
}

pub fn get(key: &str) -> Option<Value> {
    store().values.get(key).cloned()
}

pub fn all() -> Map<String, Value> {
    store().values.clone()
}

/// Sets `key` to `value`, or takes it out with `None` (or `null`). Whether
/// it changed, or why it can't be set.
pub fn set(key: &str, value: Option<Value>) -> Result<bool, String> {
    if key.is_empty() || key.len() > LONGEST_KEY {
        return Err(format!("a key is 1 to {LONGEST_KEY} bytes"));
    }
    let mut store = store();
    let old = store.values.get(key).map(|v| size_of(key, v)).unwrap_or(0);
    let changed = match value.filter(|v| !v.is_null()) {
        None => store.values.remove(key).is_some(),
        Some(value) => {
            if store.values.get(key) == Some(&value) {
                return Ok(false);
            }
            let new = size_of(key, &value);
            if store.size - old + new > LARGEST {
                return Err(format!("shared data holds at most {} KB", LARGEST >> 10));
            }
            store.values.insert(key.to_string(), value);
            store.size += new;
            true
        }
    };
    if changed {
        store.size -= old;
        store.changed.insert(key.to_string());
    }
    Ok(changed)
}

/// Takes everything out.
pub fn clear() {
    let mut store = store();
    let keys: Vec<String> = std::mem::take(&mut store.values).into_iter().map(|(k, _)| k).collect();
    store.changed.extend(keys);
    store.size = 0;
}

/// The keys changed since last asked, with what they are now (`None`:
/// taken out).
pub fn changes() -> Vec<(String, Option<Value>)> {
    let mut store = store();
    let changed = std::mem::take(&mut store.changed);
    changed
        .into_iter()
        .map(|key| {
            let value = store.values.get(&key).cloned();
            (key, value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_only_what_changes_and_keeps_to_its_limits() {
        assert_eq!(set("tests.a", Some(Value::from(1))), Ok(true));
        assert_eq!(set("tests.a", Some(Value::from(1))), Ok(false));
        assert_eq!(get("tests.a"), Some(Value::from(1)));
        assert!(set("", Some(Value::from(1))).is_err());
        assert!(set(&"k".repeat(LONGEST_KEY + 1), Some(Value::from(1))).is_err());
        assert!(set("tests.big", Some(Value::from("x".repeat(LARGEST)))).is_err());
        assert!(changes().iter().any(|(k, v)| k == "tests.a" && *v == Some(Value::from(1))));
        assert_eq!(set("tests.a", Some(Value::Null)), Ok(true));
        assert_eq!(get("tests.a"), None);
        assert_eq!(set("tests.a", None), Ok(false));
    }
}
