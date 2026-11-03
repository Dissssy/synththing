//! The Soundfonts panel: the list, adding and removing soundfonts.

use eframe::egui;

use crate::config::nice_name;
use crate::select_list;

use super::{App, DraggedSoundfont, file_manager, icon, icon_button, placeholder, shift_active};

impl App {
    pub(super) fn soundfont_ui(&mut self, ui: &mut egui::Ui) {
        self.starter_progress_ui(ui);
        let mobile = self.mobile();
        let mut browse = false;
        let count = match self.config.soundfonts.len() {
            1 => "1 soundfont".to_string(),
            n => format!("{n} soundfonts"),
        };
        let left = |ui: &mut egui::Ui| {
            if mobile {
                if ui.button(format!("{}  Import soundfonts...", icon("plus"))).on_hover_text("Copy soundfonts into synththing").clicked() {
                    crate::import::start(crate::import::Kind::Soundfonts, &["sf2", "sf3"]);
                }
            } else {
                browse = ui.add(icon_button("plus")).on_hover_text("Add a soundfont...").clicked();
            }
        };
        select_list::toolbar(ui, left, count, |_| {});
        if browse {
            self.open_soundfont_browser();
        }

        let active_sf = self.active_sf;
        let touch = select_list::touch(ui.ctx());
        // The header, as the Queue's (Name, then the size at the right).
        let size_width = if self.config.soundfonts.is_empty() {
            placeholder(ui, "No soundfonts yet.\nAdd one with + to play MIDI songs.");
            0.0
        } else if touch {
            0.0
        } else {
            let id = egui::Id::unique("soundfonts_header");
            let widths: select_list::Widths = ui.ctx().data_mut(|d| d.get_persisted(id.with("widths"))).unwrap_or_default();
            let width = select_list::column_width(ui.ctx(), id, "size", &widths, SOUNDFONT_SIZE_WIDTH);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ui.spacing().interact_size.y), egui::Sense::hover());
            let strong = |text: &str| egui::RichText::new(text).strong();
            let columns = [select_list::Column { key: "size", title: strong("Size"), width, numeric: true }];
            let header = select_list::Header { title: strong("Name"), columns: &columns, lead: 0.0, trail: 0.0, min_main: 120.0 };
            if let Some((key, width)) = select_list::header(ui, id, rect, header).resized {
                let mut widths = widths;
                match width {
                    Some(width) => widths.insert(key, width),
                    None => widths.remove(&key),
                };
                ui.ctx().data_mut(|d| d.insert_persisted(id.with("widths"), widths));
            }
            width
        };
        let mut remove = None;
        let mut use_idx = None;
        let output = select_list::show(ui, "soundfonts", &self.config.soundfonts, &mut self.soundfont_selection, active_sf, |ui, row, path| {
            if row.response.drag_started() {
                row.response.dnd_set_drag_payload(DraggedSoundfont(path.clone()));
            }
            crate::menu::context(&row.response, |ui| {
                if ui.button("Use").clicked() {
                    use_idx = Some(row.idx);
                }
                let rows = row.rows();
                let count = if rows.len() > 1 { format!(" ({})", rows.len()) } else { String::new() };
                if ui.button(format!("Remove from the list{count}")).clicked() {
                    remove = Some(rows);
                }
                #[cfg(not(target_os = "android"))]
                file_manager::show_in_folder_button(ui, path);
            });
            let active = Some(row.idx) == active_sf;
            let text = egui::RichText::new(nice_name(path));
            let text = if active { text.strong() } else { text };
            let scroll = select_list::scrolls(row, active);
            // Its size, or that it's gone.
            let size = std::fs::metadata(path).map_or_else(|_| "missing".to_string(), |m| crate::storage::size_text(m.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if touch {
                    let detail = if active { format!("in use · {size}") } else { size };
                    select_list::two_lines(ui, std::iter::empty(), text, Some(detail), scroll);
                    return;
                }
                select_list::cell(ui, size_width, egui::RichText::new(size).weak(), false, true);
                let hover = format!(
                    "{}\nClick to use; drag onto a MIDI playlist entry to override its soundfont",
                    path.display()
                );
                select_list::label(ui, text, Some(hover), scroll);
            });
        });
        if output.delete {
            remove = Some(self.soundfont_selection.rows().to_vec());
        }

        if let Some(i) = output.activated.or(use_idx) {
            self.activate_soundfont(i);
        }
        if let Some(rows) = remove {
            self.remove_soundfonts(&rows);
        }
    }

    /// Take the soundfonts at `indices` off the list.
    pub(super) fn remove_soundfonts(&mut self, indices: &[usize]) {
        let mut indices: Vec<usize> = indices.iter().copied().filter(|&i| i < self.config.soundfonts.len()).collect();
        // Highest first, so the rest stay valid.
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        let names: Vec<String> = indices.iter().map(|&i| nice_name(&self.config.soundfonts[i])).collect();
        for &idx in &indices {
            self.config.remove_soundfont(idx);
            self.active_sf = shift_active(self.active_sf, idx);
        }
        self.soundfont_selection.clear();
        self.pending_soundfont = None;
        self.adding_soundfonts.forget_removed(&self.config.soundfonts);
        let removed = match names.as_slice() {
            [] => return,
            [name] => format!("soundfont: {name}"),
            more => format!("{} soundfonts", more.len()),
        };
        self.status = match self.config.save() {
            Err(e) => format!("Removed {removed}, but failed to save list: {e}"),
            Ok(()) => format!("Removed {removed}"),
        };
    }

    /// Browse for soundfonts to add: one or several at once. (On
    /// Android, import them: `import.rs`.)
    pub(super) fn open_soundfont_browser(&mut self) {
        #[cfg(target_os = "android")]
        crate::import::start(crate::import::Kind::Soundfonts, &["sf2", "sf3"]);
        #[cfg(not(target_os = "android"))]
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("SoundFont", &["sf2", "sf3"])
            .pick_files()
        {
            self.add_soundfonts(paths);
        }
    }
}

/// The Soundfonts list's Size column's own width.
pub(super) const SOUNDFONT_SIZE_WIDTH: f32 = 72.0;
