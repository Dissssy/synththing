//! Adding the desktop app to the applications menu on Linux: a desktop entry
//! (`~/.local/share/applications/synththing.desktop`, where the
//! freedesktop.org spec keeps one user's apps) starting this binary where it
//! is, and the icon in the user's hicolor theme. The binary isn't moved: it
//! keeps updating itself in place (`updater.rs`), so the entry stays right.
//! Moved or deleted, the entry's `TryExec` no longer finds it and menus
//! hide it, until it's added again from the new place.
//!
//! The Windows counterpart, which does more, is `install.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::BaseDirs;

/// The icon (`assets/synththing-icon.svg` in the build's color, drawn by
/// build.rs).
const ICON_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon_256.png"));

/// `$XDG_DATA_HOME`, or `~/.local/share`.
fn data_dir() -> Result<PathBuf> {
    Ok(BaseDirs::new().context("could not find this user's folders")?.data_dir().to_path_buf())
}

/// The desktop entry. Named for the window's app id (`app_name` in
/// `desktop.rs`), so docks and task bars match the window to it.
pub fn path() -> Result<PathBuf> {
    Ok(data_dir()?.join("applications").join("synththing.desktop"))
}

fn icon_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("icons/hicolor/256x256/apps/synththing.png"))
}

/// The entry is there and starts this binary.
pub fn is_added() -> bool {
    let (Ok(me), Ok(path)) = (std::env::current_exe(), path()) else { return false };
    let (Some(me), Ok(text)) = (me.to_str(), fs::read_to_string(path)) else { return false };
    let wanted = format!("TryExec={}", escape_value(me));
    text.lines().any(|line| line == wanted)
}

/// It can be added (or pointed here again): not a branch build (which
/// lives in a folder of its own, `channels.rs`), and not a test run with its
/// own config folder (whose entry would open the real one).
pub fn can_add() -> bool {
    !matches!(crate::build_info::channel(), crate::build_info::Channel::Branch(_))
        && crate::config::config_dir_override().is_none()
}

/// Write the icon and the entry, starting this binary (replacing an entry
/// for a copy somewhere else).
pub fn add() -> Result<()> {
    let me = std::env::current_exe().context("finding this program")?;
    let exe = me.to_str().with_context(|| format!("{} isn't a path a menu entry can hold", me.display()))?;
    write(&icon_path()?, ICON_PNG).context("adding the icon")?;
    write(&path()?, entry(exe).as_bytes()).context("adding the menu entry")?;
    log::info!("added to the applications menu, starting {exe}");
    Ok(())
}

/// Take the entry and the icon away again.
pub fn remove() -> Result<()> {
    for path in [path()?, icon_path()?] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
    log::info!("removed from the applications menu");
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// The entry starting `exe`.
fn entry(exe: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=synththing\n\
         GenericName=Music player\n\
         Comment=Play MIDI files and audio, with visualizers\n\
         Exec={}\n\
         TryExec={}\n\
         Icon=synththing\n\
         Terminal=false\n\
         Categories=AudioVideo;Audio;Player;\n\
         StartupWMClass=synththing\n",
        escape_value(&exec_quote(exe)),
        escape_value(exe),
    )
}

/// `Exec`'s quoting: in double quotes, with `"`, `` ` ``, `$` and `\`
/// escaped, and `%` (a field code) doubled.
fn exec_quote(exe: &str) -> String {
    let mut out = String::from("\"");
    for c in exe.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A string value's escapes, which every value has on top of its own.
fn escape_value(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path() {
        let text = entry("/home/me/Apps/synththing");
        assert!(text.contains("\nExec=\"/home/me/Apps/synththing\"\n"));
        assert!(text.contains("\nTryExec=/home/me/Apps/synththing\n"));
    }

    #[test]
    fn awkward_path() {
        // Quoted for Exec, then a backslash doubled again for the file.
        assert_eq!(escape_value(&exec_quote(r#"/a b/$x"100%\"#)), r#""/a b/\\$x\\"100%%\\\\""#);
        assert_eq!(escape_value("/a\\b"), "/a\\\\b");
    }
}
