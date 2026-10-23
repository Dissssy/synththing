//! The script editor's background worker. Every edit
//! sends the source to [`CompletionWorker`], which analyzes it off the UI
//! thread (`lua_analysis`: scopes, warnings, completions, signatures) and
//! keeps the newest result; a stale one finishing late is dropped. Also
//! hover/lookup info for a single identifier. The host API itself is
//! `lua_api`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::lua_analysis::{self, Analysis};

/// Hover/lookup info for a single identifier, by exact name match against
/// the host API (`lua_api::details`) and Lua's own globals. Cheap enough to call directly
/// from the UI thread every frame the mouse is over the editor, no need to
/// route this through [`CompletionWorker`].
pub fn lookup(word: &str) -> Option<&'static str> {
    crate::lua_api::details()
        .iter()
        .chain(lua_analysis::LUA_GLOBALS)
        .find(|&&(name, _)| name == word)
        .map(|&(_, detail)| detail)
}

/// One autocomplete suggestion.
#[derive(Clone)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
}

/// Analyzes the script off the UI thread on every [`request`](Self::request),
/// keeping only the result of the most recent request: an in-flight one
/// that's since been superseded by a newer edit finishes but is discarded
/// rather than overwriting a fresher result.
pub struct CompletionWorker {
    generation: Arc<AtomicU64>,
    analysis: Arc<Mutex<Arc<Analysis>>>,
    /// Globals a script has without defining them.
    known: Arc<HashSet<String>>,
}

impl CompletionWorker {
    /// `host_globals`: the app's own globals, from a real script VM.
    pub fn new(host_globals: Vec<String>) -> Self {
        let worker = Self {
            generation: Arc::new(AtomicU64::new(0)),
            analysis: Arc::default(),
            known: Arc::new(lua_analysis::known_globals(host_globals)),
        };
        worker.request(String::new());
        worker
    }

    /// Call whenever the script source changes.
    pub fn request(&self, source: String) {
        let this_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = Arc::clone(&self.generation);
        let analysis = Arc::clone(&self.analysis);
        let known = Arc::clone(&self.known);
        thread::spawn(move || {
            let result = Arc::new(lua_analysis::analyze(&source, &known));
            if generation.load(Ordering::SeqCst) == this_generation {
                *analysis.lock().unwrap() = result;
            }
        });
    }

    /// The most recent analysis (possibly an edit or two behind).
    pub fn analysis(&self) -> Arc<Analysis> {
        Arc::clone(&self.analysis.lock().unwrap())
    }

    /// Every host function, for the Functions list.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        crate::lua_api::details()
            .iter()
            .map(|&(label, detail)| Suggestion { label: label.to_string(), detail: detail.to_string() })
            .collect()
    }
}
