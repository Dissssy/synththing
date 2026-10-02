//! Live refresh for the folders the app shows: tells the app when files
//! appear, disappear or change in the Songs browser's current folder or the
//! scripts folder, so those views update without a manual refresh.
//!
//! Uses the OS's own change notifications (via `notify`), not polling, so
//! watching costs nothing while nothing changes, even on a network drive.
//! Bursts of events (an editor saving in several steps, a folder of files
//! being copied in) are settled for `SETTLE` before the app is told, so a
//! refresh never catches a file halfway through being written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// How long a folder has to be quiet after a change before it's refreshed.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Watched {
    /// The folder the Songs browser is showing.
    Songs,
    /// The visualizer scripts folder.
    Scripts,
}

pub struct FolderWatch {
    watcher: Option<RecommendedWatcher>,
    dirs: HashMap<Watched, PathBuf>,
    /// Paths that changed, as reported by the watcher thread.
    changed: Arc<Mutex<Vec<PathBuf>>>,
    /// When each watched folder last saw a change that hasn't been reported
    /// to the app yet.
    dirty: HashMap<Watched, Instant>,
}

impl FolderWatch {
    /// `ctx` is woken when something changes, so an idle window still
    /// refreshes. If the OS watcher can't start, this quietly does nothing.
    pub fn new(ctx: egui::Context) -> Self {
        let changed: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let sink = Arc::clone(&changed);
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result {
            Ok(event) => {
                if let Ok(mut changed) = sink.lock() {
                    changed.extend(event.paths);
                }
                ctx.request_repaint_after(SETTLE + Duration::from_millis(20));
            }
            Err(e) => log::warn!("folder watcher: {e}"),
        });
        let watcher = match watcher {
            Ok(watcher) => Some(watcher),
            Err(e) => {
                log::warn!("couldn't start watching folders for changes: {e}");
                None
            }
        };
        Self { watcher, dirs: HashMap::new(), changed, dirty: HashMap::new() }
    }

    /// Watch `dir` (not its subfolders) as `which`, replacing whatever was
    /// watched as `which` before. Cheap to call every frame with the same
    /// folder.
    pub fn watch(&mut self, which: Watched, dir: &Path) {
        if self.dirs.get(&which).is_some_and(|d| d == dir) {
            return;
        }
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        if let Some(old) = self.dirs.remove(&which)
            && !self.dirs.values().any(|d| *d == old)
        {
            let _ = watcher.unwatch(&old);
        }
        if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            log::warn!("can't watch {} for changes: {e}", dir.display());
        }
        self.dirs.insert(which, dir.to_path_buf());
        self.dirty.remove(&which);
    }

    /// The watched folders that changed and have since settled, each
    /// reported once.
    pub fn poll(&mut self) -> Vec<Watched> {
        let now = Instant::now();
        let changed: Vec<PathBuf> = self.changed.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
        for path in changed {
            for (&which, dir) in &self.dirs {
                if path.starts_with(dir) {
                    self.dirty.insert(which, now);
                }
            }
        }
        let settled: Vec<Watched> = self
            .dirty
            .iter()
            .filter(|&(_, &at)| now.duration_since(at) >= SETTLE)
            .map(|(&which, _)| which)
            .collect();
        for which in &settled {
            self.dirty.remove(which);
        }
        settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_change_once_it_settles() {
        let dir = std::env::temp_dir().join(format!("synththing-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut watch = FolderWatch::new(egui::Context::default());
        if watch.watcher.is_none() {
            return; // no OS watcher available in this environment
        }
        watch.watch(Watched::Scripts, &dir);
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.join("new.lua"), "-- hi").unwrap();

        let mut seen = Vec::new();
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            seen.extend(watch.poll());
            if !seen.is_empty() {
                break;
            }
        }
        assert_eq!(seen, vec![Watched::Scripts]);
        // Reported once, not again on the next poll.
        std::thread::sleep(SETTLE);
        assert!(watch.poll().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
