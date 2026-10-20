//! What the songs' files say of themselves (`song_store::Meta`), into the
//! library, where it syncs: read here (`song_info`, this session, not what
//! an older version kept) for every song here nothing's said it for yet.

use std::path::PathBuf;

use super::App;

impl App {
    /// Note what's been read of the songs here the library knows nothing of
    /// yet, asking for the rest to be read (they're noted once they are).
    pub(super) fn note_song_meta(&mut self) {
        let seen = (self.songs.revision(), self.song_info.revision());
        if self.meta_seen == Some(seen) {
            return;
        }
        self.meta_seen = Some(seen);
        let lacking: Vec<(String, PathBuf)> =
            self.songs.songs().filter(|(_, song)| song.meta.is_none()).filter_map(|(path, _)| Some((self.songs.key(&path)?, path))).collect();
        let mut read = Vec::new();
        for (key, path) in lacking {
            if let Some(info) = self.song_info.fresh(&path) {
                let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                read.push((key, info.meta(size)));
            }
        }
        if self.songs.note_meta(read) {
            self.songs_changed();
        }
    }
}
