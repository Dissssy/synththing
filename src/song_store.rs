//! The song library: synththing's own copy of every song, in one flat
//! folder (`config::music_dir`), each file named by a hash of its contents
//! (`<sha256>.<ext>`, its key), so a song is the same song on every device
//! and in every playlist however its original was named or moved.
//! `library.json` beside them says what's known of each: its original file
//! name and when it came in.
//!
//! Getting a file in ([`copy_in`], [`move_in`], [`write_in`]) is plain file
//! work, done off the main thread; [`SongStore::add`] then records it. The
//! files say which songs are here: [`SongStore::open`] adds any file the
//! index lacks (a copy that finished as the app quit). The index also
//! keeps songs that aren't (removed here, or in a playlist synced from
//! another device: [`SongStore::know`]), so they keep their names.
//!
//! A song removed here is blocked (`blocked.json`): this device doesn't
//! fetch it from the others again (`song_sync.rs`), until it's unblocked
//! or imported again.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const INDEX: &str = "library.json";
const BLOCKED: &str = "blocked.json";

/// What's known of a stored song.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Song {
    /// Its original file name, extension included (`Overworld.mid`).
    pub name: String,
    /// When it came in (Unix seconds).
    #[serde(default)]
    pub imported: i64,
}

/// A file now in the store, to [`SongStore::add`].
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub key: String,
    /// The original file name.
    pub name: String,
}

pub struct SongStore {
    dir: PathBuf,
    songs: BTreeMap<String, Song>,
    /// The keys whose files are here.
    present: BTreeSet<String>,
    /// The keys removed here, not to be fetched again.
    blocked: BTreeSet<String>,
}

impl SongStore {
    /// The store in `dir`, as it is on disk.
    pub fn open(dir: PathBuf) -> Self {
        let mut songs: BTreeMap<String, Song> = std::fs::read_to_string(dir.join(INDEX))
            .ok()
            .and_then(|text| serde_json::from_str(&text).map_err(|e| log::warn!("{INDEX}: {e}")).ok())
            .unwrap_or_default();
        let present: BTreeSet<String> = std::fs::read_dir(&dir)
            .map(|entries| entries.flatten().filter_map(|e| e.file_name().to_str().filter(|n| is_key(n)).map(str::to_string)).collect())
            .unwrap_or_default();
        let mut changed = false;
        for key in &present {
            songs.entry(key.clone()).or_insert_with_key(|key| {
                changed = true;
                Song { name: key.clone(), imported: now() }
            });
        }
        let blocked = std::fs::read_to_string(dir.join(BLOCKED))
            .ok()
            .and_then(|text| serde_json::from_str(&text).map_err(|e| log::warn!("{BLOCKED}: {e}")).ok())
            .unwrap_or_default();
        let store = Self { dir, songs, present, blocked };
        if changed {
            store.save();
        }
        store
    }

    /// Whether there's a store in `dir` yet (the one-time import hasn't
    /// run if not).
    pub fn exists(dir: &Path) -> bool {
        dir.join(INDEX).exists()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Record a file now in the store (keeping what's known of it if it
    /// was already there), and save.
    pub fn add(&mut self, stored: Stored) {
        let song = self.songs.entry(stored.key.clone()).or_insert_with(|| Song { name: stored.name.clone(), imported: now() });
        // (Found on disk with nothing known of it: named after its key.)
        if song.name == stored.key {
            song.name = stored.name;
        }
        if self.blocked.remove(&stored.key) {
            self.save_blocked();
        }
        self.present.insert(stored.key);
        self.save();
    }

    /// Note a song that isn't here (yet) by its key and name, unless it's
    /// known already.
    pub fn know(&mut self, key: &str, name: &str) {
        if !is_key(key) || self.songs.contains_key(key) {
            return;
        }
        self.songs.insert(key.to_string(), Song { name: name.to_string(), imported: now() });
        self.save();
    }

    /// Delete the song at `path` from the store, and block it (it stays
    /// known).
    pub fn remove(&mut self, path: &Path) -> Result<()> {
        let Some(key) = self.key_of(path).map(str::to_string) else { return Ok(()) };
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
        }
        self.present.remove(&key);
        self.blocked.insert(key);
        self.save_blocked();
        Ok(())
    }

    /// Fetch the song `key` from the other devices again.
    pub fn unblock(&mut self, key: &str) {
        if self.blocked.remove(key) {
            self.save_blocked();
        }
    }

    /// The songs removed here, by key, with what's known of them.
    pub fn blocked(&self) -> impl Iterator<Item = (&str, Option<&Song>)> {
        self.blocked.iter().map(|key| (key.as_str(), self.songs.get(key)))
    }

    pub fn is_blocked(&self, key: &str) -> bool {
        self.blocked.contains(key)
    }

    /// Whether the song `key`'s file is here.
    pub fn has(&self, key: &str) -> bool {
        self.present.contains(key)
    }

    /// Every song known, here or not: its key and what's known of it.
    pub fn known(&self) -> impl Iterator<Item = (&str, &Song)> {
        self.songs.iter().map(|(key, song)| (key.as_str(), song))
    }

    /// What's known of the song at `path` (here or not), if it's one of
    /// the store's.
    pub fn get(&self, path: &Path) -> Option<&Song> {
        self.songs.get(self.key_of(path)?)
    }

    /// Every song here: its file and what's known of it.
    pub fn songs(&self) -> impl Iterator<Item = (PathBuf, &Song)> {
        self.present.iter().filter_map(|key| Some((self.dir.join(key), self.songs.get(key)?)))
    }

    /// The key of the song at `path`, if it's in this store.
    fn key_of<'a>(&self, path: &'a Path) -> Option<&'a str> {
        (path.parent() == Some(self.dir.as_path())).then_some(())?;
        path.file_name()?.to_str().filter(|n| is_key(n))
    }

    /// Write `library.json` (even with nothing in it: the one-time import
    /// has run).
    fn save(&self) {
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|()| write_atomically(&self.dir.join(INDEX), serde_json::to_string_pretty(&self.songs).unwrap_or_default().as_bytes()));
        if let Err(e) = written {
            log::warn!("couldn't save {}: {e}", self.dir.join(INDEX).display());
        }
    }

    fn save_blocked(&self) {
        let path = self.dir.join(BLOCKED);
        if let Err(e) = write_atomically(&path, serde_json::to_string(&self.blocked).unwrap_or_default().as_bytes()) {
            log::warn!("couldn't save {}: {e}", path.display());
        }
    }
}

/// Whether `name` is a stored song's file name: 64 hex digits, a dot, an
/// extension.
pub fn is_key(name: &str) -> bool {
    name.split_once('.').is_some_and(|(hash, ext)| {
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) && !ext.is_empty()
    })
}

/// The key for contents hashed to `hash`, from a file called `name`.
fn key_for(hash: &[u8], name: &str) -> String {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "bin".to_string());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex}.{ext}")
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Copy `file` into the store in `dir`. (The same contents already there
/// aren't copied again.)
pub fn copy_in(file: &Path, dir: &Path) -> Result<Stored> {
    let name = file_name(file);
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut from = std::fs::File::open(file).with_context(|| format!("reading {}", file.display()))?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = dir.join(format!(".importing-{}-{n}", std::process::id()));
    let copied = (|| -> std::io::Result<Vec<u8>> {
        let mut to = std::fs::File::create(&temp)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1 << 16];
        loop {
            let n = from.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            to.write_all(&buffer[..n])?;
        }
        to.sync_all()?;
        Ok(hasher.finalize().to_vec())
    })();
    let hash = match copied {
        Ok(hash) => hash,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e).with_context(|| format!("copying {}", file.display()));
        }
    };
    let key = key_for(&hash, &name);
    let to = dir.join(&key);
    if to.exists() {
        let _ = std::fs::remove_file(&temp);
    } else {
        std::fs::rename(&temp, &to).with_context(|| format!("writing {}", to.display()))?;
    }
    Ok(Stored { key, name })
}

/// Move `file`, already in the store's folder `dir` under its own name (a
/// copy Android made), into the store.
pub fn move_in(file: &Path, dir: &Path) -> Result<Stored> {
    let name = file_name(file);
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let key = key_for(&Sha256::digest(&bytes), &name);
    let to = dir.join(&key);
    if to.exists() {
        std::fs::remove_file(file).with_context(|| format!("removing {}", file.display()))?;
    } else {
        std::fs::rename(file, &to).with_context(|| format!("writing {}", to.display()))?;
    }
    Ok(Stored { key, name })
}

/// Write `bytes`, a song called `name`, into the store in `dir`.
pub fn write_in(bytes: &[u8], name: &str, dir: &Path) -> Result<Stored> {
    let key = key_for(&Sha256::digest(bytes), name);
    let to = dir.join(&key);
    if !to.exists() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        write_atomically(&to, bytes).with_context(|| format!("writing {}", to.display()))?;
    }
    Ok(Stored { key, name: name.to_string() })
}

/// Write `bytes`, fetched as the song `key`, into the store in `dir`, if
/// they are that song.
pub fn write_as(bytes: &[u8], key: &str, dir: &Path) -> Result<()> {
    let (hash, _) = key.split_once('.').filter(|_| is_key(key)).context("that's not a song's key")?;
    let hex: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    anyhow::ensure!(hex == hash, "the song didn't arrive whole");
    let to = dir.join(key);
    if !to.exists() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        write_atomically(&to, bytes).with_context(|| format!("writing {}", to.display()))?;
    }
    Ok(())
}

/// What the one-time import ([`import_old`]) did.
#[derive(Debug, Default)]
pub struct OldImport {
    /// Each song brought in: where it was, and where it is now.
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// Playlist entries whose songs weren't there any more.
    pub dropped: usize,
    pub failed: Vec<String>,
}

/// The one-time import, from before the library: every song (with one of
/// `extensions`) under `folders` into the store in `dir`, with where it
/// came from if it was downloaded (its `.song.json`), then the playlists
/// in `playlists` changed from naming files to naming songs in the store
/// (also bringing in songs from elsewhere that are still there), and last
/// `library.json` written, which says it's done. Songs already in `dir`'s
/// folder (Android's imports) are moved; the rest are copied, their
/// folders left as they were.
pub fn import_old(folders: &[PathBuf], dir: &Path, playlists: &Path, extensions: &[&str]) -> OldImport {
    let mut done = OldImport::default();
    let mut songs: Vec<Stored> = Vec::new();
    let mut keys: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut bring_in = |file: &Path, done: &mut OldImport, songs: &mut Vec<Stored>| -> Option<String> {
        if let Some(key) = keys.get(file) {
            return Some(key.clone());
        }
        let stored = if file.starts_with(dir) { move_in(file, dir) } else { copy_in(file, dir) };
        match stored {
            Ok(stored) => {
                keys.insert(file.to_path_buf(), stored.key.clone());
                done.moved.push((file.to_path_buf(), dir.join(&stored.key)));
                let key = stored.key.clone();
                songs.push(stored);
                Some(key)
            }
            Err(e) => {
                done.failed.push(format!("{e:#}"));
                None
            }
        }
    };
    for folder in folders {
        for file in songs_under(folder, extensions) {
            // (Already the store's.)
            if file.parent() == Some(dir) && file.file_name().and_then(|n| n.to_str()).is_some_and(is_key) {
                continue;
            }
            bring_in(&file, &mut done, &mut songs);
        }
    }
    // Android's song folders, now empty.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            let _ = std::fs::remove_dir(entry.path());
        }
    }
    let playlist_files: Vec<PathBuf> = std::fs::read_dir(playlists)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect())
        .unwrap_or_default();
    for file in playlist_files {
        let Ok(mut playlist) = std::fs::read_to_string(&file).map_err(|_| ()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|_| ())) else {
            continue;
        };
        let Some(entries) = playlist.get_mut("entries").and_then(|e| e.as_array_mut()) else { continue };
        let mut changed = false;
        entries.retain_mut(|entry| {
            let Some(path) = entry.get("path").and_then(|p| p.as_str()).map(PathBuf::from) else { return true };
            changed = true;
            let key = path.exists().then(|| bring_in(&path, &mut done, &mut songs)).flatten();
            match (key, entry.as_object_mut()) {
                (Some(key), Some(fields)) => {
                    fields.remove("path");
                    fields.insert("song".into(), key.into());
                    true
                }
                _ => {
                    done.dropped += 1;
                    false
                }
            }
        });
        if changed && let Err(e) = write_atomically(&file, serde_json::to_string_pretty(&playlist).unwrap_or_default().as_bytes()) {
            done.failed.push(format!("{}: {e}", file.display()));
        }
    }
    let mut store = SongStore::open(dir.to_path_buf());
    for stored in songs {
        store.add(stored);
    }
    store.save();
    done
}

/// Every file under `dir` (subfolders too) with one of `extensions`.
pub fn songs_under(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                pending.push(path);
            } else if has_extension(&path, extensions) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("synththing-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_same_song_is_stored_once() {
        let dir = temp_dir("once");
        let store_dir = dir.join("store");
        std::fs::write(dir.join("A.MID"), b"song").unwrap();
        std::fs::write(dir.join("b.mid"), b"song").unwrap();
        let a = copy_in(&dir.join("A.MID"), &store_dir).unwrap();
        let b = copy_in(&dir.join("b.mid"), &store_dir).unwrap();
        assert_eq!(a.key, b.key);
        assert!(a.key.ends_with(".mid"), "{}", a.key);
        assert_eq!(a.name, "A.MID");
        assert_eq!(write_in(b"song", "c.mid", &store_dir).unwrap().key, a.key);

        let mut store = SongStore::open(store_dir.clone());
        store.add(a.clone());
        store.add(b);
        assert_eq!(store.songs().count(), 1);
        let path = store_dir.join(&a.key);
        assert_eq!(store.get(&path).unwrap().name, "A.MID");
        assert_eq!(std::fs::read(&path).unwrap(), b"song");
        // No leftovers.
        assert_eq!(std::fs::read_dir(&store_dir).unwrap().count(), 2);
    }

    #[test]
    fn the_files_say_whats_here() {
        let dir = temp_dir("truth");
        let kept = write_in(b"kept", "kept.mid", &dir).unwrap();
        let gone = write_in(b"gone", "gone.mid", &dir).unwrap();
        let mut store = SongStore::open(dir.clone());
        store.add(kept.clone());
        store.add(gone.clone());
        std::fs::remove_file(dir.join(&gone.key)).unwrap();
        // Written as the app quit, never recorded.
        let unrecorded = write_in(b"new", "new.ogg", &dir).unwrap();
        std::fs::write(dir.join("not a song.txt"), b"").unwrap();

        let mut store = SongStore::open(dir.clone());
        let keys: Vec<String> = store.songs().map(|(p, _)| file_name(&p)).collect();
        let mut expected = vec![kept.key.clone(), unrecorded.key.clone()];
        expected.sort();
        assert_eq!(keys, expected);
        assert_eq!(store.get(&dir.join(&kept.key)).unwrap().name, "kept.mid");
        // Gone from here, still known by name.
        assert_eq!(store.get(&dir.join(&gone.key)).unwrap().name, "gone.mid");
        // Known from elsewhere: named, not here.
        let elsewhere = format!("{}.mid", "a".repeat(64));
        store.know(&elsewhere, "Elsewhere.mid");
        store.know("../escape", "nope");
        let store = SongStore::open(dir.clone());
        assert_eq!(store.get(&dir.join(&elsewhere)).unwrap().name, "Elsewhere.mid");
        assert!(store.get(&dir.join("../escape")).is_none());
        assert_eq!(store.songs().count(), 2);
    }

    #[test]
    fn moving_in_and_removing() {
        let dir = temp_dir("move");
        std::fs::write(dir.join("Picked.mid"), b"picked").unwrap();
        let stored = move_in(&dir.join("Picked.mid"), &dir).unwrap();
        assert!(!dir.join("Picked.mid").exists());
        let mut store = SongStore::open(dir.clone());
        store.add(stored.clone());
        let path = dir.join(&stored.key);
        assert_eq!(store.get(&path).unwrap().name, "Picked.mid");
        // Not one of its files.
        assert!(store.get(&dir.join("elsewhere").join(&stored.key)).is_none());
        store.remove(&path).unwrap();
        assert!(!path.exists());
        let mut store = SongStore::open(dir.clone());
        assert_eq!(store.songs().count(), 0);
        assert!(store.is_blocked(&stored.key), "removed here: not fetched again");
        store.unblock(&stored.key);
        assert!(!SongStore::open(dir.clone()).is_blocked(&stored.key));
        store.remove(&path).unwrap();
        std::fs::write(dir.join("Picked.mid"), b"picked").unwrap();
        store.add(move_in(&dir.join("Picked.mid"), &dir).unwrap());
        assert!(!store.is_blocked(&stored.key), "imported again");
    }

    #[test]
    fn importing_the_old_songs_and_playlists() {
        let root = temp_dir("old");
        let (songs, store_dir, playlists) = (root.join("Songs"), root.join("store"), root.join("playlists"));
        std::fs::create_dir_all(songs.join("Library")).unwrap();
        std::fs::create_dir_all(&playlists).unwrap();
        std::fs::write(songs.join("a.mid"), b"a").unwrap();
        std::fs::write(songs.join("notes.txt"), b"").unwrap();
        let downloaded = songs.join("Library").join("b.mid");
        std::fs::write(&downloaded, b"b").unwrap();
        let elsewhere = root.join("elsewhere.ogg");
        std::fs::write(&elsewhere, b"c").unwrap();
        let entries = serde_json::json!([
            { "path": songs.join("a.mid"), "soundfont": "/sf.sf2" },
            { "path": elsewhere },
            { "path": root.join("gone.mid") },
        ]);
        std::fs::write(playlists.join("p.json"), serde_json::json!({ "name": "P", "entries": entries }).to_string()).unwrap();

        let done = import_old(std::slice::from_ref(&songs), &store_dir, &playlists, &["mid", "ogg"]);
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert_eq!(done.moved.len(), 3);
        assert_eq!(done.dropped, 1);
        let store = SongStore::open(store_dir.clone());
        let (_, b) = done.moved.iter().find(|(old, _)| *old == downloaded).unwrap();
        assert_eq!(store.get(b).unwrap().name, "b.mid");
        assert!(downloaded.exists(), "the old folder is left as it was");

        let (library, errors) = crate::playlist::Library::load(playlists.clone(), store_dir.clone());
        assert!(errors.is_empty(), "{errors:?}");
        let entries = &library.lists[0].playlist.entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(std::fs::read(&entries[0].path).unwrap(), b"a");
        assert_eq!(entries[0].soundfont.as_deref(), Some(Path::new("/sf.sf2")));
        assert_eq!(std::fs::read(&entries[1].path).unwrap(), b"c");
        assert!(entries.iter().all(|e| !e.missing));
    }
}
