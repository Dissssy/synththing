//! The changelog: `CHANGELOG.md`, embedded and parsed into versions for
//! the What's new window (after an update) and Help > Changelog....
//!
//! The file's format, kept simple on purpose: `## 0.2.2 (2026-10-03)` (or
//! `## Unreleased`) starts a version, `### New` a group within it, `- `
//! a bullet (wrapped lines carry on), anything else a paragraph. Text
//! before the first version is the file's own introduction, not shown.
//! `cargo release` turns `## Unreleased` into the new version's heading
//! (release.toml), and the release workflow publishes that version's
//! section as the GitHub release notes.

use std::sync::OnceLock;

const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// A version number, comparable (`(0, 2, 2)`).
pub type Version = (u32, u32, u32);

pub struct Release {
    /// The heading as written: "0.2.2 (2026-10-03)", "Unreleased".
    pub heading: String,
    /// `None` for Unreleased.
    pub version: Option<Version>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, PartialEq)]
pub enum Block {
    /// A group: "New", "Changed", "Fixed".
    Heading(String),
    /// May have `inline code`.
    Bullet(String),
    Paragraph(String),
}

/// "0.2.2" as a `Version`.
pub fn parse_version(text: &str) -> Option<Version> {
    let mut parts = text.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// "0.2.2".
pub fn format_version((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

/// This build's version.
pub fn current_version() -> Option<Version> {
    parse_version(env!("CARGO_PKG_VERSION"))
}

/// Every version in the changelog with something in it, newest first.
pub fn releases() -> &'static [Release] {
    static RELEASES: OnceLock<Vec<Release>> = OnceLock::new();
    RELEASES.get_or_init(|| parse(CHANGELOG))
}

/// The versions newer than `since`, up to this build's (Unreleased
/// changes aren't "new" to anyone yet).
pub fn newer_than(since: Version) -> impl Iterator<Item = &'static Release> {
    let current = current_version();
    releases().iter().filter(move |r| r.version.is_some_and(|v| v > since && current.is_none_or(|c| v <= c)))
}

/// The newest version in the changelog older than `version`.
pub fn previous_version(version: Version) -> Option<Version> {
    releases().iter().filter_map(|r| r.version).find(|&v| v < version)
}

fn parse(text: &str) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    // The paragraph or bullet being built, to join wrapped lines into.
    let mut open = false;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(heading) = line.strip_prefix("## ") {
            let heading = heading.trim().to_string();
            let version = heading.split_whitespace().next().and_then(parse_version);
            releases.push(Release { heading, version, blocks: Vec::new() });
            open = false;
            continue;
        }
        let Some(release) = releases.last_mut() else { continue };
        if line.trim().is_empty() {
            open = false;
        } else if let Some(group) = line.strip_prefix("### ") {
            release.blocks.push(Block::Heading(group.trim().to_string()));
            open = false;
        } else if let Some(bullet) = line.strip_prefix("- ") {
            release.blocks.push(Block::Bullet(bullet.trim().to_string()));
            open = true;
        } else if open && let Some(Block::Bullet(text) | Block::Paragraph(text)) = release.blocks.last_mut() {
            text.push(' ');
            text.push_str(line.trim());
        } else {
            release.blocks.push(Block::Paragraph(line.trim().to_string()));
            open = true;
        }
    }
    releases.retain(|r| !r.blocks.is_empty());
    releases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changelog_parses_and_covers_this_version() {
        let current = current_version().unwrap();
        assert!(
            releases().iter().any(|r| r.version == Some(current)),
            "CHANGELOG.md has no section for {}",
            env!("CARGO_PKG_VERSION")
        );
        // Newest first, each version once, every one dated.
        let versions: Vec<Version> = releases().iter().filter_map(|r| r.version).collect();
        assert!(versions.windows(2).all(|w| w[0] > w[1]), "{versions:?}");
        for release in releases() {
            if release.version.is_some() {
                assert!(release.heading.contains("(20"), "no date: {}", release.heading);
            } else {
                assert_eq!(release.heading, "Unreleased");
            }
            for block in &release.blocks {
                if let Block::Bullet(text) | Block::Paragraph(text) = block {
                    assert_eq!(text.matches('`').count() % 2, 0, "unbalanced backticks: {text}");
                    assert!(!text.contains('\u{2014}'), "an em dash: {text}");
                }
            }
        }
        assert!(CHANGELOG.contains("## Unreleased"), "cargo release needs the Unreleased heading to rename");
    }

    #[test]
    fn parsing() {
        let text = "# Changelog\n\nIntro.\n\n## Unreleased\n\n## 1.2.0 (2026-01-02)\n\n### New\n- One\n  wrapped\n- Two `code`\n\n## 1.1.0 (2026-01-01)\n\nFirst.\n";
        let releases = parse(text);
        assert_eq!(releases.len(), 2, "the empty Unreleased is dropped");
        assert_eq!(releases[0].version, Some((1, 2, 0)));
        assert_eq!(
            releases[0].blocks,
            [Block::Heading("New".into()), Block::Bullet("One wrapped".into()), Block::Bullet("Two `code`".into())]
        );
        assert_eq!(releases[1].blocks, [Block::Paragraph("First.".into())]);
        assert_eq!(parse_version("v0.2.10"), Some((0, 2, 10)));
        assert_eq!(parse_version("0.2"), None);
        assert_eq!(parse_version("Unreleased"), None);
        assert_eq!(format_version((0, 2, 2)), "0.2.2");
    }

    #[test]
    fn whats_new_lists_the_versions_since() {
        let current = current_version().unwrap();
        let previous = previous_version(current).unwrap();
        assert!(previous < current);
        let shown: Vec<Option<Version>> = newer_than(previous).map(|r| r.version).collect();
        assert_eq!(shown, [Some(current)]);
        assert_eq!(newer_than(current).count(), 0);
        assert!(newer_than((0, 0, 0)).count() >= 12);
    }
}
