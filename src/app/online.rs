//! A branch build's online features (`build_info::Online`): off at every
//! launch, and turned on here, kind by kind, for that run only.

use eframe::egui;

use super::App;
use crate::build_info::{self, Channel, Online};

impl App {
    /// Whether `kind` is on. If it's off (a branch build), says so where it
    /// would be, with a button that turns it on for this run.
    pub(super) fn online_ui(&mut self, ui: &mut egui::Ui, kind: Online) -> bool {
        if build_info::online(kind) {
            return true;
        }
        ui.horizontal_wrapped(|ui| {
            ui.weak(kind.off_note());
            if ui.button("Turn on for this run").on_hover_text(TURN_ON_HOVER).clicked() {
                self.set_online(kind, true);
            }
        });
        false
    }

    /// Preferences > Online Library, in a branch build: every kind, to turn
    /// on or off for this run.
    pub(super) fn branch_online_ui(&mut self, ui: &mut egui::Ui) {
        if !matches!(build_info::channel(), Channel::Branch(_)) {
            return;
        }
        ui.strong("Online in this branch build");
        ui.weak(TURN_ON_HOVER);
        for kind in Online::ALL {
            let mut on = build_info::online(kind);
            if ui.checkbox(&mut on, kind.name()).changed() {
                self.set_online(kind, on);
            }
        }
        ui.separator();
    }

    pub(super) fn set_online(&mut self, kind: Online, on: bool) {
        build_info::set_online(kind, on);
        log::info!("{} turned {} for this run", kind.name(), if on { "on" } else { "off" });
        if kind == Online::Scripts {
            // (Searched again, now with the servers, or without them.)
            self.library.searched = false;
        }
    }
}

/// What turning one on means.
const TURN_ON_HOVER: &str = "Branch builds start with online features off, so testing one doesn't touch the real \
                             servers by accident. Turn on only what this branch is about; it's never saved, so the \
                             next launch starts with them off again.";
