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
