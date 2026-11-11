//! Help > Log: the app's log, filtered, with its actions.

use std::time::Duration;

use eframe::egui;

use crate::applog;

use super::App;

impl App {
    /// The log: level filter, search, its actions and the log file's
    /// location, then the entries filling what's left.
    pub(super) fn log_body_ui(&mut self, ui: &mut egui::Ui) {
        let entries = applog::entries();
        let search = self.log_search.to_lowercase();
        let shown: Vec<&applog::Entry> = entries
            .iter()
            .filter(|e| e.level <= self.log_min_level)
            .filter(|e| search.is_empty() || e.line().to_lowercase().contains(&search))
            .collect();
        ui.ctx().request_repaint_after(Duration::from_millis(250));

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
        log_actions_ui(ui, &shown);
        log_file_note_ui(ui);
        ui.separator();
        let scroll = egui::ScrollArea::both()
            .id_salt("app_log_scroll")
            .auto_shrink([false, false])
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
                        log::Level::Error => ui.visuals().error_fg_color,
                        log::Level::Warn => ui.visuals().warn_fg_color,
                        _ => ui.visuals().text_color(),
                    };
                    ui.add(egui::Label::new(egui::RichText::new(entry.line()).monospace().color(color)).extend());
                }
            });
        }
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
            super::file_manager::show_in_folder(&path);
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
