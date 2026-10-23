//! Importing: copying songs into the song library (`song_store.rs`), and
//! soundfonts the user picks into synththing's own folder (the mobile
//! layout's way to get them, a phone having no disk to browse, only files
//! apps hand over).
//!
//! [`start`] opens the platform's picker without waiting for it: on the
//! desktop the file dialog, on another thread; on Android the system's
//! document picker (`android.rs`), which copies the picked files itself.
//! [`songs`] imports files already known (dropped on the window, or an
//! .m3u's). Either way the copies arrive later through [`take_finished`],
//! which the app polls, with what to do with them ([`Then`]).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use eframe::egui;

use crate::song_store::{self, Stored};

/// What's being imported, and so where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Songs,
    Soundfonts,
}

impl Kind {
    /// The folder the picker copies imports of this kind into: for songs,
    /// a folder beside the library's files, moved in from there.
    pub fn dir(self) -> Result<PathBuf> {
        match self {
            Self::Songs => Ok(crate::config::music_dir()?.join(".incoming")),
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

/// What to do with imported songs, once they're in the library.
#[derive(Clone, Debug, PartialEq)]
pub enum Then {
    /// Nothing more: they're in the library.
    Keep,
    /// Insert them into playlist `list` at `at`.
    Insert { list: usize, at: usize },
    /// Make a playlist of them called `name`.
    NewPlaylist { name: String },
}

/// An import that's done: what it brought in, or why it failed.
pub struct Finished {
    pub kind: Kind,
    pub then: Then,
    pub result: Result<Imported, String>,
}

/// What an import brought in (all empty if the picker was cancelled).
#[derive(Debug, Default, PartialEq)]
pub struct Imported {
    /// Songs now in the library, in the order asked for.
    pub songs: Vec<ImportedSong>,
    /// Soundfonts now in synththing's folder.
    pub soundfonts: Vec<PathBuf>,
    /// The names of those picked that aren't of a type it takes.
    pub skipped: Vec<String>,
    /// Those that couldn't be brought in, and why.
    pub failed: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct ImportedSong {
    pub stored: Stored,
    /// The soundfont it's played with in the playlist it's going into.
    pub soundfont: Option<PathBuf>,
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
        Err(e) => return finish(Finished { kind, then: Then::Keep, result: Err(format!("{e:#}")) }),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return finish(Finished {
            kind,
            then: Then::Keep,
            result: Err(format!("couldn't make {}: {e}", dir.display())),
        });
    }
    #[cfg(target_os = "android")]
    crate::android::pick_files(kind, extensions, &dir);
    #[cfg(not(target_os = "android"))]
    {
        let extensions: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().add_filter("Files", &extensions).pick_files().unwrap_or_default();
            let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
            let result = match kind {
                Kind::Songs => import_songs(picked.into_iter().map(|p| (p, None)).collect(), &extensions),
                Kind::Soundfonts => import_soundfonts(&picked, &extensions, &dir),
            };
            finish(Finished { kind, then: Then::Keep, result });
        });
    }
}

/// Ask the user for a folder, and import every song (with one of
/// `extensions`) in it and its subfolders.
#[cfg(not(target_os = "android"))]
pub fn start_folder(extensions: &'static [&'static str]) {
    std::thread::spawn(move || {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else { return };
        let files = song_store::songs_under(&folder, extensions);
        let result = import_songs(files.into_iter().map(|p| (p, None)).collect(), extensions);
        finish(Finished { kind: Kind::Songs, then: Then::Keep, result });
    });
}

/// Import `files` (each with its playlist soundfont, if any), then do
/// `then` with them.
pub fn songs(files: Vec<(PathBuf, Option<PathBuf>)>, extensions: &'static [&'static str], then: Then) {
    std::thread::spawn(move || {
        let result = import_songs(files, extensions);
        finish(Finished { kind: Kind::Songs, then, result });
    });
}

/// Copy those of `files` synththing takes (`song_store::takes`, by
/// `extensions`) into the library.
fn import_songs(files: Vec<(PathBuf, Option<PathBuf>)>, extensions: &[&str]) -> Result<Imported, String> {
    let dir = crate::config::music_dir().map_err(|e| format!("{e:#}"))?;
    let mut imported = Imported::default();
    for (file, soundfont) in files {
        if !song_store::takes(&file, extensions) {
            imported.skipped.push(display_name(&file));
            continue;
        }
        match song_store::copy_in(&file, &dir) {
            Ok(stored) => imported.songs.push(ImportedSong { stored, soundfont }),
            Err(e) => imported.failed.push(format!("{e:#}")),
        }
    }
    Ok(imported)
}

/// Copy those of `files` synththing takes (by `extensions`) into `dir`
/// ([`copy_into`]); the rest are skipped.
#[cfg(not(target_os = "android"))]
fn import_soundfonts(files: &[PathBuf], extensions: &[&str], dir: &Path) -> Result<Imported, String> {
    let mut imported = Imported::default();
    for file in files {
        if !song_store::takes(file, extensions) {
            imported.skipped.push(display_name(file));
            continue;
        }
        match copy_into(file, dir) {
            Ok(copy) => imported.soundfonts.push(copy),
            Err(e) => imported.failed.push(format!("{e:#}")),
        }
    }
    Ok(imported)
}

/// Songs Android copied into `Kind::Songs.dir()` (`copied`), moved into
/// the library.
#[cfg(target_os = "android")]
pub fn songs_copied(copied: Vec<PathBuf>) -> Imported {
    let mut imported = Imported::default();
    let dir = match crate::config::music_dir() {
        Ok(dir) => dir,
        Err(e) => {
            imported.failed.push(format!("{e:#}"));
            return imported;
        }
    };
    for file in copied {
        match song_store::move_in(&file, &dir) {
            Ok(stored) => imported.songs.push(ImportedSong { stored, soundfont: None }),
            Err(e) => imported.failed.push(format!("{e:#}")),
        }
    }
    imported
}

fn display_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Copy `file` into `dir`, keeping its name; a different file already
/// there by that name gets the copy a number (`font (2).sf2`). The same
/// file imported again isn't copied twice.
pub fn copy_into(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
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
        let font = from.join("font.sf2");
        std::fs::write(&font, b"one").unwrap();
        assert_eq!(copy_into(&font, &to).unwrap(), to.join("font.sf2"));
        // The same file again: nothing new.
        assert_eq!(copy_into(&font, &to).unwrap(), to.join("font.sf2"));
        // Another file by that name: numbered.
        std::fs::write(&font, b"two").unwrap();
        assert_eq!(copy_into(&font, &to).unwrap(), to.join("font (2).sf2"));
        assert_eq!(std::fs::read(to.join("font (2).sf2")).unwrap(), b"two");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
