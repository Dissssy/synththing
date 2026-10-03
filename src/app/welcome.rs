//! The welcome window: shown the first time synththing runs (and from
//! Help > Welcome...), offering the starter pack (`starter.rs`): songs
//! written to a folder the Songs tab then opens in, and soundfonts
//! downloaded in the background and added to the list as they arrive.

use std::path::Path;
use std::time::Duration;

use eframe::egui;

use super::App;
use crate::starter;

#[derive(Default)]
pub(super) struct Welcome {
    pub open: bool,
    /// The "Add the starter pack" checkbox.
    pub get_pack: bool,
    pub downloader: starter::Downloader,
    /// How many of the downloaded soundfonts are in the list already.
    registered: usize,
    /// Whether the download was running last frame, to report its end.
    was_running: bool,
}

fn same_file(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canonical(a) == canonical(b)
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

impl App {
    /// Show the welcome window, with the starter pack ticked unless it's
    /// all there already.
    pub fn open_welcome(&mut self) {
        let have_all = starter::soundfonts_dir().is_ok_and(|dir| {
            starter::SOUNDFONTS.iter().all(|sf| self.config.soundfonts.iter().any(|p| same_file(p, &dir.join(sf.file))))
        });
        self.welcome.get_pack = !have_all;
        self.welcome.open = true;
    }

    pub(super) fn welcome_ui(&mut self, ctx: &egui::Context) {
        if !self.welcome.open {
            return;
        }
        let state = self.welcome.downloader.state();
        let mut start = false;
        let response = egui::Modal::new(egui::Id::new("welcome")).show(ctx, |ui| {
            ui.set_width(470.0);
            ui.heading("Welcome to synththing");
            ui.add_space(4.0);
            ui.label(
                "Play MIDI files and audio, watch them through visualizers, and play along with the games. \
                 Everything is a tab you can drag around; the View menu brings back any you close.",
            );
            ui.add_space(10.0);
            ui.add_enabled_ui(!state.running, |ui| {
                ui.checkbox(
                    &mut self.welcome.get_pack,
                    format!("Add the starter pack ({} download)", megabytes(starter::download_size())),
                );
            });
            ui.indent("starter_details", |ui| {
                ui.weak(
                    "Two soundfonts, the instruments MIDI files are played with: GeneralUser GS (full and \
                     detailed) and TimGM6mb (small and light). They download in the background.",
                );
                ui.weak(format!(
                    "Four songs to try, each instrument on its own channel, in {}. The Songs tab opens there.",
                    starter::songs_dir().display()
                ));
            });
            if state.running {
                ui.add_space(4.0);
                let fraction = state.done as f32 / starter::download_size().max(1) as f32;
                ui.add(egui::ProgressBar::new(fraction).show_percentage().text("Downloading soundfonts"));
            }
            if let Some(error) = &state.error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("The last download failed: {error}"));
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let label = if self.welcome.get_pack && !state.running { "Get started" } else { "Close" };
                start = ui.button(label).clicked();
            });
        });
        if start {
            if self.welcome.get_pack && !state.running {
                self.install_starter();
            }
            self.welcome.open = false;
        } else if response.should_close() {
            self.welcome.open = false;
        }
    }

    /// Write the songs (and open the Songs tab there), and start fetching
    /// the soundfonts.
    fn install_starter(&mut self) {
        match starter::write_songs() {
            Ok(dir) => {
                self.browser.navigate_to(dir.clone());
                if let Err(e) = self.config.set_browse_dir(dir) {
                    log::warn!("couldn't save the songs folder: {e:#}");
                }
            }
            Err(e) => self.status = format!("Couldn't write the starter songs: {e:#}"),
        }
        self.welcome.registered = 0;
        self.welcome.downloader.start();
    }

    /// Every frame: add soundfonts to the list as they finish downloading
    /// (the first one becoming the one in use, if none is yet), and report
    /// how the download went.
    pub(super) fn poll_starter(&mut self, ctx: &egui::Context) {
        let state = self.welcome.downloader.state();
        while let Some(path) = state.ready.get(self.welcome.registered).cloned() {
            self.welcome.registered += 1;
            if !self.config.soundfonts.iter().any(|p| same_file(p, &path)) {
                self.add_soundfont(path.clone());
            }
            if self.active_sf.is_none()
                && let Some(idx) = self.config.soundfonts.iter().position(|p| same_file(p, &path))
            {
                self.activate_soundfont(idx);
            }
        }
        if self.welcome.was_running && !state.running {
            self.status = match &state.error {
                Some(e) => format!("Couldn't download the starter soundfonts ({e}). Help > Welcome... tries again."),
                None => "Starter pack ready: pick a song in the Songs tab.".to_string(),
            };
        }
        self.welcome.was_running = state.running;
        if state.running {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// The download's progress, at the top of the Soundfonts tab.
    pub(super) fn starter_progress_ui(&self, ui: &mut egui::Ui) {
        let state = self.welcome.downloader.state();
        if state.running {
            let fraction = state.done as f32 / starter::download_size().max(1) as f32;
            ui.add(egui::ProgressBar::new(fraction).show_percentage().text("Downloading the starter soundfonts"));
        }
    }
}
