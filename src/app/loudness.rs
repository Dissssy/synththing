//! Measuring how loud the songs here are (`crate::loudness`), to even out
//! their volume: one at a time, on a thread of its own, for each song its
//! tags don't say it for and no device has measured yet; saved to the
//! songs, and so synced. Not on Android: phones take what other devices
//! measured.

use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui;

use super::App;
use crate::fields::Facts;
use crate::loudness::Measurer;
use crate::song_store::{META_VERSION, Song};

#[derive(Default)]
pub(super) struct MeasureState {
    measurer: Option<Measurer>,
    /// The song being measured, by key.
    measuring: Option<String>,
    /// The songs left to measure, as of a songs revision.
    left: Option<(u64, Vec<(String, PathBuf)>)>,
    /// The songs that couldn't be measured (this session).
    failed: HashSet<String>,
}

/// Whether `song` is still to be measured: its file's been read (by this
/// version, so its ReplayGain tags would be known), they say nothing, and
/// it's not MIDI (whose loudness is its soundfont's).
fn unmeasured(song: &Song) -> bool {
    song.loudness.is_none() && song.meta.as_ref().is_some_and(|m| m.version >= META_VERSION && m.replay_gain.is_none()) && !Facts::of(song).is_midi()
}

impl App {
    /// Take in what's been measured, and ask for the next song once it's
    /// in. Once a frame.
    pub(super) fn measure_tick(&mut self, ctx: &egui::Context) {
        if cfg!(target_os = "android") {
            return;
        }
        while let Some((key, measured)) = self.loudness.measurer.as_ref().and_then(Measurer::answer) {
            self.loudness.measuring = None;
            match measured {
                Ok(loudness) => {
                    self.songs.set_loudness(&key, loudness);
                    self.songs_changed();
                    // (It may be the one playing.)
                    self.send_volume();
                }
                Err(e) => {
                    log::info!("couldn't measure how loud {key} is: {e}");
                    self.loudness.failed.insert(key);
                }
            }
        }
        if self.loudness.measuring.is_some() {
            return;
        }
        let revision = self.songs.revision();
        if self.loudness.left.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            let left = self.songs.songs().filter(|(_, song)| unmeasured(song)).filter_map(|(path, _)| Some((self.songs.key(&path)?, path))).collect();
            self.loudness.left = Some((revision, left));
        }
        let Some((_, left)) = self.loudness.left.as_mut() else { return };
        let Some((key, path)) = std::iter::from_fn(|| left.pop()).find(|(key, _)| !self.loudness.failed.contains(key)) else { return };
        if self.loudness.measurer.is_none() {
            let ctx = ctx.clone();
            match Measurer::start(move || ctx.request_repaint()) {
                Ok(measurer) => self.loudness.measurer = Some(measurer),
                Err(e) => {
                    log::warn!("couldn't start measuring songs' loudness: {e}");
                    return;
                }
            }
        }
        if let Some(measurer) = &self.loudness.measurer {
            measurer.ask(key.clone(), path);
            self.loudness.measuring = Some(key);
        }
    }
}
