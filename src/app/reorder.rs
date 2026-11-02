//! An ordered list as settings rows (`reorder_list`): Preferences' Media
//! controls and Controls bar, and any list the user puts in order. Each
//! row is dragged by its grip, or moved with Higher and Lower; lines across
//! the list mark its sections, and moving past one moves the row into the
//! section beyond it.

use eframe::egui;

use super::{icon, row_icon_button, setting_row};

/// One entry of a `reorder_list`: the lines over it (section names, where
/// a place's room runs out), and its row, if it has one (an entry without
/// is only its lines, a section's start: moved past, never moved).
pub(super) struct ListEntry {
    pub lines: Vec<String>,
    pub row: Option<egui::RichText>,
}

/// What's dragged in a `reorder_list`: which list (its id), and the index
/// of the entry.
#[derive(Clone, Copy)]
struct Dragged {
    list: egui::Id,
    from: usize,
}

/// An ordered list as settings rows: a grip at each row's left to drag it
/// to its place, and Higher, Lower and Take it out at its right. Lines
/// (`ListEntry::lines`) take a row each, striped in turn as the rows are.
/// `id_salt` tells it from other lists on the page.
pub(super) fn reorder_list<T>(ui: &mut egui::Ui, id_salt: &str, items: &mut Vec<T>, entry: impl Fn(usize, &T) -> ListEntry) {
    let list = ui.make_persistent_id(("reorder_list", id_salt));
    let dragging = egui::DragAndDrop::payload::<Dragged>(ui.ctx()).map(|d| *d).filter(|d| d.list == list);
    let last = items.len().saturating_sub(1);
    let mut moved = None;
    let mut remove = None;
    let mut dropped = None;
    for (i, item) in items.iter().enumerate() {
        let ListEntry { lines, row } = entry(i, item);
        let top = ui.cursor().top();
        for line in lines {
            setting_row(ui, |ui| cutoff_line(ui, &line));
        }
        if let Some(text) = row {
            setting_row(ui, |ui| {
                let grip = ui
                    .add(egui::Label::new(egui::RichText::new(icon("dots-six-vertical")).weak()).selectable(false).sense(egui::Sense::drag()))
                    .on_hover_cursor(egui::CursorIcon::Grab)
                    .on_hover_text("Drag to move it");
                grip.dnd_set_drag_payload(Dragged { list, from: i });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(row_icon_button(ui, "x")).on_hover_text("Take it out").clicked() {
                        remove = Some(i);
                    }
                    if ui.add_enabled(i < last, row_icon_button(ui, "arrow-down")).on_hover_text("Lower").clicked() {
                        moved = Some(i + 1);
                    }
                    if ui.add_enabled(i > 0, row_icon_button(ui, "arrow-up")).on_hover_text("Higher").clicked() {
                        moved = Some(i);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(egui::Label::new(text).selectable(false).truncate()));
                });
            });
        }
        // The entry's lines and row take a drop: before it when it lands on
        // the top half, after it on the bottom half.
        let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
        let Some(drag) = dragging else { continue };
        let target = ui.interact(rect, list.with(("drop", i)), egui::Sense::hover());
        let Some(pointer) = ui.ctx().pointer_interact_pos().filter(|p| rect.contains(*p)) else { continue };
        let before = pointer.y < rect.center().y;
        let to = if before { i } else { i + 1 };
        if drag.from == i {
            ui.painter().rect_filled(rect, 2.0, ui.visuals().selection.bg_fill.gamma_multiply(0.3));
        } else {
            let y = if before { rect.top() } else { rect.bottom() };
            ui.painter().hline(rect.x_range(), y, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
        }
        if target.dnd_release_payload::<Dragged>().is_some() {
            dropped = Some((drag.from, to));
        }
    }
    if let Some(i) = moved {
        items.swap(i - 1, i);
    }
    if let Some(i) = remove {
        items.remove(i);
    }
    if let Some((from, to)) = dropped {
        let item = items.remove(from);
        items.insert(if to > from { to - 1 } else { to }, item);
    }
}

/// A line across a list with `text` on it, in weak small text: where a
/// section starts, or what's above it ends (Media controls' room).
fn cutoff_line(ui: &mut egui::Ui, text: &str) {
    let stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), galley.size().y + 4.0), egui::Sense::hover());
    let y = rect.center().y;
    let text_left = rect.left() + 12.0;
    ui.painter().line_segment([egui::pos2(rect.left(), y), egui::pos2(text_left - 4.0, y)], stroke);
    ui.painter().line_segment([egui::pos2(text_left + galley.size().x + 4.0, y), egui::pos2(rect.right(), y)], stroke);
    ui.painter().galley(egui::pos2(text_left, y - galley.size().y / 2.0), galley, ui.visuals().weak_text_color());
}
