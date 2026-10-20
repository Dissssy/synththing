//! Repair library: every song in the store checked, on a thread of its
//! own. A file that no longer hashes to its key is damaged (the app
//! deletes it, and sync fetches it again); one that does but can't be read
//! (`song_info::read`) was broken when it came in, so fetching it again
//! won't help (the app offers to remove and block it).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};

use crate::{song_info, song_store};

/// What's wrong with a song.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// Its file isn't what was stored.
    Damaged,
    /// Its file is what was stored, and can't be read: why.
    Unreadable(String),
}

/// A check under way.
pub struct Check {
    checked: Arc<AtomicUsize>,
    total: usize,
    done: Receiver<Vec<(String, Problem)>>,
}

impl Check {
    /// Check `songs` (key, file), calling `wake` when it's done.
    pub fn start(songs: Vec<(String, PathBuf)>, wake: impl FnOnce() + Send + 'static) -> Self {
        let checked = Arc::new(AtomicUsize::new(0));
        let total = songs.len();
        let (tx, done) = mpsc::channel();
        let counter = Arc::clone(&checked);
        let _ = std::thread::Builder::new()
            .name("song-repair".into())
            .spawn(move || {
                let found: Vec<(String, Problem)> = songs
                    .into_iter()
                    .filter_map(|(key, path)| {
                        let problem = check(&key, &path);
                        counter.fetch_add(1, Ordering::Relaxed);
                        Some((key, problem?))
                    })
                    .collect();
                let _ = tx.send(found);
                wake();
            });
        Self {
            checked,
            total,
            done,
        }
    }

    /// How many songs have been checked, of how many.
    pub fn progress(&self) -> (usize, usize) {
        (self.checked.load(Ordering::Relaxed), self.total)
    }

    /// The songs with problems, once it's done (an empty list if the
    /// thread died).
    pub fn finished(&self) -> Option<Vec<(String, Problem)>> {
        match self.done.try_recv() {
            Ok(found) => Some(found),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Vec::new()),
        }
    }
}

/// What's wrong with the song `key` at `path`, if anything. (A file that
/// can't be opened is gone, not wrong.)
fn check(key: &str, path: &std::path::Path) -> Option<Problem> {
    match song_store::intact(path, key) {
        Ok(true) => song_info::read(path).err().map(Problem::Unreadable),
        Ok(false) => Some(Problem::Damaged),
        Err(e) => {
            log::warn!("couldn't check {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_damaged_and_unreadable_songs() {
        let dir = std::env::temp_dir().join(format!("synththing-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let broken = song_store::write_in(b"not a song at all", "broken.mp3", &dir).unwrap();
        let damaged = song_store::write_in(b"whole", "damaged.mid", &dir).unwrap();
        let damaged_path = song_store::path_of(&dir, &damaged.key);
        std::fs::write(&damaged_path, b"wh").unwrap();
        let songs = vec![
            (broken.key.clone(), song_store::path_of(&dir, &broken.key)),
            (damaged.key.clone(), damaged_path),
            (format!("{}.mid", "a".repeat(64)), dir.join("gone.mid")),
        ];
        let check = Check::start(songs, || {});
        let found = loop {
            if let Some(found) = check.finished() {
                break found;
            }
            std::thread::yield_now();
        };
        assert_eq!(check.progress(), (3, 3));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(matches!(&found[0], (key, Problem::Unreadable(_)) if *key == broken.key));
        assert_eq!(found[1], (damaged.key, Problem::Damaged));
    }
}
