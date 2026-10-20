//! Preferences > App > Storage: how much space synththing's own files
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

        ui.horizontal(|ui| {
            ui.strong("Storage");
            if let Some(Ok(usage)) = &state.usage {
                ui.label(storage::size_text(usage.total()));
            }
            if state.counting.is_some() {
                ui.spinner();
            }
        });
        match &state.usage {
            Some(Ok(usage)) => {
                egui::Grid::new("preferences_storage").num_columns(2).show(ui, |ui| {
                    row(ui, "Songs", usage.songs);
                    row(ui, "Soundfonts", usage.soundfonts);
                    row(ui, "Settings, playlists and the rest", usage.other);
                });
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("Couldn't count it: {e}"));
            }
            None => {}
        }

        if (reopened || state.usage.is_none()) && state.counting.is_none() {
            let (tx, rx) = mpsc::channel();
            state.counting = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let _ = tx.send(storage::measure().map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
        }
    }
}

fn row(ui: &mut egui::Ui, name: &str, size: Size) {
    ui.label(name);
    ui.label(format!(
        "{} ({} file{})",
        storage::size_text(size.bytes),
        size.files,
        if size.files == 1 { "" } else { "s" }
    ));
    ui.end_row();
}
