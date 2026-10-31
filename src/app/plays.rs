//! Counting the songs' plays and skips as they're listened to
//! (`play_stats.rs`): each counted on this device, in the library, and
//! synced.

use std::path::Path;

use super::App;
use crate::engine::EngineView;
use crate::play_stats::Heard;

impl App {
    /// The song at `path` is starting: the one before was skipped, maybe.
    pub(super) fn listening_to(&mut self, path: &Path) {
        let heard = self.listening.start(self.songs.key(path));
        self.count(heard);
    }

    /// Keep up with the playing song: played, maybe.
    pub(super) fn plays_tick(&mut self, view: &EngineView) {
        // (Until the song sent is the one playing, the view's another's.)
        if view.loads < self.song_loads {
            return;
        }
        let heard = self.listening.at(view.position, view.length, view.finished);
        self.count(heard);
    }

    fn count(&mut self, heard: Option<Heard>) {
        let Some(heard) = heard else { return };
        let device = crate::device::Device::load().id;
        match heard {
            Heard::Played(key) => self.songs.played(&key, &device),
            Heard::Skipped(key) => self.songs.skipped(&key, &device),
        }
        self.stats_changed();
    }
}
