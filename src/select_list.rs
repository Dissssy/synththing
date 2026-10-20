//! The list the Songs, Soundfonts and Playlists panels share: rows of one
//! height, click to select (Ctrl toggles, Shift extends a range), double-click
//! to open or play, Ctrl+A to select all, and Delete reported to the list
//! that owns the rows. Each list draws its own row contents with a closure
//! given the entry; [`label`] is the entry's text, cut to the width left
//! for it with the full text on hover.
//!
//! For touch (the mobile layout, [`set_touch`]) rows are taller, a tap
//! opens or plays a row, a long press brings up its menu, and dragging
//! scrolls the list instead of dragging rows.

use eframe::egui;

/// Which rows of a list are selected, and where a Shift+click range starts.
#[derive(Default)]
pub struct Selection {
    /// Sorted, no duplicates.
    rows: Vec<usize>,
    /// The last plain or Ctrl click.
    anchor: Option<usize>,
}

impl Selection {
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn contains(&self, idx: usize) -> bool {
        self.rows.binary_search(&idx).is_ok()
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.anchor = None;
    }

    pub fn select_all(&mut self, len: usize) {
        self.rows = (0..len).collect();
    }

    /// The rows an action on row `idx` applies to: the whole selection if
    /// `idx` is part of it, otherwise just `idx`.
    pub fn rows_for(&self, idx: usize) -> Vec<usize> {
        if self.contains(idx) { self.rows.clone() } else { vec![idx] }
    }

    /// Follow the rows to where a reorder put them: `positions[old]` is
    /// each row's new index.
    pub fn remap(&mut self, positions: &[usize]) {
        let mut rows: Vec<usize> = self.rows.iter().filter_map(|&i| positions.get(i).copied()).collect();
        rows.sort_unstable();
        self.rows = rows;
        self.anchor = self.anchor.and_then(|i| positions.get(i).copied());
    }

    /// A click on row `idx`, by the modifier keys held: plain selects just
    /// it, Ctrl toggles it, Shift extends a range from the anchor (adding to
    /// the selection with Ctrl too).
    fn click(&mut self, idx: usize, modifiers: egui::Modifiers) {
        if modifiers.shift
            && let Some(anchor) = self.anchor
        {
            if !modifiers.command {
                self.rows.clear();
            }
            self.rows.extend(anchor.min(idx)..=anchor.max(idx));
            self.rows.sort_unstable();
            self.rows.dedup();
            return;
        }
        if modifiers.command {
            match self.rows.binary_search(&idx) {
                Ok(pos) => {
                    self.rows.remove(pos);
                }
                Err(pos) => self.rows.insert(pos, idx),
            }
        } else {
            self.rows = vec![idx];
        }
        self.anchor = Some(idx);
    }
}

/// The row height for touch: about a fingertip.
const TOUCH_ROW_HEIGHT: f32 = 44.0;

fn touch_id() -> egui::Id {
    egui::Id::new("select_list_touch")
}

/// Lay lists out for touch (or not) from now on.
pub fn set_touch(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(touch_id(), on));
}

/// Whether lists are laid out for touch.
pub fn touch(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(touch_id())).unwrap_or(false)
}

/// One row, as its drawing closure sees it.
pub struct Row<'a> {
    pub idx: usize,
    /// The whole row: drag payloads, right-click menus, drop targets and
    /// row-wide tooltips go on this.
    pub response: egui::Response,
    selection: &'a Selection,
}

impl Row<'_> {
    /// The rows an action on this row applies to (see [`Selection::rows_for`]).
    pub fn rows(&self) -> Vec<usize> {
        self.selection.rows_for(self.idx)
    }
}

/// What happened in a list this frame.
#[derive(Default)]
pub struct ListOutput {
    /// The row double-clicked (tapped, for touch).
    pub activated: Option<usize>,
    /// The row under the pointer.
    pub hovered: Option<usize>,
    /// Delete was pressed over the list with rows selected.
    pub delete: bool,
}

/// Draw `items` as a scrolling list filling `ui`, each row's contents drawn
/// left to right by `draw` on top of the row's own click-and-drag area
/// (widgets it adds, such as buttons, take their clicks first).
pub fn show<T>(
    ui: &mut egui::Ui,
    id_salt: impl egui::AsId,
    items: &[T],
    selection: &mut Selection,
    mut draw: impl FnMut(&mut egui::Ui, &Row, &T),
) -> ListOutput {
    let id = egui::Id::new(id_salt);
    let mut output = ListOutput::default();
    let touch = touch(ui.ctx());
    let row_height = if touch { TOUCH_ROW_HEIGHT } else { ui.spacing().interact_size.y };
    let sense = if touch { egui::Sense::click() } else { egui::Sense::click_and_drag() };
    let modifiers = ui.input(|i| i.modifiers);
    let scroll = egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show_rows(ui, row_height, items.len(), |ui, range| {
            for idx in range {
                let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width(), row_height));
                // Not focusable: a focused row would count as typing and turn the keys off.
                let response = ui.interact(rect, id.with(idx), sense - egui::Sense::FOCUSABLE);
                if response.double_clicked() || (touch && response.clicked()) {
                    output.activated = Some(idx);
                } else if response.clicked() {
                    selection.click(idx, modifiers);
                }
                if response.contains_pointer() {
                    output.hovered = Some(idx);
                }

                let selected = selection.contains(idx);
                let visuals = ui.style().interact_selectable(&response, selected);
                if selected || response.hovered() {
                    ui.painter().rect(
                        rect.expand(visuals.expansion),
                        visuals.corner_radius,
                        visuals.weak_bg_fill,
                        visuals.bg_stroke,
                        egui::StrokeKind::Inside,
                    );
                }

                let row = Row { idx, response, selection };
                let builder = egui::UiBuilder::new()
                    .id_salt(idx)
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center));
                ui.scope_builder(builder, |ui| {
                    // Selectable text would take the row's clicks and drags.
                    ui.style_mut().interaction.selectable_labels = false;
                    if selected {
                        ui.visuals_mut().override_text_color = Some(visuals.text_color());
                    }
                    draw(ui, &row, &items[idx]);
                });
            }
        });

    // Keys, while the pointer is over the list and nothing's being typed.
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    if !typing && ui.rect_contains_pointer(scroll.inner_rect) {
        let (select_all, delete) = ui.input_mut(|i| {
            (i.consume_key(egui::Modifiers::COMMAND, egui::Key::A), i.key_pressed(egui::Key::Delete))
        });
        if select_all {
            selection.select_all(items.len());
        }
        output.delete = delete && !selection.is_empty();
    }
    output
}

/// An entry's text, filling the width left in the row and cut short with
/// "..." where it doesn't fit; hovering a cut one shows all of it, or
/// `hover` instead whenever it's given (so that should hold the full text
/// too). Draw a row's right-hand widgets first (in a right-to-left layout),
/// then this.
pub fn label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, hover: Option<String>) -> egui::Response {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let label = egui::Label::new(text).truncate().selectable(false);
        match hover {
            Some(hover) => ui.add(label.show_tooltip_when_elided(false)).on_hover_text(hover),
            None => ui.add(label),
        }
    })
    .inner
}

/// A tag's chip: its name on its color (`song_store::Tag`), in text that
/// reads on it.
pub fn chip(ui: &mut egui::Ui, name: &str, color: [u8; 3]) -> egui::Response {
    let [r, g, b] = color;
    let light = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b) > 150.0;
    let text = if light { egui::Color32::BLACK } else { egui::Color32::WHITE };
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(r, g, b))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(5, 0))
        .show(ui, |ui| ui.add(egui::Label::new(egui::RichText::new(name).small().color(text)).selectable(false)))
        .response
}

/// Tags' chips, in a row (right to left in a right-to-left layout).
pub fn chips<'a>(ui: &mut egui::Ui, tags: impl DoubleEndedIterator<Item = (&'a str, [u8; 3])>) {
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 3.0;
    if ui.layout().prefer_right_to_left() {
        for (name, color) in tags.rev() {
            chip(ui, name, color);
        }
    } else {
        for (name, color) in tags {
            chip(ui, name, color);
        }
    }
    ui.spacing_mut().item_spacing.x = spacing;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicks(selection: &mut Selection, clicks: &[(usize, egui::Modifiers)]) -> Vec<usize> {
        for &(idx, modifiers) in clicks {
            selection.click(idx, modifiers);
        }
        selection.rows().to_vec()
    }

    const PLAIN: egui::Modifiers = egui::Modifiers::NONE;
    const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;
    const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;

    #[test]
    fn plain_click_selects_just_that_row() {
        let mut selection = Selection::default();
        assert_eq!(clicks(&mut selection, &[(2, PLAIN), (5, PLAIN)]), vec![5]);
    }

    #[test]
    fn ctrl_click_toggles() {
        let mut selection = Selection::default();
        assert_eq!(clicks(&mut selection, &[(4, PLAIN), (1, CTRL), (6, CTRL), (4, CTRL)]), vec![1, 6]);
    }

    #[test]
    fn shift_click_selects_a_range_from_the_anchor() {
        let mut selection = Selection::default();
        assert_eq!(clicks(&mut selection, &[(5, PLAIN), (2, SHIFT)]), vec![2, 3, 4, 5]);
        // The anchor stays put, so a second Shift+click re-ranges from it.
        assert_eq!(clicks(&mut selection, &[(7, SHIFT)]), vec![5, 6, 7]);
        // Ctrl+Shift adds the range to what's there.
        assert_eq!(clicks(&mut selection, &[(0, CTRL), (1, CTRL | SHIFT)]), vec![0, 1, 5, 6, 7]);
    }

    #[test]
    fn rows_for_is_the_selection_only_when_the_row_is_in_it() {
        let mut selection = Selection::default();
        clicks(&mut selection, &[(1, PLAIN), (3, CTRL)]);
        assert_eq!(selection.rows_for(3), vec![1, 3]);
        assert_eq!(selection.rows_for(2), vec![2]);
    }

    #[test]
    fn remap_follows_a_reorder() {
        let mut selection = Selection::default();
        clicks(&mut selection, &[(0, PLAIN), (2, CTRL)]);
        // Rows 0, 1, 2 became 2, 0, 1.
        selection.remap(&[2, 0, 1]);
        assert_eq!(selection.rows(), &[1, 2]);
        assert_eq!(clicks(&mut selection, &[(0, SHIFT)]), vec![0, 1]);
    }
}
