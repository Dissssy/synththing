//! The library's background work, which one device does for all of them
//! and whose results sync: measuring how loud songs are
//! (`loudness::Measuring`). It runs on the hub when the user has one (it
//! has every song, and it's always on), else on each computer; never on a
//! phone ([`runs_here`]). The hub (`hub.rs`) and the app
//! (`app/background.rs`) tick it, and tell sync when the songs changed.

use crate::loudness::Measuring;
use crate::song_store::SongStore;

/// Whether background work runs on this device: `has_hub` is whether the
/// user has a hub, `is_hub` whether this device is it.
pub fn runs_here(has_hub: bool, is_hub: bool) -> bool {
    !cfg!(target_os = "android") && (is_hub || !has_hub)
}

/// Every background job.
pub struct Jobs {
    measuring: Measuring,
}

impl Jobs {
    /// `wake` is called when a job has something to take in (to `tick`
    /// soon).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { measuring: Measuring::new(wake) }
    }

    /// Take in what the jobs did, saved to `songs`, and start what's next.
    /// Whether the songs changed (for sync to send).
    pub fn tick(&mut self, songs: &mut SongStore) -> bool {
        self.measuring.tick(songs)
    }
}
