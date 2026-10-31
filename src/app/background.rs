//! The library's background work in the app (`crate::background`): on a
//! computer, while the user has no hub (with one, the hub does it).

use eframe::egui;

use super::App;
use crate::background::{self, Jobs};

impl App {
    /// Take in what the background jobs did, and start what's next. Once a
    /// frame.
    pub(super) fn background_tick(&mut self, ctx: &egui::Context) {
        if !background::runs_here(self.has_hub(), false) {
            return;
        }
        let ctx = ctx.clone();
        let jobs = self.background.get_or_insert_with(|| Jobs::new(move || ctx.request_repaint()));
        if jobs.tick(&mut self.songs, &self.config) {
            self.songs_changed();
            // (How loud the playing song is may be known now.)
            self.send_volume();
        }
    }
}
