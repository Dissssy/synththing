//! The song library: synththing's own copy of every song, in one folder
//! (`config::music_dir`), each file named by a hash of its contents
//! (`<sha256>.<ext>`, its key), so a song is the same song on every device
//! and in every playlist however its original was named or moved. Files
//! sit in a subfolder named by their key's first two characters
//! (`0b/0b14…cb7.mp3`, [`path_of`]): one flat folder of a whole library
//! outgrows what some filesystems hold well (FAT32, on SD cards, fits
//! about 9,000 names this long in a folder).
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
//! Songs can carry tags of the user's own (a name and a color, defined
//! once in `tags.json`), by the tags' IDs, and one built in: starred
//! (`STARRED`), never defined and never shown as a tag.
//!
//! Each song keeps what its file says of itself (`Meta`: its tags, length
//! and size), read on the device that has it (`App::note_song_meta`) and
//! synced, so every device can list and sort the songs it doesn't have.
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
const TAGS: &str = "tags.json";

/// The built-in tag starred songs have: hidden from the tag editor and
/// the chips, it syncs like any other.
pub const STARRED: &str = "starred";

/// What's known of a stored song.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Song {
    /// Its original file name, extension included (`Overworld.mid`).
    pub name: String,
    /// When it came in (Unix seconds).
    #[serde(default)]
    pub imported: i64,
    /// Its tags, by ID (`SongStore::tags`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// What its file says of itself, once a device that has it has read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl Song {
    fn new(name: String) -> Self {
        Self { name, imported: now(), tags: Vec::new(), meta: None }
    }
}

/// What a song's file says of itself: its tags (`song_info::Tags`), how
/// long it is and its size. Kept under short names: the synced library has
/// one for every song.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    #[serde(rename = "t", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "ar", skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "al", skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(rename = "aa", skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(rename = "c", skip_serializing_if = "Option::is_none")]
    pub composer: Option<String>,
    #[serde(rename = "g", skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(rename = "n", skip_serializing_if = "Option::is_none")]
    pub track: Option<u32>,
    #[serde(rename = "d", skip_serializing_if = "Option::is_none")]
    pub disc: Option<u32>,
    #[serde(rename = "y", skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// Seconds.
    #[serde(rename = "l", skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    /// Bytes.
    #[serde(rename = "s")]
    pub size: u64,
}

impl Meta {
    /// The tag `name` (as a title pattern writes it), if the file has it.
    pub fn tag(&self, name: &str) -> Option<String> {
        let text = |t: &Option<String>| t.clone();
        match name {
            "title" => text(&self.title),
            "artist" => text(&self.artist),
            "album" => text(&self.album),
            "album_artist" => text(&self.album_artist),
            "composer" => text(&self.composer),
            "genre" => text(&self.genre),
            "track" => self.track.map(|n| n.to_string()),
            "disc" => self.disc.map(|n| n.to_string()),
            "year" => self.year.map(|n| n.to_string()),
            _ => None,
        }
    }
}

/// A tag of the user's own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    /// sRGB.
    pub color: [u8; 3],
}

/// A file now in the store, to [`SongStore::add`].
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub key: String,
    /// The original file name.
    pub name: String,
}

#[derive(Clone)]
pub struct SongStore {
    dir: PathBuf,
    songs: BTreeMap<String, Song>,
    /// The keys whose files are here.
    present: BTreeSet<String>,
    /// The keys removed here, not to be fetched again.
    blocked: BTreeSet<String>,
    /// The tags, by ID.
    tags: BTreeMap<String, Tag>,
    /// Counts the changes made (`revision`).
    revision: u64,
}

impl SongStore {
    /// The store in `dir`, as it is on disk.
    pub fn open(dir: PathBuf) -> Self {
        let mut songs: BTreeMap<String, Song> = std::fs::read_to_string(dir.join(INDEX))
            .ok()
            .and_then(|text| serde_json::from_str(&text).map_err(|e| log::warn!("{INDEX}: {e}")).ok())
            .unwrap_or_default();
        bucket_flat_files(&dir);
        let present = present_in(&dir);
        let mut changed = false;
        for key in &present {
            songs.entry(key.clone()).or_insert_with_key(|key| {
                changed = true;
                Song::new(key.clone())
            });
        }
        let blocked = std::fs::read_to_string(dir.join(BLOCKED))
            .ok()
            .and_then(|text| serde_json::from_str(&text).map_err(|e| log::warn!("{BLOCKED}: {e}")).ok())
            .unwrap_or_default();
        let tags = std::fs::read_to_string(dir.join(TAGS))
            .ok()
            .and_then(|text| serde_json::from_str(&text).map_err(|e| log::warn!("{TAGS}: {e}")).ok())
            .unwrap_or_default();
        let mut store = Self { dir, songs, present, blocked, tags, revision: 0 };
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
        let song = self.songs.entry(stored.key.clone()).or_insert_with(|| Song::new(stored.name.clone()));
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
        self.songs.insert(key.to_string(), Song::new(name.to_string()));
        self.save();
    }

    /// Take what the synced library says of each song: its name, tags and
    /// what its file says (when a device has read it), and the first time
    /// any device imported it (knowing it, if it wasn't). Saved once, at
    /// the end.
    pub fn learn(&mut self, songs: impl IntoIterator<Item = (String, Song)>) {
        let mut changed = false;
        for (key, synced) in songs {
            if !is_key(&key) {
                continue;
            }
            let song = self.songs.entry(key).or_insert_with(|| Song::new(synced.name.clone()));
            let mut learned = Song { meta: synced.meta.or_else(|| song.meta.clone()), ..synced };
            // (One from before it was synced says 0.)
            if learned.imported == 0 || song.imported < learned.imported {
                learned.imported = song.imported;
            }
            if *song != learned {
                *song = learned;
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    /// Note what the songs' files say of themselves, for those not known
    /// yet (a device that read them first said it, for all of them).
    /// Saved once, at the end: whether anything changed.
    pub fn note_meta(&mut self, metas: impl IntoIterator<Item = (String, Meta)>) -> bool {
        let mut changed = false;
        for (key, meta) in metas {
            if let Some(song) = self.songs.get_mut(&key).filter(|s| s.meta.is_none()) {
                song.meta = Some(meta);
                changed = true;
            }
        }
        if changed {
            self.save();
        }
        changed
    }

    /// Whether the song at `path` is starred.
    pub fn is_starred(&self, path: &Path) -> bool {
        self.get(path).is_some_and(|song| song.tags.iter().any(|t| t == STARRED))
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

    /// Delete the song `key`'s file, damaged, without blocking it: this
    /// device fetches it from the others again.
    pub fn discard(&mut self, key: &str) -> Result<()> {
        let path = self.path(key);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
        }
        self.present.remove(key);
        self.revision += 1;
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

    /// The tags, by ID.
    pub fn tags(&self) -> &BTreeMap<String, Tag> {
        &self.tags
    }

    /// The song at `path`'s tags (by ID, those defined), by name.
    pub fn tags_of(&self, path: &Path) -> Vec<(&str, &Tag)> {
        let Some(song) = self.get(path) else { return Vec::new() };
        let mut tags: Vec<(&str, &Tag)> = song.tags.iter().filter_map(|id| self.tags.get_key_value(id)).map(|(id, tag)| (id.as_str(), tag)).collect();
        tags.sort_by_key(|(_, tag)| tag.name.to_lowercase());
        tags
    }

    /// Add or change the tag `id`.
    pub fn set_tag(&mut self, id: &str, tag: Tag) {
        if self.tags.get(id) != Some(&tag) {
            self.tags.insert(id.to_string(), tag);
            self.save_tags();
        }
    }

    /// Delete the tag `id`, from every song too.
    pub fn remove_tag(&mut self, id: &str) {
        if self.tags.remove(id).is_some() {
            self.save_tags();
        }
        let mut changed = false;
        for song in self.songs.values_mut() {
            let before = song.tags.len();
            song.tags.retain(|t| t != id);
            changed |= song.tags.len() != before;
        }
        if changed {
            self.save();
        }
    }

    /// Give the song `key` these tags (by ID), if it's known.
    pub fn set_song_tags(&mut self, key: &str, mut tags: Vec<String>) {
        tags.sort();
        tags.dedup();
        if let Some(song) = self.songs.get_mut(key)
            && song.tags != tags
        {
            song.tags = tags;
            self.save();
        }
    }

    /// A number that changes whenever anything in the store does, so
    /// what's made from it (the Songs list) knows to be made again.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The key of the song at `path`, if it's in this store.
    pub fn key(&self, path: &Path) -> Option<String> {
        self.key_of(path).map(str::to_string)
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

    /// What's known of song `key` (here or not).
    pub fn get_by_key(&self, key: &str) -> Option<&Song> {
        self.songs.get(key)
    }

    /// Every song here: its file and what's known of it.
    pub fn songs(&self) -> impl Iterator<Item = (PathBuf, &Song)> {
        self.present.iter().filter_map(|key| Some((path_of(&self.dir, key), self.songs.get(key)?)))
    }

    /// Where the song `key`'s file is (or would be).
    pub fn path(&self, key: &str) -> PathBuf {
        path_of(&self.dir, key)
    }

    /// The key of the song at `path`, if it's in this store.
    fn key_of<'a>(&self, path: &'a Path) -> Option<&'a str> {
        key_in(&self.dir, path)
    }

    /// Write `library.json` (even with nothing in it: the one-time import
    /// has run).
    fn save(&mut self) {
        let _timing = crate::frame_timing::scope("song store save");
        self.revision += 1;
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|()| write_atomically(&self.dir.join(INDEX), serde_json::to_string_pretty(&self.songs).unwrap_or_default().as_bytes()));
        if let Err(e) = written {
            log::warn!("couldn't save {}: {e}", self.dir.join(INDEX).display());
        }
    }

    fn save_tags(&mut self) {
        let _timing = crate::frame_timing::scope("song store save");
        self.revision += 1;
        let path = self.dir.join(TAGS);
        let written = std::fs::create_dir_all(&self.dir).and_then(|()| write_atomically(&path, serde_json::to_string_pretty(&self.tags).unwrap_or_default().as_bytes()));
        if let Err(e) = written {
            log::warn!("couldn't save {}: {e}", path.display());
        }
    }

    fn save_blocked(&mut self) {
        let _timing = crate::frame_timing::scope("song store save");
        self.revision += 1;
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

/// Where the song `key`'s file is in the store in `dir`: in the subfolder
/// named by its first two characters.
pub fn path_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(key.get(..2).unwrap_or_default()).join(key)
}

/// The key of the song at `path`, if it's a file of the store in `dir`.
pub fn key_in<'a>(dir: &Path, path: &'a Path) -> Option<&'a str> {
    let key = path.file_name()?.to_str().filter(|n| is_key(n))?;
    (path.parent()? == path_of(dir, key).parent()?).then_some(key)
}

/// The keys of the songs whose files are in the store in `dir`.
fn present_in(dir: &Path) -> BTreeSet<String> {
    let Ok(buckets) = std::fs::read_dir(dir) else { return BTreeSet::new() };
    let mut present = BTreeSet::new();
    for bucket in buckets.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
        let bucket_name = bucket.file_name();
        let Some(prefix) = bucket_name.to_str().filter(|n| n.len() == 2) else { continue };
        let Ok(entries) = std::fs::read_dir(bucket.path()) else { continue };
        present.extend(entries.flatten().filter_map(|e| e.file_name().to_str().filter(|n| is_key(n) && n.starts_with(prefix)).map(str::to_string)));
    }
    present
}

/// Move songs stored before the subfolders, straight in `dir`, into theirs.
fn bucket_flat_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())) {
        let Some(key) = entry.file_name().to_str().filter(|n| is_key(n)).map(str::to_string) else { continue };
        let to = path_of(dir, &key);
        let moved = std::fs::create_dir_all(to.parent().unwrap_or(dir)).and_then(|()| {
            if to.exists() { std::fs::remove_file(entry.path()) } else { std::fs::rename(entry.path(), &to) }
        });
        if let Err(e) = moved {
            log::warn!("couldn't move {} into its subfolder: {e}", entry.path().display());
        }
    }
}

/// Whether the file at `path` still hashes to `key`, the song it's kept as.
pub fn intact(path: &Path, key: &str) -> std::io::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(key_for(&hasher.finalize(), key) == key)
}

/// Make the folder for `to`, a song's file, if need be.
fn make_parent(to: &Path) -> Result<()> {
    let parent = to.parent().unwrap_or(to);
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))
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
    let to = path_of(dir, &key);
    if to.exists() {
        let _ = std::fs::remove_file(&temp);
    } else {
        make_parent(&to)?;
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
    let to = path_of(dir, &key);
    if to.exists() {
        std::fs::remove_file(file).with_context(|| format!("removing {}", file.display()))?;
    } else {
        make_parent(&to)?;
        std::fs::rename(file, &to).with_context(|| format!("writing {}", to.display()))?;
    }
    Ok(Stored { key, name })
}

/// Write `bytes`, a song called `name`, into the store in `dir`.
pub fn write_in(bytes: &[u8], name: &str, dir: &Path) -> Result<Stored> {
    let key = key_for(&Sha256::digest(bytes), name);
    let to = path_of(dir, &key);
    if !to.exists() {
        make_parent(&to)?;
        write_atomically(&to, bytes).with_context(|| format!("writing {}", to.display()))?;
    }
    Ok(Stored { key, name: name.to_string() })
}

/// A song fetched as `key`, written into the store in `dir` as it comes,
/// and kept (`finish`) only if it's that song.
pub struct Incoming {
    key: String,
    dir: PathBuf,
    temp: PathBuf,
    /// (Closed before it's renamed: Windows won't rename an open file.)
    file: Option<std::fs::File>,
    hasher: Sha256,
}

impl Incoming {
    pub fn new(key: &str, dir: &Path) -> Result<Self> {
        anyhow::ensure!(is_key(key), "that's not a song's key");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp = dir.join(format!(".fetching-{}-{n}-{key}", std::process::id()));
        let file = std::fs::File::create(&temp).with_context(|| format!("writing {}", temp.display()))?;
        Ok(Self { key: key.to_string(), dir: dir.to_path_buf(), temp, file: Some(file), hasher: Sha256::new() })
    }

    /// Keep it, if it's the song.
    pub fn finish(mut self) -> Result<()> {
        let hex: String = self.hasher.clone().finalize().iter().map(|b| format!("{b:02x}")).collect();
        anyhow::ensure!(self.key.starts_with(&format!("{hex}.")), "the song didn't arrive whole");
        if let Some(file) = self.file.take() {
            file.sync_all().with_context(|| format!("writing {}", self.temp.display()))?;
        }
        let to = path_of(&self.dir, &self.key);
        make_parent(&to)?;
        std::fs::rename(&self.temp, &to).with_context(|| format!("writing {}", to.display()))
    }
}

impl Write for Incoming {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.as_mut().map_or(Ok(0), |f| f.write(buf))?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

impl Drop for Incoming {
    // (Gone after `finish`.)
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.temp);
    }
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
                done.moved.push((file.to_path_buf(), path_of(dir, &stored.key)));
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
            if key_in(dir, &file).is_some() {
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
        let path = path_of(&store_dir, &a.key);
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
        std::fs::remove_file(path_of(&dir, &gone.key)).unwrap();
        // Written as the app quit, never recorded.
        let unrecorded = write_in(b"new", "new.ogg", &dir).unwrap();
        std::fs::write(dir.join("not a song.txt"), b"").unwrap();

        let mut store = SongStore::open(dir.clone());
        let keys: Vec<String> = store.songs().map(|(p, _)| file_name(&p)).collect();
        let mut expected = vec![kept.key.clone(), unrecorded.key.clone()];
        expected.sort();
        assert_eq!(keys, expected);
        assert_eq!(store.get(&path_of(&dir, &kept.key)).unwrap().name, "kept.mid");
        // Gone from here, still known by name.
        assert_eq!(store.get(&path_of(&dir, &gone.key)).unwrap().name, "gone.mid");
        // Known from elsewhere: named, not here.
        let elsewhere = format!("{}.mid", "a".repeat(64));
        store.know(&elsewhere, "Elsewhere.mid");
        store.know("../escape", "nope");
        let store = SongStore::open(dir.clone());
        assert_eq!(store.get(&path_of(&dir, &elsewhere)).unwrap().name, "Elsewhere.mid");
        assert!(store.get(&dir.join("../escape")).is_none());
        assert_eq!(store.songs().count(), 2);
    }

    #[test]
    fn a_damaged_song_is_fetched_again() {
        let dir = temp_dir("damaged");
        let stored = write_in(b"whole", "song.mp3", &dir).unwrap();
        let mut store = SongStore::open(dir.clone());
        store.add(stored.clone());
        let path = path_of(&dir, &stored.key);
        assert!(intact(&path, &stored.key).unwrap());
        std::fs::write(&path, b"wh").unwrap();
        assert!(!intact(&path, &stored.key).unwrap());
        store.discard(&stored.key).unwrap();
        assert!(!path.exists());
        assert!(!store.has(&stored.key));
        assert!(!store.is_blocked(&stored.key), "not removed: fetched again");
        assert_eq!(store.known().count(), 1, "still known by name");
    }

    #[test]
    fn moving_in_and_removing() {
        let dir = temp_dir("move");
        std::fs::write(dir.join("Picked.mid"), b"picked").unwrap();
        let stored = move_in(&dir.join("Picked.mid"), &dir).unwrap();
        assert!(!dir.join("Picked.mid").exists());
        let mut store = SongStore::open(dir.clone());
        store.add(stored.clone());
        let path = path_of(&dir, &stored.key);
        assert_eq!(store.get(&path).unwrap().name, "Picked.mid");
        // Not one of its files.
        assert!(store.get(&path_of(&dir.join("elsewhere"), &stored.key)).is_none());
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
    fn flat_stores_move_into_subfolders() {
        let dir = temp_dir("flat");
        let key = key_for(&Sha256::digest(b"flat"), "Flat.mid");
        std::fs::write(dir.join(&key), b"flat").unwrap();
        // Already in its subfolder too: the flat copy goes.
        let both = write_in(b"both", "Both.mid", &dir).unwrap();
        std::fs::write(dir.join(&both.key), b"both").unwrap();
        let store = SongStore::open(dir.clone());
        assert_eq!(store.songs().count(), 2);
        assert_eq!(std::fs::read(path_of(&dir, &key)).unwrap(), b"flat");
        assert!(!dir.join(&key).exists() && !dir.join(&both.key).exists());
        assert_eq!(path_of(&dir, &key).parent().unwrap().file_name().unwrap(), &key[..2]);
        assert!(store.get(&dir.join(&key)).is_none(), "not where its files are");
    }

    #[test]
    fn tags() {
        let dir = temp_dir("tags");
        let mut store = SongStore::open(dir.clone());
        let stored = write_in(b"tagged", "Tagged.mid", &dir).unwrap();
        store.add(stored.clone());
        let path = path_of(&dir, &stored.key);
        store.set_tag("b", Tag { name: "Chill".into(), color: [1, 2, 3] });
        store.set_tag("a", Tag { name: "party".into(), color: [4, 5, 6] });
        store.set_song_tags(&stored.key, vec!["a".into(), "b".into(), "a".into(), "gone".into()]);
        let names = |store: &SongStore| store.tags_of(&path).iter().map(|(_, t)| t.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&store), ["Chill", "party"], "by name, without case; an unknown ID left out");
        let mut store = SongStore::open(dir.clone());
        assert_eq!(names(&store), ["Chill", "party"], "kept");
        store.remove_tag("b");
        assert_eq!(names(&SongStore::open(dir.clone())), ["party"], "deleted from the song too");
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
