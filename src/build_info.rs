//! Which build this is: its version, channel and commit, stamped by
//! build.rs from what CI sets for a canary or branch build
//! (docs/design/release-channels.md). Built anywhere else, it's a stable
//! build of Cargo.toml's version, with no commit.

use std::sync::atomic::{AtomicBool, Ordering};

/// This build's version. A release's (or a local build's) is Cargo.toml's;
/// a canary or branch build's is the next patch version as a prerelease,
/// numbered by commits since the release (`0.7.1-nightly.12`), so it sorts
/// after the release it's built on and before the next one.
pub const VERSION: &str = env!("SYNTHTHING_VERSION");

/// The color its icon and wordmark are drawn in (`#RRGGBB`): which build
/// this is, at a glance (build.rs, `accent`). Off-white for a local build.
pub const ACCENT: &str = env!("SYNTHTHING_ACCENT");

/// The commit it's built from, when CI said.
const COMMIT: &str = env!("SYNTHTHING_COMMIT");

/// Where a build comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// A release, or a build made locally.
    Stable,
    /// Canary: the newest commit on master.
    Nightly,
    /// The newest commit on a branch (its name).
    Branch(&'static str),
}

pub fn channel() -> Channel {
    let channel = env!("SYNTHTHING_CHANNEL");
    match channel.strip_prefix("branch-") {
        Some(name) => Channel::Branch(name),
        None if channel == "nightly" => Channel::Nightly,
        None => Channel::Stable,
    }
}

/// The kinds of online feature. A branch build is for trying a feature,
/// not for talking to the real servers, so each starts off there and a
/// tester turns on the ones the branch is about (`set_online`), for that
/// run only. Feedback and update checks are always on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Online {
    /// The Online Library: finding, installing, publishing and reporting
    /// scripts, and its notifications.
    Scripts,
    /// The identity, sign-in, and syncing settings, playlists and songs.
    Sync,
    /// Album art and details from MusicBrainz, the Cover Art Archive and
    /// the iTunes Store.
    Lookup,
}

impl Online {
    pub const ALL: [Online; 3] = [Self::Scripts, Self::Sync, Self::Lookup];

    pub fn name(self) -> &'static str {
        match self {
            Self::Scripts => "Online Library (scripts)",
            Self::Sync => "Sign-in and sync",
            Self::Lookup => "Album art and details lookup",
        }
    }

    /// Said where it would be, while it's off.
    pub fn off_note(self) -> &'static str {
        match self {
            Self::Scripts => "The Online Library is off in branch builds.",
            Self::Sync => "Sign-in and sync are off in branch builds.",
            Self::Lookup => "Looking albums up is off in branch builds.",
        }
    }
}

/// Which kinds a branch build's tester turned on, by `Online as usize`.
/// Never saved: every run starts with them off.
static TURNED_ON: [AtomicBool; Online::ALL.len()] = [const { AtomicBool::new(false) }; Online::ALL.len()];

/// Whether `kind` is on: always, but in a branch build only once turned on.
/// A library request for one that's off fails before it's sent
/// (`library::client`).
pub fn online(kind: Online) -> bool {
    !matches!(channel(), Channel::Branch(_)) || TURNED_ON[kind as usize].load(Ordering::Relaxed)
}

/// Turn `kind` on or off for this run of a branch build.
pub fn set_online(kind: Online, on: bool) {
    TURNED_ON[kind as usize].store(on, Ordering::Relaxed);
}

/// The commit, shortened as git does, if known.
pub fn short_commit() -> Option<&'static str> {
    (!COMMIT.is_empty()).then(|| &COMMIT[..COMMIT.len().min(7)])
}

/// What isn't in the version: the branch and the commit, for a test build
/// ("youtube, abc1234"); nothing for a stable one.
fn extra() -> Option<String> {
    let parts: Vec<&str> = match channel() {
        Channel::Stable => return None,
        Channel::Nightly => vec!["nightly"],
        Channel::Branch(name) => vec![name],
    };
    Some(parts.into_iter().chain(short_commit()).collect::<Vec<_>>().join(", "))
}

/// The version with the branch and commit, for the About line and the log:
/// "0.7.0", "0.7.1-nightly.12 (nightly, abc1234)".
pub fn describe() -> String {
    match extra() {
        Some(extra) => format!("{VERSION} ({extra})"),
        None => VERSION.to_string(),
    }
}

/// The main window's title: a test build says which it is, so screenshots
/// do too.
pub fn window_title() -> String {
    match extra() {
        Some(extra) => format!("synththing ({extra})"),
        None => "synththing".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_semver() {
        let version = semver::Version::parse(VERSION).unwrap();
        let package = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        assert!(version >= package || !version.pre.is_empty(), "{version} is at least Cargo.toml's");
    }
}
