//! The dedicated visualizer: fullscreen, nothing else on screen.

use eframe::egui;

use crate::audio::PlaybackShared;
use crate::config::nice_name;
use crate::script_host;
use crate::visualizer::DisplayMode;

use super::{App, DEDICATED_HINT_SECS, lock_panel};

impl App {
    /// Enter the dedicated visualizer fullscreen: OS fullscreen with only
    /// the visualizer, which gets keyboard focus.
    pub(super) fn enter_dedicated(&mut self, ctx: &egui::Context) {
        let since = ctx.input(|i| i.time);
        self.dedicated = Some(Dedicated { since });
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        lock_panel(&self.visualizer).request_focus();
    }

    /// Start the running visualizer script over from scratch (F5, or the
    /// Restart button).
    pub(super) fn restart_script(&mut self) {
        let name = self.script.path().map(nice_name).unwrap_or_else(|| "script".to_string());
        crate::shared_data::clear();
        self.status = if self.script.restart() {
            format!("Restarted {name}.")
        } else {
            "No script running to restart.".to_string()
        };
    }

    /// Out of the dedicated fullscreen (F11 or Esc): back to the window.
    pub(super) fn exit_dedicated(&mut self, ctx: &egui::Context) {
        if self.dedicated.take().is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    /// The dedicated fullscreen's whole window: the visualizer edge to edge,
    /// plus a brief "Esc to exit" hint.
    pub(super) fn dedicated_ui(&mut self, ui: &mut egui::Ui, shared: &PlaybackShared) {
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            self.show_visualizer(ui, &shared.notes, &shared.view, DisplayMode::Dedicated);
        });
        let ctx = ui.ctx().clone();
        let elapsed = ctx.input(|i| i.time) - self.dedicated.as_ref().map_or(0.0, |d| d.since);
        if elapsed < DEDICATED_HINT_SECS {
            egui::Area::new(egui::Id::unique("dedicated_exit_hint"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 24.0))
                .interactable(false)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label("Press Esc to exit");
                    });
                });
        }
    }

    /// The script has been on one frame (or loading) for a while: say so in
    /// the visualizer's bottom-left corner, with a button to stop it.
    pub(super) fn script_busy_ui(&mut self, ctx: &egui::Context) {
        let rect = self.visualizer_output.image_rect;
        let Some(busy) = self.script.busy_for() else { return };
        if !self.visualizer_drawn || busy < script_host::BUSY_NOTICE_AFTER || !rect.is_positive() {
            return;
        }
        let name = self.script.path().map(nice_name).unwrap_or_else(|| "The script".to_string());
        let mut stop = false;
        egui::Area::new(egui::Id::unique("script_busy_notice"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(rect.left_bottom() + egui::vec2(12.0, -12.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(ui.visuals().warn_fg_color, "!");
                        ui.label(format!("{name} hasn't finished a frame in {:.1} s", busy.as_secs_f32()));
                        stop = ui
                            .button("Stop")
                            .on_hover_text(format!(
                                "Stop it now (the watchdog would at {} s). Fix it and save, or press Restart (F5), to run it again.",
                                script_host::APP_WATCHDOG_LIMIT.as_secs()
                            ))
                            .clicked();
                    });
                });
            });
        if stop {
            self.script.stop();
            self.status = format!("Stopped {name}.");
        }
    }

    /// Confine the cursor to the window while a script asks for it, but only
    /// while the visualizer has focus and the window is focused. Released the
    /// moment either stops being true (Escape or a click elsewhere included),
    /// so a script can never trap the cursor. `mouse_delta` is raw mouse
    /// motion while focused, so turning doesn't stop at the window's edge.
    pub(super) fn apply_cursor_confinement(&mut self, ctx: &egui::Context) {
        let window_focused = ctx.input(|i| i.focused);
        let want = self.visualizer_output.focused
            && self.visualizer_output.cursor.confined
            && window_focused;
        if want != self.cursor_confined {
            self.cursor_confined = want;
            let grab = if want { egui::viewport::CursorGrab::Confined } else { egui::viewport::CursorGrab::None };
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab));
        }
    }
}

/// The dedicated visualizer fullscreen: just the visualizer, nothing else.
pub(super) struct Dedicated {
    /// egui time it was entered, for the exit hint.
    since: f64,
}
