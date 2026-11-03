//! Help > Log: the app's log, filtered, with its actions.

use std::time::Duration;

use eframe::egui;

use crate::applog;

use super::{App, file_manager, fit_width};

impl App {
    /// The app log viewer, as a modal (the mobile layout shows it as a
    /// page instead).
    pub(super) fn log_ui(&mut self, ctx: &egui::Context) {
        if !self.log_open || self.mobile() {
            return;
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::unique("app_log")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 720.0));
            ui.heading("Log");
            close = self.log_body_ui(ui, false);
        });
        if close || response.should_close() {
            self.log_open = false;
        }
    }

    /// The log: level filter, search, copy, and the log file's location.
    /// On a page the entries fill what's left, under the buttons, and
    /// there's no Close; whether Close was clicked.
    pub(super) fn log_body_ui(&mut self, ui: &mut egui::Ui, page: bool) -> bool {
        let entries = applog::entries();
        let search = self.log_search.to_lowercase();
        let shown: Vec<&applog::Entry> = entries
            .iter()
            .filter(|e| e.level <= self.log_min_level)
            .filter(|e| search.is_empty() || e.line().to_lowercase().contains(&search))
            .collect();
        ui.ctx().request_repaint_after(Duration::from_millis(250));

        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("log_min_level")
                .selected_text(match self.log_min_level {
                    log::Level::Error => "Errors",
                    log::Level::Warn => "Warnings and up",
                    _ => "Everything",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.log_min_level, log::Level::Info, "Everything");
                    ui.selectable_value(&mut self.log_min_level, log::Level::Warn, "Warnings and up");
                    ui.selectable_value(&mut self.log_min_level, log::Level::Error, "Errors");
                });
            ui.add(egui::TextEdit::singleline(&mut self.log_search).hint_text("search...").desired_width(200.0));
            ui.weak(format!("{} of {}", shown.len(), entries.len()));
        });
        if page {
            log_actions_ui(ui, &shown);
            log_file_note_ui(ui);
        }
        ui.separator();
        let scroll = egui::ScrollArea::both()
            .id_salt("app_log_scroll")
            .max_height(if page { f32::INFINITY } else { 420.0 })
            .auto_shrink([false, !page])
            .stick_to_bottom(true);
        if shown.is_empty() {
            scroll.show(ui, |ui| ui.weak("(nothing)"));
        } else {
            // (Only the lines in sight: there are up to thousands, and
            // laying them all out cost tens of milliseconds a frame on a
            // phone. Each is one line, never wrapped.)
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
            scroll.show_rows(ui, row_height, shown.len(), |ui, rows| {
                for entry in &shown[rows] {
                    let color = match entry.level {
                        log::Level::Error => egui::Color32::from_rgb(220, 90, 90),
                        log::Level::Warn => egui::Color32::from_rgb(220, 180, 80),
                        _ => ui.visuals().text_color(),
                    };
                    ui.add(egui::Label::new(egui::RichText::new(entry.line()).monospace().color(color)).extend());
                }
            });
        }
        if !page {
            ui.separator();
            log_file_note_ui(ui);
            ui.horizontal(|ui| {
                log_actions_ui(ui, &shown);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        }
        close
    }
}

/// Where the log file is.
pub(super) fn log_file_note_ui(ui: &mut egui::Ui) {
    if let Some(path) = applog::file_path() {
        ui.weak(format!("Also saved to {} (the previous run's is synththing.prev.log)", path.display()));
    }
}

/// The log's buttons: copy what's shown, open or share the file, clear.
pub(super) fn log_actions_ui(ui: &mut egui::Ui, shown: &[&applog::Entry]) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy shown").clicked() {
            let text: Vec<String> = shown.iter().map(|e| e.line()).collect();
            ui.ctx().copy_text(text.join("\n"));
        }
        // (Android has no file manager to open: the log is shared, to
        // whatever app the user picks, instead.)
        #[cfg(not(target_os = "android"))]
        if let Some(path) = applog::file_path()
            && ui.button("Open log folder").clicked()
        {
            file_manager::show_in_folder(&path);
        }
        #[cfg(target_os = "android")]
        if let Some(path) = applog::file_path()
            && ui.button("Share log").clicked()
        {
            crate::android::share_log(&path);
        }
        if ui.button("Clear").on_hover_text("Clears this view; the log file keeps everything").clicked() {
            applog::clear();
        }
    });
}
