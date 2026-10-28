//! The Layout menu on the title bar: the layouts (arrangements under a
//! name) to switch between, locking one, making a new one (from the
//! current arrangement or a template), the Edit layouts window (rename or
//! delete one), and the mobile layout.

use eframe::egui;

use super::{fit_width, layout, App};

/// What the Edit layouts window is in the middle of.
#[derive(Default)]
pub(super) struct Editing {
    /// The saved layout being renamed, and its new name so far.
    renaming: Option<(usize, String)>,
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
    Rename(usize, String),
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

    /// Edit layouts: each layout with Rename and Delete (which asks first).
    pub(super) fn layout_edit_ui(&mut self, ctx: &egui::Context) {
        let Some(editing) = &mut self.layout_edit else { return };
        let mut ask = None;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("edit_layouts")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 360.0));
            ui.heading("Layouts");
            let row = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), add);
            };
            for (n, saved) in self.config.layout_presets.iter().enumerate() {
                row(ui, &mut |ui| match (&mut editing.renaming, editing.deleting) {
                    (Some((at, name)), _) if *at == n => {
                        let taken = self.config.layout_presets.iter().enumerate().any(|(i, p)| i != n && p.name.eq_ignore_ascii_case(name.trim()));
                        let fine = !name.trim().is_empty() && !taken;
                        if ui.button("Cancel").clicked() {
                            editing.renaming = None;
                            return;
                        }
                        let ok = ui.add_enabled(fine, egui::Button::new("Rename")).on_disabled_hover_text("Another layout has this name");
                        let field = ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width()));
                        if fine && (ok.clicked() || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) {
                            ask = Some(Ask::Rename(n, name.trim().to_string()));
                        }
                    }
                    (_, Some(at)) if at == n => {
                        if ui.button("Cancel").clicked() {
                            editing.deleting = None;
                        }
                        let delete = egui::Button::new(egui::RichText::new("Delete").color(ui.visuals().error_fg_color));
                        if ui.add(delete).clicked() {
                            ask = Some(Ask::Delete(n));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.label(format!("Delete \"{}\"?", saved.name)));
                    }
                    _ => {
                        if ui.button("Delete").clicked() {
                            editing.deleting = Some(n);
                            editing.renaming = None;
                        }
                        if ui.button("Rename").clicked() {
                            editing.renaming = Some((n, saved.name.clone()));
                            editing.deleting = None;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.label(&saved.name));
                    }
                });
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            self.layout_edit = None;
        }
        if let Some(ask) = ask {
            if let Some(editing) = &mut self.layout_edit {
                *editing = Editing::default();
            }
            self.layouts_do(ask);
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
            Ask::Rename(n, name) => {
                let old = std::mem::replace(&mut self.config.layout_presets[n].name, name.clone());
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
        let response = egui::Modal::new(egui::Id::new("new_layout")).show(ctx, |ui| {
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
