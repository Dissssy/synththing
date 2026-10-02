//! The app-wide log. Release builds have no console, so everything worth
//! knowing about (status messages, failed loads, update checks, warnings
//! from egui/wgpu/winit, panics on any thread) goes here instead: kept in
//! memory for the Log viewer (Help > Log...) and written to
//! `<config dir>/synththing.log`. The previous run's file is kept as
//! `synththing.prev.log`, so a crash's log survives the restart after it.
//!
//! Anything can log through the standard `log` macros (`log::info!`,
//! `log::warn!`, ...); this is the logger behind them.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Entries kept in memory for the viewer; the file keeps everything.
const MAX_ENTRIES: usize = 5000;

#[derive(Clone, Debug)]
pub struct Entry {
    /// Seconds since the app started.
    pub secs: f64,
    pub level: Level,
    pub target: String,
    pub message: String,
}

impl Entry {
    /// One line, as written to the file and copied from the viewer.
    pub fn line(&self) -> String {
        format!("[{:>9.3}] {:<5} {}: {}", self.secs, self.level, self.target, self.message)
    }
}

struct Logger {
    start: Instant,
    entries: Mutex<VecDeque<Entry>>,
    file: Mutex<Option<File>>,
    path: Option<PathBuf>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

impl Log for Logger {
    /// The app's own messages from Info up; other crates (egui, wgpu, ...)
    /// only from Warn up, they're chatty.
    fn enabled(&self, metadata: &Metadata) -> bool {
        if metadata.target().starts_with("synththing") {
            metadata.level() <= Level::Info
        } else {
            metadata.level() <= Level::Warn
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.push(record.level(), record.target(), record.args().to_string());
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.flush();
        }
    }
}

impl Logger {
    fn push(&self, level: Level, target: &str, message: String) {
        let entry = Entry { secs: self.start.elapsed().as_secs_f64(), level, target: target.to_string(), message };
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = writeln!(file, "{}", entry.line());
        }
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= MAX_ENTRIES {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }
}

/// Install the logger and the panic hook. Call once, first thing in `main`.
/// Logging still works in memory if the log file can't be opened.
pub fn init() {
    let path = crate::config::config_dir().ok().map(|dir| dir.join("synththing.log"));
    let file = path.as_ref().and_then(|path| {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if path.exists() {
            let _ = fs::rename(path, path.with_file_name("synththing.prev.log"));
        }
        File::create(path).ok()
    });
    let logger = LOGGER.get_or_init(|| Logger {
        start: Instant::now(),
        entries: Mutex::new(VecDeque::new()),
        file: Mutex::new(file),
        path,
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }

    // Panics anywhere (the audio thread included) would otherwise vanish
    // without a console. Record them, then let the default hook run too.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!(target: "synththing::panic", "thread '{thread}' panicked: {info}\n{backtrace}");
        log::logger().flush();
        default_hook(info);
    }));
}

/// A copy of the in-memory log, oldest first.
pub fn entries() -> Vec<Entry> {
    LOGGER
        .get()
        .and_then(|l| l.entries.lock().ok().map(|e| e.iter().cloned().collect()))
        .unwrap_or_default()
}

/// Empty the in-memory log (the file keeps everything).
pub fn clear() {
    if let Some(logger) = LOGGER.get()
        && let Ok(mut entries) = logger.entries.lock()
    {
        entries.clear();
    }
}

/// Where the log file is, if there is one.
pub fn file_path() -> Option<PathBuf> {
    LOGGER.get().and_then(|l| l.path.clone())
}
