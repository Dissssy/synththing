//! Anonymous usage data (docs/USAGE.md): what an app that opted in sends
//! the official server, and the rules that say which log lines it wants.
//! Shared by the app (`crate::usage`) and the server (`server/usage.rs`).

use serde::{Deserialize, Serialize};

/// The most lines one report may carry, and the longest any of its text.
pub const MAX_USAGE_LINES: usize = 1000;
pub const MAX_USAGE_TEXT: usize = 300;

/// The levels a line can have: the log's, and panics.
pub const USAGE_LEVELS: [&str; 4] = ["panic", "error", "warn", "info"];

/// What the server keeps until it's told otherwise: problems, not the
/// app's everyday info lines.
pub const DEFAULT_USAGE_RULES: [&str; 3] = ["error * keep", "warn * keep", "info * drop"];

/// One report: counts since the last one. Nothing in it says who sent it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageReport {
    /// The release ("1.7.0"), with " nightly" for a canary build.
    pub version: String,
    /// OS and CPU ("windows x86_64").
    pub platform: String,
    /// Times the app was started.
    pub sessions: u64,
    pub lines: Vec<UsageLine>,
    /// How long frames took, per kind of hardware and the settings that
    /// cost the most.
    #[serde(default)]
    pub perf: Vec<PerfGroup>,
}

/// Frame-time buckets' upper ends, in microseconds: under 8 ms, 16.7 ms
/// (60 fps), 33 ms, 50 ms, 100 ms, and slower.
pub const FRAME_BUCKETS_US: [u64; 5] = [8_000, 16_667, 33_333, 50_000, 100_000];
/// Groups (hardware and settings) one report may carry, and sections per
/// group.
pub const MAX_PERF_GROUPS: usize = 20;
pub const MAX_PERF_SECTIONS: usize = 64;

/// What the app runs on, coarsely: enough to pick defaults by, not enough
/// to tell one device from another.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hardware {
    /// The graphics adapter's name and API (`Adreno (TM) 640`, `Vulkan`).
    pub gpu: String,
    pub backend: String,
    pub cores: u32,
    /// Memory, in whole GB rounded to a power of two, where it's known.
    pub ram_gb: Option<u32>,
}

/// The settings frames cost depends on most.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PerfSettings {
    pub mobile_layout: bool,
    pub visualizer_scale: u32,
    /// The window's size, in physical pixels, rounded to quarter
    /// megapixels (thousands of pixels: 250, 500, ...).
    pub window_kpx: u32,
}

/// Frames counted with one hardware and settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PerfGroup {
    pub hardware: Hardware,
    pub settings: PerfSettings,
    /// Frames per bucket of `FRAME_BUCKETS_US`, and one more for slower.
    pub frames: Vec<u64>,
    pub sections: Vec<SectionStat>,
}

/// One part of the frame (`frame_timing`'s sections): its time over the
/// frames it ran in, and its worst.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SectionStat {
    pub name: String,
    pub total_us: u64,
    pub frames: u64,
    pub worst_us: u64,
}

/// The bucket a frame of `us` microseconds goes in.
pub fn frame_bucket(us: u64) -> usize {
    FRAME_BUCKETS_US.iter().position(|&end| us < end).unwrap_or(FRAME_BUCKETS_US.len())
}

/// Frame times as the admin sees them (`AdminAction::UsagePerf`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PerfRow {
    pub version: String,
    pub platform: String,
    #[serde(flatten)]
    pub group: PerfGroup,
}

/// A kind of log line and how many times it came up.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageLine {
    /// One of `USAGE_LEVELS`.
    pub level: String,
    /// The log target (`synththing::library::sync`, `wgpu_core::device`),
    /// or the thread, for a panic.
    pub target: String,
    /// Where in the code it was logged, and its text when that's fixed:
    /// never anything the message was filled in with (`crate::usage`).
    pub signature: String,
    pub count: u64,
}

/// One level of the server's tree (`AdminAction::Usage`): a version,
/// platform, level, target or signature, with the counts under it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UsageNode {
    pub name: String,
    pub total: u64,
    /// Since intake last took them.
    pub new: u64,
    /// App starts, for the version and platform levels.
    pub sessions: Option<u64>,
}

/// A rule: `<level> <target> keep|drop`. The level is one of
/// `USAGE_LEVELS` or `*`; the target is a log target, a prefix ending in
/// `*` (`synththing::library::*`), or `*` for all. The first rule a line
/// matches decides it; a line no rule matches is dropped. Panics are
/// always kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageRule {
    level: String,
    target: String,
    keep: bool,
}

impl UsageRule {
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let [level, target, action] = parts[..] else {
            return Err(format!("\"{text}\": a rule is <level> <target> keep|drop"));
        };
        if level != "*" && !USAGE_LEVELS.contains(&level) {
            return Err(format!("\"{text}\": the level is one of {} or *", USAGE_LEVELS.join(", ")));
        }
        let keep = match action {
            "keep" => true,
            "drop" => false,
            _ => return Err(format!("\"{text}\": the last word is keep or drop")),
        };
        Ok(Self { level: level.to_string(), target: target.to_string(), keep })
    }

    fn matches(&self, line: &UsageLine) -> bool {
        let level = self.level == "*" || self.level == line.level;
        let target = match self.target.strip_suffix('*') {
            Some(prefix) => line.target.starts_with(prefix),
            None => self.target == line.target,
        };
        level && target
    }
}

/// Parse rules, all or none.
pub fn parse_rules(rules: &[String]) -> Result<Vec<UsageRule>, String> {
    rules.iter().map(|rule| UsageRule::parse(rule)).collect()
}

/// Whether `rules` keep `line`.
pub fn kept(rules: &[UsageRule], line: &UsageLine) -> bool {
    line.level == "panic" || rules.iter().find(|rule| rule.matches(line)).is_some_and(|rule| rule.keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(level: &str, target: &str) -> UsageLine {
        UsageLine { level: level.into(), target: target.into(), signature: "x:1".into(), count: 1 }
    }

    #[test]
    fn the_first_matching_rule_decides() {
        let rules = parse_rules(&[
            "info synththing::library::* keep".to_string(),
            "info * drop".to_string(),
            "* wgpu_core drop".to_string(),
            "warn * keep".to_string(),
        ])
        .unwrap();
        assert!(kept(&rules, &line("info", "synththing::library::sync")));
        assert!(!kept(&rules, &line("info", "synththing::app")));
        assert!(!kept(&rules, &line("warn", "wgpu_core")));
        assert!(kept(&rules, &line("warn", "wgpu_core::device")));
        assert!(!kept(&rules, &line("error", "synththing::app")), "no rule matches: dropped");
        assert!(kept(&rules, &line("panic", "main")), "panics are always kept");
    }

    #[test]
    fn bad_rules_are_refused() {
        assert!(UsageRule::parse("debug * keep").is_err());
        assert!(UsageRule::parse("warn * maybe").is_err());
        assert!(UsageRule::parse("warn keep").is_err());
        assert!(parse_rules(&DEFAULT_USAGE_RULES.map(String::from)).is_ok());
    }
}
