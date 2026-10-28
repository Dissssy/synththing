//! How much space synththing's own files take (Preferences > Library >
//! Storage): the song library, the soundfonts, and everything else in the
//! config folder. On Android that's all of the app's storage.

use std::path::Path;

use anyhow::Result;

/// The bytes and files in a folder (and its folders).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub bytes: u64,
    pub files: u64,
}

impl std::ops::AddAssign for Size {
    fn add_assign(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.files += other.files;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    /// The song library (`config::music_dir`).
    pub songs: Size,
    /// The soundfonts (`starter::soundfonts_dir`): imported ones on
    /// Android, the starter pack's downloads on the desktop.
    pub soundfonts: Size,
    /// The rest of the config folder: settings, playlists, scripts, the
    /// log, sync state.
    pub other: Size,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.songs.bytes + self.soundfonts.bytes + self.other.bytes
    }
}

/// Add up the folders (reads every file's size, so off the UI thread).
pub fn measure() -> Result<Usage> {
    let config = crate::config::config_dir()?;
    let music = crate::config::music_dir()?;
    let soundfonts = crate::starter::soundfonts_dir()?;
    // (Branch builds are apps of their own, with their own storage.)
    let channels = config.join("channels");
    Ok(Usage {
        songs: folder_size(&music, &[]),
        soundfonts: folder_size(&soundfonts, &[]),
        other: folder_size(&config, &[&music, &soundfonts, &channels]),
    })
}

/// What's in `dir`, leaving out the folders `skip` (missing: nothing).
/// Symlinks count as themselves, not what they point to.
fn folder_size(dir: &Path, skip: &[&Path]) -> Size {
    let mut size = Size::default();
    let Ok(entries) = std::fs::read_dir(dir) else { return size };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if !skip.contains(&path.as_path()) {
                size += folder_size(&path, skip);
            }
        } else {
            size += Size { bytes: meta.len(), files: 1 };
        }
    }
    size
}

/// A size to read ("160 MB").
pub fn size_text(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else if bytes == 0.0 {
        "0 KB".to_string()
    } else {
        format!("{:.0} KB", (bytes / KB).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_add_up_and_skip() {
        let root = std::env::temp_dir().join(format!("synththing-storage-{}", std::process::id()));
        let music = root.join("music");
        std::fs::create_dir_all(music.join("deep")).unwrap();
        std::fs::write(root.join("config.json"), [0u8; 10]).unwrap();
        std::fs::write(music.join("a.mid"), [0u8; 100]).unwrap();
        std::fs::write(music.join("deep").join("b.mid"), [0u8; 1000]).unwrap();
        assert_eq!(folder_size(&root, &[]), Size { bytes: 1110, files: 3 });
        assert_eq!(folder_size(&root, &[&music]), Size { bytes: 10, files: 1 });
        assert_eq!(folder_size(&music, &[]), Size { bytes: 1100, files: 2 });
        assert_eq!(folder_size(&root.join("missing"), &[]), Size::default());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_text(0), "0 KB");
        assert_eq!(size_text(10), "1 KB");
        assert_eq!(size_text(160 * 1024 * 1024), "160 MB");
        assert_eq!(size_text(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
