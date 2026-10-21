//! The which-key popup: a moment into a key sequence (Ctrl+G, then...),
//! a panel at the bottom of the window lists every press that can follow
//! and what it does, so the keys never need learning by heart.

use eframe::egui;

use super::App;
use crate::commands::{self, Keymap, Next};

/// How long a sequence waits before showing what can follow, so the
/// popup doesn't flash up for someone who knows the keys.
const DELAY: f64 = 0.4;

impl App {
    pub(super) fn which_key_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.keymap_pending else { return };
        let waited = ctx.input(|i| i.time) - pending.since;
        if waited < DELAY {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(DELAY - waited));
            return;
        }
        let next = Keymap::of(ctx).next(&pending.presses);
        egui::Area::new(egui::Id::new("which_key"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -72.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.strong(format!("{} ...", commands::display(ctx, &pending.presses)));
                    ui.add_space(4.0);
                    egui::Grid::new("which_key_list").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                        for (press, then) in next {
                            ui.monospace(ctx.format_shortcut(&press));
                            match then {
                                Next::Command(command) => ui.label(command.name()),
                                Next::More(n) => ui.weak(format!("{n} more...")),
                            };
                            ui.end_row();
                        }
                    });
                    ui.add_space(4.0);
                    ui.weak("Esc or any other key cancels");
                });
            });
    }
}
