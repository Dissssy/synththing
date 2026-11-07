//! The Layout menu on the title bar: the layouts (arrangements under a
//! name) to switch between, locking one, making a new one (from the
//! current arrangement or a template), the Edit layouts window (order,
//! rename or delete them), and the mobile layout.

use eframe::egui;

use super::reorder::{reorder_list, trash_ui, ListEntry};
use super::{fit_width, layout, row_icon_button, row_icon_button_room, settings_group, App};

/// What the Edit layouts window is in the middle of.
#[derive(Default)]
pub(super) struct Editing {
    /// The saved layout being renamed (by its name), and its new name so
    /// far.
    renaming: Option<(String, String)>,
    /// The rename's field still has to take the keyboard.
    focus: bool,
    /// The saved layout asked to be deleted, waiting to be confirmed.
    deleting: Option<usize>,
}

/// Layout > New layout...: its name so far, and what it starts from (a
/// template, by index, or the current arrangement).
#[derive(Default)]
pub(super) struct NewLayout {
    name: String,
    from: Option<usize>,
}

/// What the menu or the window asked for.
enum Ask {
    Switch(String),
    Rename(String, String),
    Delete(usize),
    Lock(String, bool),
}

impl App {
    /// Whether the showing layout is locked.
    pub(super) fn layout_locked(&self) -> bool {
        self.active_layout().is_some_and(|name| self.config.locked_layouts.contains(name))
    }

    /// The Layout menu: the layouts, the showing one marked, then Lock,
    /// New, Edit and the mobile layout.
    pub(super) fn layout_menu_ui(&mut self, ui: &mut egui::Ui) {
        let active = self.active_layout().cloned();
        let mut ask = None;
        for saved in &self.config.layout_presets {
            if ui.radio(active.as_deref() == Some(saved.name.as_str()), &saved.name).clicked() {
                ask = Some(Ask::Switch(saved.name.clone()));
            }
        }
        if active.is_none() {
            ui.separator();
            ui.weak("Showing your own arrangement");
        }
        ui.separator();
        let mut locked = self.layout_locked();
        if ui
            .add_enabled(active.is_some(), egui::Checkbox::new(&mut locked, "Lock layout"))
            .on_hover_text("Keep this layout's panels from being moved or resized")
            .on_disabled_hover_text("Save your arrangement as a layout to lock it")
            .changed()
            && let Some(name) = &active
        {
            ask = Some(Ask::Lock(name.clone(), locked));
        }
        if ui.button("New layout...").on_hover_text("From this arrangement, or from a template").clicked() {
            self.layout_new = Some(NewLayout::default());
        }
        if ui
            .add_enabled(!self.config.layout_presets.is_empty(), egui::Button::new("Edit layouts..."))
            .on_hover_text("Rename or delete layouts")
            .clicked()
        {
            self.layout_edit = Some(Editing::default());
        }
        if ui.button("Mobile layout").on_hover_text("The Android app's layout, one page at a time, in a phone-sized window").clicked() {
            let ctx = ui.ctx().clone();
            self.set_mobile_layout(&ctx, true);
        }
        if let Some(ask) = ask {
            self.layouts_do(ask);
        }
    }

    /// Edit layouts: a section of the layouts, one per row in the Layout
    /// menu's order, the showing one marked, with New layout and Close in
    /// its header. A row's grip drags it to its place, or onto the header's
    /// trash to delete it (asking first, in a dialog of its own); a pencil by
    /// its name renames it in place.
    pub(super) fn layout_edit_ui(&mut self, ctx: &egui::Context) {
        let Some(editing) = &mut self.layout_edit else { return };
        let active = self.config.active_layout.as_deref();
        let presets = &mut self.config.layout_presets;
        let len = presets.len();
        let order: Vec<String> = presets.iter().map(|p| p.name.clone()).collect();
        let mut ask = None;
        let mut close = false;
        let mut add = false;
        let mut stop_renaming = false;
        // (Renaming stops when a row's dragged.)
        if egui::DragAndDrop::has_any_payload(ctx) {
            editing.renaming = None;
        }
        let response = egui::Modal::new(egui::Id::unique("edit_layouts")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 400.0));
            let mut buttons = |ui: &mut egui::Ui| {
                close = ui.add(row_icon_button(ui, "x")).on_hover_text("Close").clicked();
                add = ui.add(row_icon_button(ui, "plus")).on_hover_text("New layout: from this arrangement, or from a template").clicked();
                trash_ui(ui, "layouts", len);
            };
            let group = settings_group("Layouts").icon("layout").hint("Drag to put them in order, or onto the trash to delete one. Switch between them from the Layout menu.");
            group.buttons(&mut buttons).show(ui, |ui| {
                let entry = |_: usize, _: &layout::SavedLayout| ListEntry { lines: Vec::new(), row: true };
                let row = |ui: &mut egui::Ui, saved: &layout::SavedLayout| match &mut editing.renaming {
                    Some((old, name)) if *old == saved.name => {
                        let taken = order.iter().any(|other| *other != saved.name && other.eq_ignore_ascii_case(name.trim()));
                        let fine = !name.trim().is_empty() && !taken;
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(row_icon_button(ui, "x")).on_hover_text("Cancel").clicked() {
                                stop_renaming = true;
                                return;
                            }
                            let save = ui
                                .add_enabled(fine, row_icon_button(ui, "check"))
                                .on_hover_text("Rename")
                                .on_disabled_hover_text(if taken { "Another layout has this name" } else { "Give it a name" });
                            let field = ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width()));
                            if editing.focus {
                                field.request_focus();
                                editing.focus = false;
                            }
                            if fine && (save.clicked() || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) {
                                ask = Some(Ask::Rename(old.clone(), name.trim().to_string()));
                            }
                        });
                    }
                    _ => {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if active == Some(saved.name.as_str()) {
                                ui.weak("Showing");
                            }
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.scope(|ui| {
                                    ui.set_max_width(ui.available_width() - row_icon_button_room(ui));
                                    ui.add(egui::Label::new(&saved.name).truncate());
                                });
                                if ui.add(row_icon_button(ui, "pencil-simple")).on_hover_text("Rename this layout").clicked() {
                                    editing.renaming = Some((saved.name.clone(), saved.name.clone()));
                                    editing.focus = true;
                                }
                            });
                        });
                    }
                };
                if let Some(n) = reorder_list(ui, "layouts", presets, entry, row) {
                    editing.deleting = Some(n);
                }
            });
        });
        let reordered = presets.iter().map(|p| &p.name).ne(order.iter());
        if reordered && let Err(e) = self.config.save() {
            self.status = format!("Couldn't save the layouts: {e}");
        }
        let Some(editing) = &mut self.layout_edit else { return };
        let presets = &self.config.layout_presets;
        // (A new layout's dialog shows over this one, which lists it once
        // it's made.)
        if add {
            self.layout_new = Some(NewLayout::default());
        }
        // (Esc while renaming stops the renaming, not the window.)
        if stop_renaming || (response.should_close() && editing.renaming.is_some() && editing.deleting.is_none()) {
            editing.renaming = None;
        } else if close || (response.should_close() && editing.deleting.is_none()) {
            self.layout_edit = None;
            return;
        }
        if let Some(n) = editing.deleting {
            let mut cancel = false;
            let dialog = egui::Modal::new(egui::Id::unique("delete_layout")).show(ctx, |ui| {
                ui.set_width(fit_width(ui, 360.0));
                ui.heading(format!("Delete \"{}\"?", presets[n].name));
                ui.label("Its arrangement is gone for good. The panels showing now stay as they are.");
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    cancel = ui.button("Cancel").clicked();
                    if ui.add(egui::Button::new(egui::RichText::new("Delete").color(ui.visuals().error_fg_color))).clicked() {
                        ask = Some(Ask::Delete(n));
                    }
                });
            });
            if cancel || dialog.should_close() {
                editing.deleting = None;
            }
        }
        if let Some(ask) = ask {
            *editing = Editing::default();
            self.layouts_do(ask);
            if self.config.layout_presets.is_empty() {
                self.layout_edit = None;
            }
        }
    }

    fn layouts_do(&mut self, ask: Ask) {
        match ask {
            Ask::Switch(name) => self.switch_layout(&name),
            Ask::Lock(name, locked) => {
                if locked {
                    self.config.locked_layouts.insert(name);
                } else {
                    self.config.locked_layouts.remove(&name);
                }
                if let Err(e) = self.config.save() {
                    self.status = format!("Couldn't save the layouts: {e}");
                }
            }
            Ask::Rename(old, name) => {
                let Some(saved) = self.config.layout_presets.iter_mut().find(|p| p.name == old) else { return };
                saved.name = name.clone();
                if self.config.locked_layouts.remove(&old) {
                    self.config.locked_layouts.insert(name.clone());
                }
                if self.config.active_layout.as_deref() == Some(old.as_str()) {
                    self.config.active_layout = Some(name.clone());
                }
                self.status = match self.config.save() {
                    Ok(()) => format!("Renamed the layout \"{old}\" to \"{name}\"."),
                    Err(e) => format!("Couldn't save the layouts: {e}"),
                };
            }
            Ask::Delete(n) => {
                let removed = self.config.layout_presets.remove(n);
                self.config.locked_layouts.remove(&removed.name);
                if self.config.active_layout.as_deref() == Some(removed.name.as_str()) {
                    self.config.active_layout = None;
                }
                self.status = match self.config.save() {
                    Ok(()) => format!("Deleted the layout \"{}\".", removed.name),
                    Err(e) => format!("Couldn't save the layouts: {e}"),
                };
            }
        }
    }

    /// Layout > New layout...: name it and pick what it starts from. It
    /// shows straight away, locked.
    pub(super) fn layout_new_ui(&mut self, ctx: &egui::Context) {
        let Some(new) = &mut self.layout_new else { return };
        let mut make = false;
        let mut close = false;
        let name = new.name.trim().to_string();
        let taken = self.config.layout_presets.iter().any(|p| p.name.eq_ignore_ascii_case(&name));
        let fine = !name.is_empty() && !taken;
        let response = egui::Modal::new(egui::Id::unique("new_layout")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 320.0));
            ui.heading("New layout");
            ui.label("Name:");
            let field = ui.text_edit_singleline(&mut new.name);
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                make = true;
            }
            if taken {
                ui.colored_label(ui.visuals().warn_fg_color, "A layout has this name; pick another.");
            }
            ui.label("Start from:");
            let from = |from: Option<usize>| from.map_or("This arrangement", |i| layout::TEMPLATES[i].name);
            egui::ComboBox::from_id_salt("new_layout_from").selected_text(from(new.from)).show_ui(ui, |ui| {
                ui.selectable_value(&mut new.from, None, from(None)).on_hover_text("The panels as they are now");
                for (i, template) in layout::TEMPLATES.iter().enumerate() {
                    ui.selectable_value(&mut new.from, Some(i), template.name).on_hover_text(template.description);
                }
            });
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(fine, egui::Button::new("Make")).clicked() {
                    make = true;
                }
            });
        });
        if make && fine {
            let dock = new.from.map_or_else(|| self.dock.clone(), |i| (layout::TEMPLATES[i].build)());
            self.save_layout();
            self.config.layout_presets.push(layout::SavedLayout { name: name.clone(), dock: dock.clone() });
            self.config.locked_layouts.insert(name.clone());
            self.set_active_layout(Some(name.clone()));
            self.apply_layout(dock);
            self.status = format!("Made the layout \"{name}\" (Layout menu).");
            close = true;
        }
        if close || response.should_close() {
            self.layout_new = None;
        }
    }
}
