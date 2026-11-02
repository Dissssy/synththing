//! An ordered list as settings rows (`reorder_list`): Preferences' Media
//! controls and Controls bar, and any list the user puts in order. Each
//! row is dragged by its grip to its place; lines across the list mark its
//! sections, and dropping a row past one moves it into the section beyond.

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
/// to its place, and Take it out at its right. While a
/// row is dragged it follows the pointer, and the others make room for it
/// where it would land (with an empty row), showing just their names.
/// Lines (`ListEntry::lines`) take a row each, striped in turn as the rows
/// are, and belong to the place in the list, not to an entry: dropping a
/// row past one moves it into the section beyond. `id_salt` tells it from
/// other lists on the page.
pub(super) fn reorder_list<T>(ui: &mut egui::Ui, id_salt: &str, items: &mut Vec<T>, entry: impl Fn(usize, &T) -> ListEntry) {
    let list = ui.make_persistent_id(("reorder_list", id_salt));
    let landing = list.with("landing");
    let dragging = egui::DragAndDrop::payload::<Dragged>(ui.ctx()).map(|d| *d).filter(|d| d.list == list && d.from < items.len());
    let Some(drag) = dragging else {
        ui.ctx().data_mut(|d| d.remove::<usize>(landing));
        return still_list_ui(ui, list, items, entry);
    };
    // Where the dragged row would land, among the others (as worked out
    // last frame from where the pointer was).
    let from = drag.from;
    let mut to = ui.ctx().data(|d| d.get_temp::<usize>(landing)).unwrap_or(from).min(items.len() - 1);
    let mut shown: Vec<Option<usize>> = (0..items.len()).filter(|&i| i != from).map(Some).collect();
    shown.insert(to, None);
    let pointer = ui.ctx().pointer_interact_pos();
    let mut others = 0;
    let mut name = None;
    for (at, slot) in shown.iter().enumerate() {
        let ListEntry { lines, row } = entry(at, &items[slot.unwrap_or(from)]);
        let top = ui.cursor().top();
        for line in lines {
            setting_row(ui, |ui| cutoff_line(ui, &line));
        }
        let row_top = ui.cursor().top();
        if let Some(text) = &row {
            setting_row(ui, |ui| {
                // (The dragged row's grip stays, so the drag carries on.)
                grip(ui, list, slot.unwrap_or(from), slot.is_some());
                if slot.is_some() {
                    ui.add(egui::Label::new(text.clone()).selectable(false).truncate());
                } else {
                    ui.allocate_space(egui::vec2(ui.available_width(), ui.spacing().interact_size.y));
                }
            });
        }
        let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
        match slot {
            None => {
                name = row;
                let hole = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), row_top..=ui.cursor().top()).shrink(1.0);
                ui.painter().rect_stroke(hole, 4.0, egui::Stroke::new(1.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
            }
            // (Over another entry, its row or its lines: land before it on
            // its top half, after it on its bottom half.)
            Some(_) => {
                if let Some(p) = pointer.filter(|p| p.y >= rect.top() && p.y < rect.bottom()) {
                    to = if p.y < rect.center().y { others } else { others + 1 };
                }
                others += 1;
            }
        }
    }
    // The dragged row, following the pointer.
    if let (Some(p), Some(text)) = (pointer, name) {
        egui::Area::new(list.with("dragged"))
            .order(egui::Order::Tooltip)
            .fixed_pos(p + egui::vec2(-8.0, -12.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(icon("dots-six-vertical")).weak());
                        ui.add(egui::Label::new(text).selectable(false));
                    });
                });
            });
    }
    if ui.ctx().input(|i| i.pointer.any_released()) {
        ui.ctx().data_mut(|d| d.remove::<usize>(landing));
        egui::DragAndDrop::clear_payload(ui.ctx());
        let item = items.remove(from);
        items.insert(to.min(items.len()), item);
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(landing, to));
        ui.ctx().request_repaint();
    }
}

/// `reorder_list` while nothing's dragged: each row with its grip, name,
/// and Take it out.
fn still_list_ui<T>(ui: &mut egui::Ui, list: egui::Id, items: &mut Vec<T>, entry: impl Fn(usize, &T) -> ListEntry) {
    let mut remove = None;
    for (i, item) in items.iter().enumerate() {
        let ListEntry { lines, row } = entry(i, item);
        for line in lines {
            setting_row(ui, |ui| cutoff_line(ui, &line));
        }
        let Some(text) = row else { continue };
        setting_row(ui, |ui| {
            grip(ui, list, i, true);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(row_icon_button(ui, "x")).on_hover_text("Take it out").clicked() {
                    remove = Some(i);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(egui::Label::new(text).selectable(false).truncate()));
            });
        });
    }
    if let Some(i) = remove {
        items.remove(i);
    }
}

/// A row's grip, at its left: dragging it drags entry `index` (its id
/// stays the entry's, wherever the row is drawn, so a drag carries on as
/// the rows make room). Drawn faint when `shown` is false (its row is the
/// empty place the dragged one would land in).
fn grip(ui: &mut egui::Ui, list: egui::Id, index: usize, shown: bool) {
    let glyph = icon("dots-six-vertical");
    let font = egui::TextStyle::Body.resolve(ui.style());
    let width = ui.fonts_mut(|f| f.layout_no_wrap(glyph.to_string(), font.clone(), egui::Color32::WHITE).size().x);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, ui.spacing().interact_size.y), egui::Sense::hover());
    let response = ui.interact(rect, list.with(("grip", index)), egui::Sense::drag()).on_hover_cursor(egui::CursorIcon::Grab);
    if shown {
        let response = response.on_hover_text("Drag to move it");
        response.dnd_set_drag_payload(Dragged { list, from: index });
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, glyph, font, ui.visuals().weak_text_color());
    } else {
        response.dnd_set_drag_payload(Dragged { list, from: index });
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
