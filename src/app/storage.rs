//! Preferences > Library > Storage: how much space synththing's own files
//! take (`crate::storage`), counted afresh each time it's shown.

use std::sync::mpsc::{self, Receiver};

use eframe::egui;

use super::App;
use crate::storage::{self, Size, Usage};

#[derive(Default)]
pub struct StorageState {
    /// The last count, or why there isn't one.
    usage: Option<Result<Usage, String>>,
    /// Being counted: the count, when it's done.
    counting: Option<Receiver<Result<Usage, String>>>,
    /// The pass it was last drawn in: not drawn the pass before means it's
    /// just been opened, so it's counted again.
    drawn_pass: u64,
}

impl App {
    pub(super) fn storage_ui(&mut self, ui: &mut egui::Ui) {
        let state = &mut self.storage;
        let pass = ui.ctx().cumulative_pass_nr();
        let reopened = pass > state.drawn_pass + 1;
        state.drawn_pass = pass;
        if let Some(rx) = &state.counting {
            match rx.try_recv() {
                Ok(usage) => {
                    state.usage = Some(usage);
                    state.counting = None;
                }
                Err(mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
                Err(mpsc::TryRecvError::Disconnected) => state.counting = None,
            }
        }

        let title = match &state.usage {
            Some(Ok(usage)) => format!("Storage \u{b7} {}", storage::size_text(usage.total())),
            _ if state.counting.is_some() => "Storage \u{b7} counting...".into(),
            _ => "Storage".into(),
        };
        let mut repair = false;
        super::settings_group(&title).icon("hard-drives").show(ui, |ui| {
            match &state.usage {
                Some(Ok(usage)) => {
                    let total = usage.total().max(1);
                    row(ui, "Songs", usage.songs, total);
                    row(ui, "Soundfonts", usage.soundfonts, total);
                    row(ui, "Everything else", usage.other, total).on_hover_text("Settings, playlists, scripts, the log and sync state");
                }
                Some(Err(e)) => {
                    super::setting_row(ui, |ui| ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't count it: {e}")));
                }
                None => {}
            }
            super::with_info(ui, "Check every song: damaged ones are fetched again, unplayable ones can be removed.", |ui| {
                let button = ui.button(format!("{}  Repair library...", super::icon("wrench")));
                repair = button.clicked();
                button
            });
        });

        if (reopened || state.usage.is_none()) && state.counting.is_none() {
            let (tx, rx) = mpsc::channel();
            state.counting = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let _ = tx.send(storage::measure().map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
        }
        if repair {
            self.open_repair();
        }
    }
}

/// A kind of file: its name, a bar of its share of `total`, its size and
/// how many files.
fn row(ui: &mut egui::Ui, name: &str, size: Size, total: u64) -> egui::Response {
    super::setting_row(ui, |ui| {
        let label = super::setting_label(ui, name);
        let width = super::CONTROL_WIDTH;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
        let visuals = ui.visuals();
        ui.painter().rect_filled(rect, 3.0, visuals.extreme_bg_color);
        let share = size.bytes as f32 / total as f32;
        let filled = egui::Rect::from_min_size(rect.min, egui::vec2((width * share).max(if size.bytes > 0 { 2.0 } else { 0.0 }), rect.height()));
        ui.painter().rect_filled(filled, 3.0, visuals.selection.bg_fill);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.weak(format!("{} file{}", size.files, if size.files == 1 { "" } else { "s" }));
            ui.label(storage::size_text(size.bytes));
        });
        label
    })
}
