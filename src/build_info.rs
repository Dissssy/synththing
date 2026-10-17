//! Which build this is: its version, channel and commit, stamped by
//! build.rs from what CI sets for a canary or branch build
//! (docs/design/release-channels.md). Built anywhere else, it's a stable
//! build of Cargo.toml's version, with no commit.

/// This build's version. A release's (or a local build's) is Cargo.toml's;
/// a canary or branch build's is the next patch version as a prerelease,
/// numbered by commits since the release (`0.7.1-nightly.12`), so it sorts
/// after the release it's built on and before the next one.
pub const VERSION: &str = env!("SYNTHTHING_VERSION");

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

/// Whether online features (the script and song library) are on: not in
/// branch builds, which are for trying a feature, not for posting to the
/// real servers. A library request from one fails before it's sent
/// (`library::client`).
pub fn online_features() -> bool {
    !matches!(channel(), Channel::Branch(_))
}

/// Said wherever an online feature would be, in a branch build.
pub const OFFLINE_NOTE: &str = "Online features (the script and song library) are off in branch builds.";

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
