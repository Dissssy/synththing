//! Anonymous usage data (docs/USAGE.md), for people who said yes to it
//! (Preferences > App, or the question on first launch): how often each
//! kind of log line and each panic came up, sent to the official server
//! once a day, a panic's report as soon as the app runs again. The server's
//! rules (`library::usage::UsageRule`) say which lines it wants; the rest
//! are counted and dropped.
//!
//! A line is sent as where it was logged (module and line) and the format
//! string it was written with (from the map build.rs makes of every log
//! call, `LOG_LINES`), never what it was filled in with, so song names,
//! paths and errors stay on the device. A panic carries its
//! message with numbers, quoted text and paths taken out. Nothing says
//! which device or person it came from. Branch builds never send it.
//!
//! The counts are kept in `<config dir>/usage.json` until they're sent, so
//! they survive restarts and crashes.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use log::Record;
use serde::{Deserialize, Serialize};

use crate::build_info::{self, Channel};
use crate::library::usage::{kept, parse_rules, UsageLine, UsageReport, MAX_USAGE_LINES, MAX_USAGE_TEXT};
use crate::library::{client, official_url};

/// A report goes once this long after the last one.
const REPORT_EVERY: i64 = 24 * 60 * 60;
/// How often the counts are saved and a report is considered.
const TICK: Duration = Duration::from_secs(10 * 60);
/// The longest a panic's message is kept.
const MESSAGE_CHARS: usize = 200;

static ON: AtomicBool = AtomicBool::new(false);
/// The counts, once loaded.
static COUNTS: Mutex<Option<Counts>> = Mutex::new(None);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Counts {
    /// When counting started, or the last report went (unix time).
    since: i64,
    sessions: u64,
    /// A panic is waiting to be sent: send without waiting for the day.
    panicked: bool,
    lines: Vec<UsageLine>,
}

impl Counts {
    fn add(&mut self, level: &str, target: String, signature: String) {
        let full = self.lines.len() >= MAX_USAGE_LINES;
        match self.lines.iter_mut().find(|l| l.level == level && l.target == target && l.signature == signature) {
            Some(line) => line.count += 1,
            None if !full => self.lines.push(UsageLine { level: level.to_string(), target, signature, count: 1 }),
            None => {}
        }
    }

    /// Take away what a sent report counted: what came in while it was
    /// being sent stays.
    fn subtract(&mut self, sent: &Counts) {
        self.sessions = self.sessions.saturating_sub(sent.sessions);
        for line in &sent.lines {
            if let Some(mine) = self.lines.iter_mut().find(|l| l.level == line.level && l.target == line.target && l.signature == line.signature) {
                mine.count = mine.count.saturating_sub(line.count);
            }
        }
        self.lines.retain(|l| l.count > 0);
        self.panicked = false;
        self.since = now();
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn path() -> Option<PathBuf> {
    crate::config::config_dir().ok().map(|dir| dir.join("usage.json"))
}

fn lock() -> MutexGuard<'static, Option<Counts>> {
    COUNTS.lock().unwrap_or_else(|p| p.into_inner())
}

/// The counts, loaded from the file the first time.
fn loaded(counts: &mut Option<Counts>) -> &mut Counts {
    counts.get_or_insert_with(|| {
        let mut counts: Counts = path().and_then(|p| fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if counts.since == 0 {
            counts.since = now();
        }
        counts
    })
}

fn save(counts: &Counts) {
    if let Some(path) = path()
        && let Ok(bytes) = serde_json::to_vec(counts)
    {
        let _ = fs::write(path, bytes);
    }
}

/// Whether usage data can be sent from this build at all.
pub fn available() -> bool {
    !matches!(build_info::channel(), Channel::Branch(_))
}

/// Start counting (if they said yes) and the thread that sends the
/// reports. Call once, when the app starts.
pub fn start(on: bool) {
    set_on(on);
    if ON.load(Ordering::Relaxed) {
        loaded(&mut lock()).sessions += 1;
    }
    let spawned = std::thread::Builder::new().name("usage-data".to_string()).spawn(|| loop {
        tick();
        std::thread::sleep(TICK);
    });
    if let Err(e) = spawned {
        log::warn!("couldn't start the usage data thread: {e}");
    }
}

/// Turn counting on or off. Off forgets everything not sent yet.
pub fn set_on(on: bool) {
    let on = on && available();
    if ON.swap(on, Ordering::Relaxed) == on {
        return;
    }
    let mut counts = lock();
    if on {
        loaded(&mut counts);
    } else {
        *counts = None;
        if let Some(path) = path() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Count a log line (from `applog`, for every line it keeps).
pub fn count(record: &Record) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let level = match record.level() {
        log::Level::Error => "error",
        log::Level::Warn => "warn",
        log::Level::Info => "info",
        _ => return,
    };
    let signature = signature(record.module_path().unwrap_or(record.target()), record.line(), record.args().as_str());
    loaded(&mut lock()).add(level, clip(record.target(), MAX_USAGE_TEXT), signature);
}

/// Count a panic and save it at once: the app may be going down. (From
/// `applog`'s panic hook.)
pub fn panicked(info: &std::panic::PanicHookInfo) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    // (try_lock: the panic may have happened while the counts were locked.)
    let Ok(mut counts) = COUNTS.try_lock() else { return };
    let thread = clean(std::thread::current().name().unwrap_or("unnamed"));
    let place = info.location().map_or_else(|| "unknown".to_string(), |l| format!("{}:{}", short_path(l.file()), l.line()));
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_default();
    let counts = loaded(&mut counts);
    counts.add("panic", thread, clip(&format!("{place}: {}", clean(&message)), MAX_USAGE_TEXT));
    counts.panicked = true;
    save(counts);
}

// `LOG_LINES`: every log call in `src/`, as (module, line, format string),
// sorted (build.rs).
include!(concat!(env!("OUT_DIR"), "/log_lines.rs"));

/// The format string the log call at `module:line` was written with, if
/// it's one of the app's own.
fn template(module: &str, line: u32) -> Option<&'static str> {
    LOG_LINES.binary_search_by(|&(m, l, _)| (m, l).cmp(&(module, line))).ok().map(|i| LOG_LINES[i].2)
}

/// Where a line was logged, and its text: the format string it was
/// written with (`couldn't load {path}: {e}`), or the text itself when
/// that's fixed (another crate's line without arguments).
fn signature(module: &str, line: Option<u32>, fixed: Option<&str>) -> String {
    let line = line.unwrap_or(0);
    let place = format!("{module}:{line}");
    let signature = match template(module, line).or(fixed) {
        Some(text) => format!("{place} {}", text.lines().next().unwrap_or_default()),
        None => place,
    };
    clip(&signature, MAX_USAGE_TEXT)
}

/// A source file from the crate's folder on: `src/app.rs`, or
/// `wgpu-core-0.19.0/src/device.rs` for a dependency (not the cargo
/// folder in someone's home).
fn short_path(file: &str) -> String {
    let file = file.replace('\\', "/");
    let parts: Vec<&str> = file.split('/').collect();
    match parts.iter().rposition(|&p| p == "src") {
        Some(src) => parts[src.saturating_sub(1)..].join("/"),
        None => parts.last().copied().unwrap_or_default().to_string(),
    }
}

/// A message's first line with what could be about the person taken out:
/// numbers become `#`, quoted text `"…"`, anything with a slash `<path>`.
fn clean(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default();
    let mut words = Vec::new();
    let mut quote: Option<char> = None;
    for word in line.split(' ') {
        if let Some(q) = quote {
            if word.ends_with(q) {
                quote = None;
            }
            continue;
        }
        let first = word.chars().next();
        if let Some(q @ ('"' | '\'' | '`')) = first {
            words.push("\"…\"".to_string());
            if word.len() == 1 || !word[1..].ends_with(q) {
                quote = Some(q);
            }
            continue;
        }
        if word.contains('/') || word.contains('\\') {
            words.push("<path>".to_string());
            continue;
        }
        let mut cleaned = String::new();
        for c in word.chars() {
            if c.is_ascii_digit() {
                if !cleaned.ends_with('#') {
                    cleaned.push('#');
                }
            } else {
                cleaned.push(c);
            }
        }
        words.push(cleaned);
    }
    clip(&words.join(" "), MESSAGE_CHARS)
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

fn version() -> String {
    match build_info::channel() {
        Channel::Nightly => format!("{} nightly", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn report(counts: &Counts, rules: Option<&[crate::library::usage::UsageRule]>) -> UsageReport {
    UsageReport {
        version: version(),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        sessions: counts.sessions,
        lines: counts.lines.iter().filter(|line| rules.is_none_or(|rules| kept(rules, line))).cloned().collect(),
    }
}

/// What the next report holds so far, before the server's rules leave
/// lines out, for Preferences to show. `None` while it's off.
pub fn preview() -> Option<String> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    let counts = loaded(&mut lock()).clone();
    serde_json::to_string_pretty(&report(&counts, None)).ok()
}

/// Save the counts, and send a report if one's due.
fn tick() {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let counts = {
        let mut guard = lock();
        let counts = loaded(&mut guard);
        save(counts);
        counts.clone()
    };
    if !counts.panicked && now() - counts.since < REPORT_EVERY {
        return;
    }
    let base = official_url();
    let sent = client::usage_rules(&base)
        .and_then(|rules| parse_rules(&rules))
        .and_then(|rules| client::usage(&base, &report(&counts, Some(&rules))));
    match sent {
        Ok(()) => {
            let mut guard = lock();
            if let Some(mine) = guard.as_mut() {
                mine.subtract(&counts);
                save(mine);
            }
            drop(guard);
            log::info!("sent the usage report");
        }
        // (Tried again next tick.)
        Err(e) => log::info!("couldn't send the usage report: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_keep_only_fixed_text() {
        assert_eq!(signature("synththing::library::sync", Some(42), None), "synththing::library::sync:42");
        assert_eq!(signature("wgpu_core::device", Some(7), Some("device lost\nmore")), "wgpu_core::device:7 device lost");
    }

    #[test]
    fn the_apps_own_lines_carry_their_format_string() {
        let &(module, line, format) = LOG_LINES.iter().find(|(_, _, f)| f.contains('{')).unwrap();
        assert_eq!(signature(module, Some(line), None), clip(&format!("{module}:{line} {}", format.lines().next().unwrap()), MAX_USAGE_TEXT));
        assert!(LOG_LINES.windows(2).all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)), "sorted, for the lookup");
        // This file's own call (in `start`) is in the map, at its line.
        let here = include_str!("usage.rs").lines().position(|l| l.contains("couldn't start the usage data thread")).unwrap() + 1;
        assert_eq!(template("synththing::usage", here as u32), Some("couldn't start the usage data thread: {e}"));
    }

    #[test]
    fn panic_messages_lose_what_could_be_personal() {
        assert_eq!(
            clean("index out of bounds: the len is 3 but the index is 15"),
            "index out of bounds: the len is # but the index is #"
        );
        assert_eq!(
            clean("couldn't open \"My Song (live).mid\" in C:\\Users\\ethan\\Music: denied"),
            "couldn't open \"…\" in <path> denied"
        );
        assert_eq!(clean("called `Option::unwrap()` on a `None` value"), "called \"…\" on a \"…\" value");
    }

    #[test]
    fn paths_start_at_the_crate() {
        assert_eq!(short_path("src/app.rs"), "src/app.rs");
        assert_eq!(
            short_path("/home/someone/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wgpu-core-0.19.0/src/device/mod.rs"),
            "wgpu-core-0.19.0/src/device/mod.rs"
        );
        assert_eq!(short_path("C:\\Users\\x\\.cargo\\git\\checkouts\\egui\\crates\\egui\\src\\ui.rs"), "egui/src/ui.rs");
    }

    #[test]
    fn a_sent_report_leaves_what_came_in_since() {
        let mut counts = Counts { sessions: 2, panicked: true, ..Default::default() };
        counts.add("warn", "a".into(), "a:1".into());
        let sent = counts.clone();
        counts.add("warn", "a".into(), "a:1".into());
        counts.add("error", "b".into(), "b:2".into());
        counts.subtract(&sent);
        assert_eq!(counts.sessions, 0);
        assert!(!counts.panicked);
        assert_eq!(counts.lines.iter().map(|l| (l.signature.as_str(), l.count)).collect::<Vec<_>>(), vec![("a:1", 1), ("b:2", 1)]);
    }
}
