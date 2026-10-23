//! The list the Songs, Soundfonts and Playlists panels share: rows of one
//! height, click to open or play (leaving the selection be), Ctrl+click
//! and Shift+click to select (Ctrl toggles, Shift extends a range), Ctrl+A to select all, and Delete reported to the list
//! that owns the rows. Each list draws its own row contents with a closure
//! given the entry; [`label`] is the entry's text, cut to the width left
//! for it with the full text on hover.
//!
//! The list in front (the one in the focused tab, [`set_keyboard`]) is
//! also steered from the keys: [`steer`] moves its cursor row (Shift
//! held grows the selection with it) or opens the row under it.
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
    /// The last plain or Ctrl click, or plain cursor move.
    anchor: Option<usize>,
    /// The row the keys move from: the last one clicked or moved to.
    cursor: Option<usize>,
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
        self.cursor = None;
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
        self.cursor = self.cursor.and_then(|i| positions.get(i).copied());
    }

    /// The cursor moved to row `idx` from the keys: it alone is selected,
    /// or with `extend` (Shift held), the rows from the anchor to it.
    fn move_to(&mut self, idx: usize, extend: bool) {
        self.cursor = Some(idx);
        match self.anchor {
            Some(anchor) if extend => self.rows = (anchor.min(idx)..=anchor.max(idx)).collect(),
            _ => {
                self.rows = vec![idx];
                self.anchor = Some(idx);
            }
        }
    }

    /// A click on row `idx`, by the modifier keys held: plain selects just
    /// it, Ctrl toggles it, Shift extends a range from the anchor (adding to
    /// the selection with Ctrl too).
    fn click(&mut self, idx: usize, modifiers: egui::Modifiers) {
        self.cursor = Some(idx);
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
/// The room kept clear inside a row's ends, about as much as above and
/// below its text (and clear of the selection's edge).
pub const EDGE: f32 = 4.0;

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

/// A move from the keys, for the list in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steer {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
    /// Open or play the cursor's row, as a click would.
    Open,
}

fn keyboard_id() -> egui::Id {
    egui::Id::new("select_list_keyboard")
}

fn steers_id() -> egui::Id {
    egui::Id::new("select_list_steers")
}

/// Which list the keys steer this frame, by the `id_salt` it's shown with
/// (or none). Called each frame before any [`steer`]: moves a list that
/// wasn't shown last frame didn't take are dropped.
pub fn set_keyboard(ctx: &egui::Context, list: Option<&'static str>) {
    ctx.data_mut(|d| {
        d.insert_temp(keyboard_id(), list.map(egui::Id::new));
        d.remove::<Vec<(Steer, bool)>>(steers_id());
    });
}

/// Steer the list in front, when it's next shown: with `extend` (Shift
/// held) the selection grows with the cursor.
pub fn steer(ctx: &egui::Context, steer: Steer, extend: bool) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<(Steer, bool)>>(steers_id()).push((steer, extend)));
}

/// Carry out `steers` on a list of `len` rows, with `page` rows in view:
/// the row the cursor ends up on, if it moved. The first move starts on
/// the `playing` row (else the first) when there's no cursor yet.
fn apply(
    selection: &mut Selection,
    steers: &[(Steer, bool)],
    len: usize,
    page: usize,
    playing: Option<usize>,
    output: &mut ListOutput,
) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let mut moved = None;
    for &(steer, extend) in steers {
        let at = selection.cursor.map(|c| c.min(last));
        let to = match (steer, at) {
            (Steer::Open, _) => {
                output.activated = at;
                continue;
            }
            (Steer::First, _) => 0,
            (Steer::Last, _) => last,
            (_, None) => playing.unwrap_or(0).min(last),
            (Steer::Up, Some(at)) => at.saturating_sub(1),
            (Steer::Down, Some(at)) => (at + 1).min(last),
            (Steer::PageUp, Some(at)) => at.saturating_sub(page),
            (Steer::PageDown, Some(at)) => (at + page).min(last),
        };
        selection.move_to(to, extend);
        moved = Some(to);
    }
    moved
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
    /// The row clicked (tapped, for touch) without Ctrl or Shift.
    pub activated: Option<usize>,
    /// The row under the pointer.
    pub hovered: Option<usize>,
    /// Delete was pressed over the list with rows selected.
    pub delete: bool,
}

/// Draw `items` as a scrolling list filling `ui`, each row's contents drawn
/// left to right by `draw` on top of the row's own click-and-drag area
/// (widgets it adds, such as buttons, take their clicks first), clear of
/// the row's right end by `EDGE`. The row
/// `playing` is outlined and tinted in the theme's playing color, under
/// any selection or hover.
pub fn show<T>(
    ui: &mut egui::Ui,
    id_salt: impl egui::AsId,
    items: &[T],
    selection: &mut Selection,
    playing: Option<usize>,
    mut draw: impl FnMut(&mut egui::Ui, &Row, &T),
) -> ListOutput {
    let id = egui::Id::new(id_salt);
    let mut output = ListOutput::default();
    let touch = touch(ui.ctx());
    let row_height = if touch { TOUCH_ROW_HEIGHT } else { ui.spacing().interact_size.y };
    let sense = if touch { egui::Sense::click() } else { egui::Sense::click_and_drag() };
    let modifiers = ui.input(|i| i.modifiers);
    let playing_color = crate::theme::playing_color(ui.ctx());

    // The keys, when this is the list in front. Rows are drawn only where
    // they're in view, so bringing the cursor's row into view is done by
    // the offset, from where the list was scrolled to last frame.
    let in_front = ui.ctx().data(|d| d.get_temp::<Option<egui::Id>>(keyboard_id())).flatten() == Some(id);
    let steers = match in_front {
        true => ui.ctx().data_mut(|d| d.remove_temp::<Vec<(Steer, bool)>>(steers_id())).unwrap_or_default(),
        false => Vec::new(),
    };
    let view_id = id.with("view");
    let (offset, height) = ui.ctx().data(|d| d.get_temp::<(f32, f32)>(view_id)).unwrap_or_default();
    let pitch = row_height + ui.spacing().item_spacing.y;
    let page = ((height / pitch) as usize).max(1);
    let mut area = egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]);
    if let Some(row) = apply(selection, &steers, items.len(), page, playing, &mut output) {
        let top = row as f32 * pitch;
        let shown = if top < offset {
            top
        } else if top + row_height > offset + height {
            top + row_height - height
        } else {
            offset
        };
        area = area.vertical_scroll_offset(shown);
    }
    let scroll = area.show_rows(ui, row_height, items.len(), |ui, range| {
            for idx in range {
                let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width(), row_height));
                // Not focusable: a focused row would count as typing and turn the keys off.
                let response = ui.interact(rect, id.with(idx), sense - egui::Sense::FOCUSABLE);
                // Playing and selecting are apart: a plain click plays and
                // leaves the selection be. (A double-click's second click
                // doesn't play, or load, it again.)
                if response.clicked() {
                    if touch || !(modifiers.command || modifiers.shift) {
                        if !response.double_clicked() {
                            output.activated = Some(idx);
                        }
                    } else {
                        selection.click(idx, modifiers);
                    }
                }
                if response.contains_pointer() {
                    output.hovered = Some(idx);
                }

                let selected = selection.contains(idx);
                let marked = playing == Some(idx) && !selected && !response.hovered();
                let visuals = ui.style().interact_selectable(&response, selected);
                if marked {
                    ui.painter().rect(
                        rect,
                        visuals.corner_radius,
                        playing_color.gamma_multiply(0.15),
                        egui::Stroke::new(1.0, playing_color),
                        egui::StrokeKind::Inside,
                    );
                } else if selected || response.hovered() {
                    ui.painter().rect(
                        rect.expand(visuals.expansion),
                        visuals.corner_radius,
                        visuals.weak_bg_fill,
                        visuals.bg_stroke,
                        egui::StrokeKind::Inside,
                    );
                } else if idx % 2 == 1 {
                    // Striped, to tell the rows apart.
                    ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                }

                let row = Row { idx, response, selection };
                let builder = egui::UiBuilder::new()
                    .id_salt(idx)
                    .max_rect(rect.with_max_x(rect.max.x - EDGE))
                    .layout(egui::Layout::left_to_right(egui::Align::Center));
                ui.scope_builder(builder, |ui| {
                    // Selectable text would take the row's clicks and drags.
                    ui.style_mut().interaction.selectable_labels = false;
                    if selected {
                        ui.visuals_mut().override_text_color = Some(visuals.text_color());
                    } else if marked {
                        ui.visuals_mut().override_text_color = Some(playing_color);
                    }
                    if touch {
                        ui.add_space(EDGE);
                    }
                    draw(ui, &row, &items[idx]);
                });
            }
        });

    ui.ctx().data_mut(|d| d.insert_temp(view_id, (scroll.state.offset.y, scroll.inner_rect.height())));

    // Keys, while the list is in front or under the pointer, and nothing's
    // being typed.
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    if !typing && (in_front || ui.rect_contains_pointer(scroll.inner_rect)) {
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

/// An entry's text, filling the width left in the row: while `scroll` is
/// on, text that doesn't fit scrolls sideways (see [`crate::marquee`]),
/// else it's cut short with "...". Hovering it shows `hover`, when given
/// (so that should hold the full text too). Draw a row's right-hand
/// widgets first (in a right-to-left layout), then this.
pub fn label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, hover: Option<String>, scroll: bool) -> egui::Response {
    let response = ui
        .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| crate::marquee::show(ui, text, scroll))
        .inner;
    match hover {
        Some(hover) => response.on_hover_text(hover),
        None => response,
    }
}

/// A row's text on a touch screen, the shape every list there shares:
/// `name` after `tags`' chips, over `detail` in small, weak text (or
/// `name` alone, centered, without one). Draw a row's right-hand widgets
/// first (in a right-to-left layout), then this.
pub fn two_lines<'a>(
    ui: &mut egui::Ui,
    tags: impl DoubleEndedIterator<Item = (&'a str, [u8; 3])>,
    name: impl Into<egui::WidgetText>,
    detail: Option<String>,
    scroll: bool,
) {
    let Some(detail) = detail else {
        chips(ui, tags);
        label(ui, name, None, scroll);
        return;
    };
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add_space(3.0);
        ui.spacing_mut().item_spacing.y = 1.0;
        let line = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(line, egui::Layout::right_to_left(egui::Align::Center), |ui| {
            chips(ui, tags);
            label(ui, name, None, scroll);
        });
        label(ui, egui::RichText::new(detail).small().weak(), None, scroll);
    });
}

/// Whether a row's text scrolls when it's cut short: on the `playing` row
/// and the one under the pointer (or finger), to keep the list calm.
pub fn scrolls(row: &Row, playing: bool) -> bool {
    playing || row.response.contains_pointer()
}

/// The icon starred songs are marked with (`star`): an alien, filled when
/// starred, in outline when not.
pub const STAR_ICON: &str = "alien";
/// How wide a song's star is, at the left of its row.
pub const STAR_WIDTH: f32 = 18.0;

/// A song's star, at the left of its row: its alien filled in the theme's
/// star color when it's starred, a faint outline when it isn't. Whether it
/// was clicked (to star or unstar it).
pub fn star(ui: &mut egui::Ui, starred: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(STAR_WIDTH, ui.available_height()), egui::Sense::click());
    let weak = ui.visuals().weak_text_color();
    let color = match (starred, response.hovered()) {
        (true, _) => crate::theme::star_color(ui.ctx()),
        (false, true) => weak,
        (false, false) => weak.gamma_multiply(0.3),
    };
    let mut font = egui::TextStyle::Body.resolve(ui.style());
    if starred {
        font.family = egui::FontFamily::Name(crate::app::fonts::PHOSPHOR_FILL.into());
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, crate::app::icon(STAR_ICON), font, color);
    response.on_hover_text(if starred { "Starred: click to unstar" } else { "Click to star" }).clicked()
}

/// A tag's chip: its name on its color (`song_store::Tag`), in text that
/// reads on it.
pub fn chip(ui: &mut egui::Ui, name: &str, color: [u8; 3]) -> egui::Response {
    let [r, g, b] = color;
    let light = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b) > 150.0;
    let text = if light { egui::Color32::BLACK } else { egui::Color32::WHITE };
    let font = egui::FontId::proportional(egui::TextStyle::Body.resolve(ui.style()).size * 0.85);
    let galley = ui.painter().layout_no_wrap(name.to_string(), font, text);
    let padding = egui::vec2(6.0, 1.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + 2.0 * padding, egui::Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, egui::Color32::from_rgb(r, g, b));
    ui.painter().galley(rect.min + padding, galley, text);
    response
}

/// Tags' chips, in a row (right to left in a right-to-left layout).
pub fn chips<'a>(ui: &mut egui::Ui, tags: impl DoubleEndedIterator<Item = (&'a str, [u8; 3])>) {
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 4.0;
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

    fn steers(selection: &mut Selection, steers: &[(Steer, bool)], playing: Option<usize>) -> (Vec<usize>, Option<usize>) {
        let mut output = ListOutput::default();
        apply(selection, steers, 10, 4, playing, &mut output);
        (selection.rows().to_vec(), output.activated)
    }

    #[test]
    fn keys_move_the_cursor() {
        let mut selection = Selection::default();
        assert_eq!(steers(&mut selection, &[(Steer::Down, false)], Some(6)), (vec![6], None), "starts on the playing row");
        assert_eq!(steers(&mut selection, &[(Steer::Down, false), (Steer::Down, false)], None), (vec![8], None));
        assert_eq!(steers(&mut selection, &[(Steer::PageDown, false)], None), (vec![9], None), "stops at the end");
        assert_eq!(steers(&mut selection, &[(Steer::PageUp, false), (Steer::Open, false)], None), (vec![5], Some(5)));
        assert_eq!(steers(&mut selection, &[(Steer::Up, true), (Steer::Up, true)], None), (vec![3, 4, 5], None), "Shift grows it");
        assert_eq!(steers(&mut selection, &[(Steer::Down, true)], None), (vec![4, 5], None), "and shrinks it back");
        assert_eq!(steers(&mut selection, &[(Steer::First, false)], None), (vec![0], None));
        clicks(&mut selection, &[(7, PLAIN)]);
        assert_eq!(steers(&mut selection, &[(Steer::Last, true)], None), (vec![7, 8, 9], None), "from the click");
        let mut empty = Selection::default();
        let mut output = ListOutput::default();
        assert_eq!(apply(&mut empty, &[(Steer::Down, false)], 0, 4, None, &mut output), None);
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
