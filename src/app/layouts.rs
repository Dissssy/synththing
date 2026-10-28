//! The Layout menu on the title bar: the built-in arrangements and the
//! ones saved under a name, to switch between, saving the current one,
//! the Edit layouts window (reset a changed built-in, rename or delete a
//! saved one), and the mobile layout.

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

/// What the menu or the window asked for.
enum Ask {
    Switch(String),
    Reset(String),
    Rename(usize, String),
    Delete(usize),
    Lock(String, bool),
}

impl App {
    /// Whether the showing layout is locked.
    pub(super) fn layout_locked(&self) -> bool {
        self.active_layout().is_some_and(|name| self.config.locked_layouts.contains(name))
    }

    /// The Layout menu: the layouts, the showing one marked (a built-in
    /// changed since is marked so), then Save, Edit and the mobile layout.
    pub(super) fn layout_menu_ui(&mut self, ui: &mut egui::Ui) {
        let active = self.active_layout().cloned();
        let mut ask = None;
        for preset in layout::PRESETS {
            let mut text = egui::text::LayoutJob::default();
            let style = ui.style();
            egui::RichText::new(preset.name).append_to(&mut text, style, egui::FontSelection::Default, egui::Align::Center);
            if self.config.layout_overrides.contains_key(preset.name) {
                egui::RichText::new("  changed").weak().append_to(&mut text, style, egui::FontSelection::Default, egui::Align::Center);
            }
            if ui.radio(active.as_deref() == Some(preset.name), text).on_hover_text(preset.description).clicked() {
                ask = Some(Ask::Switch(preset.name.to_string()));
            }
        }
        if !self.config.layout_presets.is_empty() {
            ui.separator();
            for saved in &self.config.layout_presets {
                if ui.radio(active.as_deref() == Some(saved.name.as_str()), &saved.name).clicked() {
                    ask = Some(Ask::Switch(saved.name.clone()));
                }
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
        if ui.button("Save current layout as...").clicked() {
            self.layout_save = Some(String::new());
        }
        let editable = !self.config.layout_presets.is_empty() || !self.config.layout_overrides.is_empty();
        if ui
            .add_enabled(editable, egui::Button::new("Edit layouts..."))
            .on_hover_text("Reset a changed built-in layout, rename or delete a saved one")
            .on_disabled_hover_text("Nothing to edit: save a layout, or change a built-in one")
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

    /// Edit layouts: each changed built-in with Reset, each saved one with
    /// Rename and Delete (which asks first).
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
            for preset in layout::PRESETS.iter().filter(|p| self.config.layout_overrides.contains_key(p.name)) {
                row(ui, &mut |ui| {
                    if ui.button("Reset").on_hover_text("Back to how this layout comes").clicked() {
                        ask = Some(Ask::Reset(preset.name.to_string()));
                    }
                    ui.weak("built-in, changed");
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.label(preset.name));
                });
            }
            for (n, saved) in self.config.layout_presets.iter().enumerate() {
                row(ui, &mut |ui| match (&mut editing.renaming, editing.deleting) {
                    (Some((at, name)), _) if *at == n => {
                        let taken = layout::PRESETS.iter().any(|p| p.name.eq_ignore_ascii_case(name.trim()))
                            || self.config.layout_presets.iter().enumerate().any(|(i, p)| i != n && p.name.eq_ignore_ascii_case(name.trim()));
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
        let active = self.active_layout().cloned();
        match ask {
            Ask::Switch(name) => self.switch_layout(&name),
            Ask::Reset(name) => {
                self.config.layout_overrides.remove(&name);
                if active.as_deref() == Some(name.as_str())
                    && let Some(preset) = layout::PRESETS.iter().find(|p| p.name == name)
                {
                    self.apply_layout((preset.build)());
                } else if let Err(e) = self.config.save() {
                    self.status = format!("Couldn't save the layouts: {e}");
                    return;
                }
                self.status = format!("Reset the {name} layout.");
            }
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
                for slot in [&mut self.config.active_layout, &mut self.config.fullscreen_active_layout] {
                    if slot.as_deref() == Some(old.as_str()) {
                        *slot = Some(name.clone());
                    }
                }
                self.status = match self.config.save() {
                    Ok(()) => format!("Renamed the layout \"{old}\" to \"{name}\"."),
                    Err(e) => format!("Couldn't save the layouts: {e}"),
                };
            }
            Ask::Delete(n) => {
                let removed = self.config.layout_presets.remove(n);
                self.config.locked_layouts.remove(&removed.name);
                for slot in [&mut self.config.active_layout, &mut self.config.fullscreen_active_layout] {
                    if slot.as_deref() == Some(removed.name.as_str()) {
                        *slot = None;
                    }
                }
                self.status = match self.config.save() {
                    Ok(()) => format!("Deleted the layout \"{}\".", removed.name),
                    Err(e) => format!("Couldn't save the layouts: {e}"),
                };
            }
        }
    }

    /// Layout > Save current layout as...: name it.
    pub(super) fn layout_save_ui(&mut self, ctx: &egui::Context) {
        let Some(name) = &mut self.layout_save else { return };
        let mut save = false;
        let mut close = false;
        let taken = self.config.layout_presets.iter().any(|p| p.name.eq_ignore_ascii_case(name.trim()));
        let built_in = layout::PRESETS.iter().any(|p| p.name.eq_ignore_ascii_case(name.trim()));
        let response = egui::Modal::new(egui::Id::new("save_layout")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 320.0));
            ui.heading("Save layout");
            ui.label("Name:");
            let field = ui.text_edit_singleline(name);
            field.request_focus();
            if built_in {
                ui.colored_label(ui.visuals().warn_fg_color, "That's a built-in layout's name; pick another.");
            } else if taken {
                ui.weak("A saved layout has this name; saving replaces it.");
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(!name.trim().is_empty() && !built_in, egui::Button::new("Save")).clicked() {
                    save = true;
                }
            });
        });
        if save && !name.trim().is_empty() && !built_in {
            let name = name.trim().to_string();
            let saved = layout::SavedLayout { name: name.clone(), dock: self.dock.clone() };
            match self.config.layout_presets.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                Some(existing) => *existing = saved,
                None => self.config.layout_presets.push(saved),
            }
            // From now on, changes go into it, but it starts locked.
            self.config.locked_layouts.insert(name.clone());
            self.set_active_layout(Some(name.clone()));
            self.status = match self.config.save() {
                Ok(()) => format!("Saved the layout \"{name}\" (Layout menu)."),
                Err(e) => format!("Couldn't save the layout: {e}"),
            };
            close = true;
        }
        if close || response.should_close() {
            self.layout_save = None;
        }
    }
}
