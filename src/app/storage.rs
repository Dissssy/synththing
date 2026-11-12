//! Preferences > Library > Storage: how much space synththing's own files
//! take (`crate::storage`), counted afresh each time it's shown, the room
//! left on this device's disk, and on the hub's, as it last said.

use std::sync::mpsc::{self, Receiver};

use eframe::egui;

use super::App;
use crate::song_store::KEEP_FREE;
use crate::storage::{self, Disk, Size, Usage};

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
        let hub = self.hub_room();
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
                    if let Some(disk) = usage.disk {
                        disk_row(ui, "This device's disk", disk.used(), disk.total, format!("{} free", storage::size_text(disk.free)), false);
                    }
                }
                Some(Err(e)) => {
                    super::setting_row(ui, |ui| ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't count it: {e}")));
                }
                None => {}
            }
            // (The hub keeps `KEEP_FREE` for the rest of what it keeps: its
            // songs have the rest, which is what's shown.)
            if let Some((Some(Disk { free, total }), full)) = hub {
                let room = total.saturating_sub(KEEP_FREE);
                let used = total.saturating_sub(free).min(room);
                let text = if full { "full".to_string() } else { format!("{} of {}", storage::size_text(used), storage::size_text(room)) };
                disk_row(ui, "Your hub", used, room, text, full).on_hover_text(if full {
                    HUB_FULL_HINT
                } else {
                    "The room your hub has for songs, and how much of it is taken"
                });
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

/// What a full hub means, on hovering where it says so.
pub(super) const HUB_FULL_HINT: &str = "Your hub has no room for more songs: new songs stay on your devices until some are removed or it's given more room";

/// The room a `row`'s size and file count take after its bar: about
/// "123.4 MB  1234 files", and the gaps.
const COUNTS_WIDTH: f32 = 120.0;

/// A kind of file: its name, a bar of its share of `total`, its size and
/// how many files.
fn row(ui: &mut egui::Ui, name: &str, size: Size, total: u64) -> egui::Response {
    super::setting_row(ui, |ui| {
        let label = super::setting_label(ui, name);
        bar(ui, size.bytes, total, false);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.weak(format!("{} file{}", size.files, if size.files == 1 { "" } else { "s" }));
            ui.label(storage::size_text(size.bytes));
        });
        label
    })
}

/// A disk: its name, a bar of how much of `total` is `used`, and `text`
/// (in the warning colour, and the bar too, when it's `full`).
fn disk_row(ui: &mut egui::Ui, name: &str, used: u64, total: u64, text: String, full: bool) -> egui::Response {
    super::setting_row(ui, |ui| {
        let label = super::setting_label(ui, name);
        bar(ui, used, total.max(1), full);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if full {
                ui.colored_label(ui.visuals().warn_fg_color, text);
            } else {
                ui.label(text);
            }
        });
        label
    })
}

/// A bar `part`'s share of `total` full, leaving `COUNTS_WIDTH` after it.
fn bar(ui: &mut egui::Ui, part: u64, total: u64, warn: bool) {
    let width = super::control_width(ui, COUNTS_WIDTH);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
    let visuals = ui.visuals();
    ui.painter().rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let share = (part as f32 / total as f32).min(1.0);
    let filled = egui::Rect::from_min_size(rect.min, egui::vec2((width * share).max(if part > 0 { 2.0 } else { 0.0 }), rect.height()));
    ui.painter().rect_filled(filled, 3.0, if warn { visuals.warn_fg_color } else { visuals.selection.bg_fill });
}
