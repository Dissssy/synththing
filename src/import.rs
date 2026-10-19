//! Importing: copying songs and soundfonts the user picks into synththing's
//! own folders, for the mobile layout, where the Songs page is a library of
//! what's been imported rather than a browser of the whole disk (a phone
//! has no disk to browse, only files apps hand over).
//!
//! [`start`] opens the platform's picker without waiting for it: on the
//! desktop the file dialog, on another thread; on Android the system's
//! document picker (`android.rs`), which copies the picked files itself.
//! Either way the copies arrive later through [`take_finished`], which
//! the app polls.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use eframe::egui;

/// What's being imported, and so where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Songs,
    Soundfonts,
}

impl Kind {
    /// The folder imports of this kind are copied into.
    pub fn dir(self) -> Result<PathBuf> {
        match self {
            Self::Songs => crate::config::music_dir(),
            Self::Soundfonts => crate::starter::soundfonts_dir(),
        }
    }

    /// Its number, as the Android side passes it back.
    #[cfg(target_os = "android")]
    pub fn code(self) -> i32 {
        match self {
            Self::Songs => 0,
            Self::Soundfonts => 1,
        }
    }

    #[cfg(target_os = "android")]
    pub fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Songs),
            1 => Some(Self::Soundfonts),
            _ => None,
        }
    }
}

/// An import that's done: the files now in synththing's folder (empty if
/// the picker was cancelled), or why it failed.
pub struct Finished {
    pub kind: Kind,
    pub result: Result<Vec<PathBuf>, String>,
}

static FINISHED: Mutex<Vec<Finished>> = Mutex::new(Vec::new());
static WAKER: Mutex<Option<egui::Context>> = Mutex::new(None);

/// Hand over a finished import, waking the app to pick it up.
pub fn finish(done: Finished) {
    FINISHED.lock().unwrap().push(done);
    if let Some(ctx) = WAKER.lock().unwrap().as_ref() {
        ctx.request_repaint();
    }
}

/// The imports finished since the last call.
pub fn take_finished() -> Vec<Finished> {
    std::mem::take(&mut *FINISHED.lock().unwrap())
}

/// Who to wake when an import finishes: the app's context. (Once is
/// enough.)
pub fn set_waker(ctx: &egui::Context) {
    let mut waker = WAKER.lock().unwrap();
    if waker.is_none() {
        *waker = Some(ctx.clone());
    }
}

/// Ask the user for files of `kind` (with one of `extensions`) to import.
pub fn start(kind: Kind, extensions: &[&str]) {
    let dir = match kind.dir() {
        Ok(dir) => dir,
        Err(e) => return finish(Finished { kind, result: Err(format!("{e:#}")) }),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return finish(Finished { kind, result: Err(format!("couldn't make {}: {e}", dir.display())) });
    }
    #[cfg(target_os = "android")]
    crate::android::pick_files(kind, extensions, &dir);
    #[cfg(not(target_os = "android"))]
    {
        let extensions: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().add_filter("Files", &extensions).pick_files().unwrap_or_default();
            let result = picked.iter().map(|path| copy_into(path, &dir)).collect::<Result<Vec<_>>>();
            finish(Finished { kind, result: result.map_err(|e| format!("{e:#}")) });
        });
    }
}

/// Copy `file` into `dir`, keeping its name; a different file already
/// there by that name gets the copy a number (`song (2).mid`). The same
/// file imported again isn't copied twice.
pub fn copy_into(file: &Path, dir: &Path) -> Result<PathBuf> {
    let name = file.file_name().context("not a file")?;
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = Path::new(name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let size = std::fs::metadata(file).with_context(|| format!("reading {}", file.display()))?.len();
    let mut n = 0;
    loop {
        n += 1;
        let target = if n == 1 { dir.join(name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match std::fs::metadata(&target) {
            Ok(existing) if existing.len() == size && std::fs::read(&target).ok() == std::fs::read(file).ok() => {
                return Ok(target);
            }
            Ok(_) => continue,
            Err(_) => {
                std::fs::copy(file, &target).with_context(|| format!("copying {} into {}", file.display(), dir.display()))?;
                return Ok(target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_keep_names_and_number_clashes() {
        let root = std::env::temp_dir().join(format!("synththing-import-{}", std::process::id()));
        let (from, to) = (root.join("from"), root.join("to"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let song = from.join("song.mid");
        std::fs::write(&song, b"one").unwrap();
        assert_eq!(copy_into(&song, &to).unwrap(), to.join("song.mid"));
        // The same file again: nothing new.
        assert_eq!(copy_into(&song, &to).unwrap(), to.join("song.mid"));
        // Another file by that name: numbered.
        std::fs::write(&song, b"two").unwrap();
        assert_eq!(copy_into(&song, &to).unwrap(), to.join("song (2).mid"));
        assert_eq!(std::fs::read(to.join("song (2).mid")).unwrap(), b"two");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
