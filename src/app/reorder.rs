//! An ordered list as settings rows (`reorder_list`): Preferences' Media
//! controls and Controls bar, and any list the user puts in order. Each
//! row is dragged by its grip to its place, or onto the trash in its
//! group's heading (`trash_ui`) to take it out; lines across the list mark
//! its sections, and dropping a row past one moves it into the section
//! beyond.

use eframe::egui;

use super::{icon, setting_row};

/// One entry of a `reorder_list`: the lines over it (section names, where
/// a place's room runs out), and whether it has a row (an entry without
/// is only its lines, a section's start: moved past, never moved).
pub(super) struct ListEntry {
    pub lines: Vec<String>,
    pub row: bool,
}

/// What's dragged in a `reorder_list`: which list (its id), and the index
/// of the entry.
#[derive(Clone, Copy)]
struct Dragged {
    list: egui::Id,
    from: usize,
}

/// The id of the list `id_salt` names: one per page, shared by the list
/// and its trash.
fn list_id(id_salt: &str) -> egui::Id {
    egui::Id::new(("reorder_list", id_salt))
}

/// The entry of list `list` being dragged, if one is (and it's still in a
/// list of `len`).
fn dragged(ctx: &egui::Context, list: egui::Id, len: usize) -> Option<usize> {
    egui::DragAndDrop::payload::<Dragged>(ctx).filter(|d| d.list == list && d.from < len).map(|d| d.from)
}

/// An ordered list as settings rows, each one a grip at its left to drag
/// it to its place, then what `row` draws (one line high). While a row is
/// dragged it follows the pointer, the others make room for it where it
/// would land (with an empty row), and what they draw doesn't answer the
/// pointer. Lines (`ListEntry::lines`) take a row each, striped in turn as
/// the rows are, and belong to the place in the list, not to an entry:
/// dropping a row past one moves it into the section beyond. `id_salt`
/// tells it from other lists on the page.
///
/// Gives back the entry dropped on the list's trash (`trash_ui`, in the
/// group's heading, drawn before the list), for the caller to take out (or
/// ask first).
pub(super) fn reorder_list<T>(
    ui: &mut egui::Ui,
    id_salt: &str,
    items: &mut Vec<T>,
    entry: impl Fn(usize, &T) -> ListEntry,
    mut row: impl FnMut(&mut egui::Ui, &T),
) -> Option<usize> {
    let list = list_id(id_salt);
    let landing = list.with("landing");
    let Some(from) = dragged(ui.ctx(), list, items.len()) else {
        ui.ctx().data_mut(|d| d.remove::<usize>(landing));
        for (i, item) in items.iter().enumerate() {
            let ListEntry { lines, row: has_row } = entry(i, item);
            for line in lines {
                setting_row(ui, |ui| cutoff_line(ui, &line));
            }
            if has_row {
                setting_row(ui, |ui| {
                    grip(ui, list, i, true);
                    row(ui, item);
                });
            }
        }
        return None;
    };
    // Where the dragged row would land, among the others (as worked out
    // last frame from where the pointer was).
    let mut to = ui.ctx().data(|d| d.get_temp::<usize>(landing)).unwrap_or(from).min(items.len() - 1);
    let mut shown: Vec<Option<usize>> = (0..items.len()).filter(|&i| i != from).map(Some).collect();
    shown.insert(to, None);
    let pointer = ui.ctx().pointer_interact_pos();
    let width = ui.available_width();
    let mut others = 0;
    for (at, slot) in shown.iter().enumerate() {
        let item = &items[slot.unwrap_or(from)];
        let ListEntry { lines, row: has_row } = entry(at, item);
        let top = ui.cursor().top();
        for line in lines {
            setting_row(ui, |ui| cutoff_line(ui, &line));
        }
        let row_top = ui.cursor().top();
        if has_row {
            setting_row(ui, |ui| {
                // (The dragged row's grip stays, so the drag carries on.)
                grip(ui, list, slot.unwrap_or(from), slot.is_some());
                // (The empty row draws the dragged one unseen, so it's as
                // tall as the others.)
                let builder = if slot.is_some() { egui::UiBuilder::new().disabled() } else { egui::UiBuilder::new().invisible() };
                ui.scope_builder(builder, |ui| row(ui, item));
            });
        }
        let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
        match slot {
            None => {
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
    // The dragged row, following the pointer (gone into the trash while
    // it's over it, so the trash shows).
    let over_trash = ui.ctx().data(|d| d.get_temp::<bool>(list.with("trash"))).unwrap_or(false);
    if let Some(p) = pointer.filter(|_| !over_trash) {
        egui::Area::new(list.with("dragged"))
            .order(egui::Order::Tooltip)
            .fixed_pos(p + egui::vec2(-8.0, -12.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(icon("dots-six-vertical")).weak());
                        row(ui, &items[from]);
                    });
                });
            });
    }
    if !ui.ctx().input(|i| i.pointer.any_released()) {
        ui.ctx().data_mut(|d| d.insert_temp(landing, to));
        ui.ctx().request_repaint();
        return None;
    }
    ui.ctx().data_mut(|d| d.remove::<usize>(landing));
    egui::DragAndDrop::clear_payload(ui.ctx());
    if over_trash {
        return Some(from);
    }
    let item = items.remove(from);
    items.insert(to.min(items.len()), item);
    None
}

/// The trash of list `id_salt` (of `len` entries), for its group's heading
/// (drawn before the list): there only while one of its rows is dragged,
/// and red, its lid open, while it's over it, to be dropped and taken out.
pub(super) fn trash_ui(ui: &mut egui::Ui, id_salt: &str, len: usize) {
    let list = list_id(id_salt);
    let over = list.with("trash");
    if dragged(ui.ctx(), list, len).is_none() {
        ui.ctx().data_mut(|d| d.remove::<bool>(over));
        return;
    }
    let height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(height + 6.0, height), egui::Sense::hover());
    let hovered = ui.rect_contains_pointer(rect);
    ui.ctx().data_mut(|d| d.insert_temp(over, hovered));
    let color = if hovered { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
    if hovered {
        ui.painter().rect_filled(rect, 4.0, color.gamma_multiply(0.15));
    }
    trash_can(ui.painter(), rect.center(), height - 6.0, color, hovered);
}

/// A trash can `size` tall at `center`, as Phosphor's trash, its lid
/// tipped open when `open`.
fn trash_can(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32, open: bool) {
    let stroke = egui::Stroke::new((size / 11.0).max(1.0), color);
    let at = |x: f32, y: f32| center + egui::vec2(x, y) * size;
    // The can, its two ribs.
    painter.add(egui::Shape::line(vec![at(-0.3, -0.25), at(-0.3, 0.5), at(0.3, 0.5), at(0.3, -0.25)], stroke));
    for x in [-0.1, 0.1] {
        painter.line_segment([at(x, -0.05), at(x, 0.3)], stroke);
    }
    // The lid and its handle, tipped up about the lid's left end.
    let pivot = at(-0.42, -0.3);
    let tip = egui::emath::Rot2::from_angle(if open { -0.45 } else { 0.0 });
    let lid = |x: f32, y: f32| pivot + tip * (at(x, y) - pivot) + if open { egui::vec2(0.0, -size * 0.08) } else { egui::Vec2::ZERO };
    painter.line_segment([lid(-0.42, -0.3), lid(0.42, -0.3)], stroke);
    painter.add(egui::Shape::line(vec![lid(-0.14, -0.3), lid(-0.14, -0.5), lid(0.14, -0.5), lid(0.14, -0.3)], stroke));
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
        let response = response.on_hover_text("Drag to move it, or onto the trash to take it out");
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
