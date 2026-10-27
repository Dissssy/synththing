//! Songs a program sent to be checked before they're imported (the local
//! API's `/v1/staged`): kept apart from the library, in a folder of their
//! own, out of everything the user sees and of sync, until the program
//! says to import one (it moves into the library without being sent
//! again) or drops it. Each is read when it comes, and can be matched at
//! MusicBrainz (`lookup`) as the library's songs are, the match kept for
//! when it's imported. What isn't imported within a day is dropped, and
//! nothing staged outlasts synththing quitting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::song_store::{self, Online, Song};

/// How long a staged song waits to be imported before it's dropped.
const KEPT: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Staged {
    dir: PathBuf,
    songs: BTreeMap<String, Entry>,
}

struct Entry {
    song: Song,
    at: Instant,
}

impl Staged {
    /// Staging in `dir`, emptied of anything staged before.
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&dir);
        Staged { dir, songs: BTreeMap::new() }
    }

    /// Stage the file `bytes` named `name`, read: its key, and whether it
    /// wasn't staged already.
    pub fn add(&mut self, bytes: &[u8], name: &str) -> Result<(String, bool)> {
        self.sweep();
        let stored = song_store::write_in(bytes, name, &self.dir)?;
        if let Some(entry) = self.songs.get_mut(&stored.key) {
            entry.at = Instant::now();
            return Ok((stored.key, false));
        }
        let path = song_store::path_of(&self.dir, &stored.key);
        let read = match crate::song_info::read(&path) {
            Ok(info) => info,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                anyhow::bail!("couldn't read {name}: {e}");
            }
        };
        let song = Song { name: stored.name, meta: Some(read.meta), ..Song::default() };
        self.songs.insert(stored.key.clone(), Entry { song, at: Instant::now() });
        Ok((stored.key, true))
    }

    /// The staged songs, by key.
    pub fn songs(&mut self) -> impl Iterator<Item = (&str, &Song)> {
        self.sweep();
        self.songs.iter().map(|(key, entry)| (key.as_str(), &entry.song))
    }

    pub fn get(&self, key: &str) -> Option<&Song> {
        self.songs.get(key).map(|entry| &entry.song)
    }

    /// Note what the song `key` was matched to at MusicBrainz.
    pub fn matched(&mut self, key: &str, online: Option<Online>) {
        if let Some(entry) = self.songs.get_mut(key) {
            entry.song.online = online;
        }
    }

    /// Move the song `key` into the store in `dir`, no longer staged: what
    /// it was named, and its match, if it was matched.
    pub fn import(&mut self, key: &str, dir: &Path) -> Result<Option<(song_store::Stored, Option<Online>)>> {
        let Some(entry) = self.songs.remove(key) else { return Ok(None) };
        let (from, to) = (song_store::path_of(&self.dir, key), song_store::path_of(dir, key));
        if to.exists() {
            let _ = std::fs::remove_file(&from);
        } else {
            std::fs::create_dir_all(to.parent().unwrap_or(dir)).with_context(|| format!("creating {}", dir.display()))?;
            if let Err(e) = std::fs::rename(&from, &to) {
                // (Elsewhere on disk: copied instead.)
                std::fs::copy(&from, &to).with_context(|| format!("moving {} into the library: {e}", from.display()))?;
                let _ = std::fs::remove_file(&from);
            }
        }
        Ok(Some((song_store::Stored { key: key.to_string(), name: entry.song.name }, entry.song.online)))
    }

    /// Drop the song `key`: whether it was staged.
    pub fn remove(&mut self, key: &str) -> bool {
        let _ = std::fs::remove_file(song_store::path_of(&self.dir, key));
        self.songs.remove(key).is_some()
    }

    /// Drop what's waited too long.
    fn sweep(&mut self) {
        let old: Vec<String> = self.songs.iter().filter(|(_, entry)| entry.at.elapsed() > KEPT).map(|(key, _)| key.clone()).collect();
        for key in old {
            self.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A MIDI file of no notes, told apart by `n`.
    fn midi(n: u8) -> Vec<u8> {
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\0".to_vec();
        bytes.push(n);
        bytes.extend_from_slice(b"MTrk\0\0\0\x04\0\xff\x2f\0");
        bytes
    }

    #[test]
    fn staged_songs_move_into_the_library_when_imported() {
        let root = std::env::temp_dir().join(format!("synththing-staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (staging, library) = (root.join("staged"), root.join("music"));
        let mut staged = Staged::new(staging.clone());
        let (key, new) = staged.add(&midi(1), "Theme.mid").unwrap();
        assert!(new);
        assert_eq!(staged.add(&midi(1), "Theme.mid").unwrap(), (key.clone(), false));
        assert_eq!(staged.songs().count(), 1);
        assert_eq!(staged.get(&key).unwrap().name, "Theme.mid");
        let online = Online { recording: Some("rec".into()), ..Online::default() };
        staged.matched(&key, Some(online.clone()));
        let (stored, matched) = staged.import(&key, &library).unwrap().unwrap();
        assert_eq!((stored.key.as_str(), stored.name.as_str(), matched), (key.as_str(), "Theme.mid", Some(online)));
        assert!(song_store::path_of(&library, &key).exists());
        assert!(!song_store::path_of(&staging, &key).exists());
        assert!(staged.import(&key, &library).unwrap().is_none(), "imported once");
        // (Dropped: gone from the folder too; and nothing outlasts a restart.)
        let (other, _) = staged.add(&midi(2), "Other.mid").unwrap();
        assert!(staged.remove(&other));
        assert!(!song_store::path_of(&staging, &other).exists());
        staged.add(&midi(3), "Third.mid").unwrap();
        let mut staged = Staged::new(staging.clone());
        assert_eq!(staged.songs().count(), 0);
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
