//! The app-wide log. Release builds have no console, so everything worth
//! knowing about (status messages, failed loads, update checks, warnings
//! from egui/wgpu/winit, panics on any thread) goes here instead: kept in
//! memory for the Log viewer (Help > Log) and written to
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
    /// Print each entry to stderr too (`--console`).
    echo: bool,
    /// Everything (every crate) from this level up instead, for chasing a
    /// problem: `SYNTHTHING_LOG=debug` or `trace` in the environment.
    verbose: Option<LevelFilter>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Library warnings about normal operation, dropped by message. egui_wgpu
/// warns "No window - clearing all surfaces" each time Android suspends the
/// app (the window goes away while backgrounded); that's expected, not a
/// problem worth logging or reporting (#386).
fn is_noise(record: &Record) -> bool {
    record.target() == "egui_wgpu::winit"
        && record.args().as_str() == Some("No window - clearing all surfaces")
}

/// Library warnings that describe the device rather than a problem, kept
/// in the log at info level (for diagnosing GPU trouble) but not counted.
/// wgpu-core warns "Missing downlevel flags" and then dumps the adapter's
/// downlevel capabilities whenever it opens a device on a GPU that isn't
/// fully WebGPU compliant, which most phone GPUs aren't (#389).
fn is_info(record: &Record) -> bool {
    record.target() == "wgpu_core::instance" && {
        let message = record.args().to_string();
        message.starts_with("Missing downlevel flags") || message.starts_with("DownlevelCapabilities")
    }
}

impl Log for Logger {
    /// The app's own messages from Info up; other crates (egui, wgpu, ...)
    /// only from Warn up, they're chatty. Nothing from symphonia: it logs
    /// files it can't read (even at error level, "probe reach EOF") that
    /// it also hands back as an `Err`, which the app handles where it
    /// probes (no info, no picture, or ffmpeg instead).
    fn enabled(&self, metadata: &Metadata) -> bool {
        if metadata.target().starts_with("symphonia") {
            false
        } else if let Some(verbose) = self.verbose {
            metadata.level() <= verbose
        } else if metadata.target().starts_with("synththing") {
            metadata.level() <= Level::Info
        } else {
            metadata.level() <= Level::Warn
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) || is_noise(record) {
            return;
        }
        if is_info(record) {
            self.push(Level::Info, record.target(), record.args().to_string());
            return;
        }
        // (A panic is counted by the hook, from what it knows of it.)
        if record.target() != "synththing::panic" {
            crate::usage::count(record);
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
        if self.echo {
            let _ = writeln!(std::io::stderr(), "{}", entry.line());
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
/// Logging still works in memory if the log file can't be opened. `echo`:
/// print everything to the terminal as well (`--console`).
pub fn init(echo: bool) {
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
        echo,
        verbose: std::env::var("SYNTHTHING_LOG").ok().and_then(|level| level.parse().ok()),
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(logger.verbose.unwrap_or(LevelFilter::Info).max(LevelFilter::Info));
    }

    // Panics anywhere (the audio thread included) would otherwise vanish
    // without a console. Record them, then let the default hook run too.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let backtrace = std::backtrace::Backtrace::force_capture();
        crate::usage::panicked(info);
        log::error!(target: "synththing::panic", "thread '{thread}' panicked: {info}\n{backtrace}");
        log::logger().flush();
        default_hook(info);
    }));
}

/// Seconds since the app started: the clock `Entry::secs` is on.
pub fn elapsed() -> f64 {
    LOGGER.get().map_or(0.0, |l| l.start.elapsed().as_secs_f64())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_egui_wgpu_suspend_warning() {
        let noise = |target: &str, args: std::fmt::Arguments| {
            is_noise(&Record::builder().level(Level::Warn).target(target).args(args).build())
        };
        assert!(noise("egui_wgpu::winit", format_args!("No window - clearing all surfaces")));
        assert!(!noise("egui_wgpu::winit", format_args!("Surface lost")));
        assert!(!noise("synththing::app", format_args!("No window - clearing all surfaces")));
    }

    #[test]
    fn demotes_wgpu_downlevel_warnings() {
        let info = |target: &str, args: std::fmt::Arguments| {
            is_info(&Record::builder().level(Level::Warn).target(target).args(args).build())
        };
        let flags = "VERTEX_STORAGE";
        assert!(info("wgpu_core::instance", format_args!("Missing downlevel flags: {flags}\nThis is not an invalid use")));
        assert!(info("wgpu_core::instance", format_args!("{:#?}", eframe::wgpu::DownlevelCapabilities::default())));
        assert!(!info("wgpu_core::instance", format_args!("Adapter lost")));
        assert!(!info("synththing::app", format_args!("Missing downlevel flags: {flags}")));
    }
}
