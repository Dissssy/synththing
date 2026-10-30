//! Measuring how loud the songs here are (`crate::loudness::Measuring`),
//! to even out their volume: on a computer, while there's no hub (with
//! one, the hub measures). Not on Android: phones take what other devices
//! measured.

use eframe::egui;

use super::App;
use crate::loudness::Measuring;

impl App {
    /// Take in what's been measured, and ask for the next song once it's
    /// in. Once a frame.
    pub(super) fn measure_tick(&mut self, ctx: &egui::Context) {
        if cfg!(target_os = "android") || self.has_hub() {
            return;
        }
        let ctx = ctx.clone();
        let measuring = self.loudness.get_or_insert_with(|| Measuring::new(move || ctx.request_repaint()));
        if measuring.tick(&mut self.songs) {
            self.songs_changed();
            // (It may be the one playing.)
            self.send_volume();
        }
    }
}
