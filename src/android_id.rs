//! The Android app's id and launcher name for each channel (docs/ANDROID.md).
//! build.rs includes this file too, to hand them to Gradle, so the app
//! and the APK it installs agree on them.
//!
//! Releases and the nightly are one app, `nl.p51.synththing`: canary
//! updates it. Each branch build is an app of its own beside it
//! (`nl.p51.synththing.branch.b_<name>`, "synththing <name>"), with its own
//! data, so trying a branch never touches the usual app and switching
//! between branches is never a downgrade, which Android refuses.

/// The usual app's id.
pub const BASE: &str = "nl.p51.synththing";

/// A channel's (`stable`, `nightly`, `branch-<name>`) application id. A
/// branch's name (already only `A-Za-z0-9._-`, test-builds.yml) is made
/// one segment of letters, digits and `_`, after `b_` so it always starts
/// with a letter.
pub fn application_id(channel: &str) -> String {
    match channel.strip_prefix("branch-") {
        Some(name) => {
            let segment: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
            format!("{BASE}.branch.b_{segment}")
        }
        None => BASE.to_string(),
    }
}

/// A channel's name in the launcher: "synththing", or "synththing <name>"
/// for a branch. The app reads the branch back from it (`channel`).
pub fn label(channel: &str) -> String {
    match channel.strip_prefix("branch-") {
        Some(name) => format!("synththing {name}"),
        None => "synththing".to_string(),
    }
}

/// The branch channel an installed app is (`branch-<name>`), from its id
/// and launcher name; `None` for anything else.
pub fn channel(application_id: &str, label: &str) -> Option<String> {
    let channel = format!("branch-{}", label.strip_prefix("synththing ")?);
    (self::application_id(&channel) == application_id).then_some(channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_is_an_app_of_its_own() {
        assert_eq!(application_id("stable"), "nl.p51.synththing");
        assert_eq!(application_id("nightly"), "nl.p51.synththing");
        assert_eq!(application_id("branch-claude-android-keyboard"), "nl.p51.synththing.branch.b_claude_android_keyboard");
        assert_eq!(application_id("branch-Mike.2"), "nl.p51.synththing.branch.b_mike_2");
        assert_eq!(label("branch-mike.2"), "synththing mike.2");
    }

    #[test]
    fn an_installed_branch_app_is_found_again() {
        let channel = "branch-claude-android-keyboard";
        assert_eq!(super::channel(&application_id(channel), &label(channel)).as_deref(), Some(channel));
        assert_eq!(super::channel(BASE, "synththing"), None);
        assert_eq!(super::channel("nl.p51.synththing.branch.b_x.test", "synththing x test"), None);
    }
}
