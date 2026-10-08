//! The running script's Settings, Controls and Debug, each in a window
//! laid out like Preferences (`categories_body`), opened from the buttons
//! above the visualizer. Settings and Controls list the script's groups
//! down the side (`{ group = ... }` on `setting_*` and `input_register`;
//! anything without one under General), one line per item, with an (i)
//! where it gave `info`. Debug has its variables (read a few times a
//! second while shown, only as far as they're opened), its last
//! `debug_locals()`, its log, and how long it takes to draw.

use eframe::egui;

use super::{App, categories_body, debug_var_ui, info_icon, settings_row_ui};
use crate::lua_visualizer::{
    self, Action, Binding, ItemLabels, LogLevel, STATE_PAGE, SettingDescriptor, SettingValue, SettingsPreset, StateVar,
    display_name,
};

/// Which window is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptWindow {
    Settings,
    Controls,
    Debug,
}

impl ScriptWindow {
    /// Its slot in `App::script_window_tab`.
    fn slot(self) -> usize {
        self as usize
    }
}

/// The Debug window's categories.
const DEBUG_TABS: [&str; 4] = ["Performance", "Variables", "debug_locals()", "Log"];
const PERFORMANCE: usize = 0;
const VARIABLES: usize = 1;
const DEBUG_LOCALS: usize = 2;

/// The heading for items with no group.
const GENERAL: &str = "General";

/// The groups `labels` fall into, in order of first appearance; General
/// first, if anything has no group.
fn groups<'a>(labels: impl Iterator<Item = &'a ItemLabels> + Clone) -> Vec<String> {
    let mut groups = Vec::new();
    if labels.clone().any(|l| l.group.is_none()) {
        groups.push(GENERAL.to_string());
    }
    for label in labels {
        if let Some(group) = &label.group
            && !groups.contains(group)
        {
            groups.push(group.clone());
        }
    }
    groups
}

/// Whether an item with `labels` is listed under `group`.
fn in_group(labels: &ItemLabels, group: &str) -> bool {
    labels.group.as_deref().unwrap_or(GENERAL) == group
}

/// An item's name, with its info on hover and an (i) after it if it has
/// any.
fn name_ui(ui: &mut egui::Ui, name: &str, labels: &ItemLabels) {
    ui.horizontal(|ui| {
        let label = ui.label(name);
        if let Some(info) = &labels.info {
            label.on_hover_text(info);
            info_icon(ui).on_hover_text(info);
        }
    });
}

impl App {
    /// The Settings, Controls and Debug buttons above the visualizer;
    /// Settings and Controls greyed out when the script has none.
    pub(super) fn script_window_buttons_ui(&mut self, ui: &mut egui::Ui) {
        let settings = self.script.settings().len();
        let controls = self.script.actions().len();
        let count = |n: usize, what: &str| match n {
            0 => format!("This script has no {what}."),
            1 => format!("1 {}", what.trim_end_matches('s')),
            n => format!("{n} {what}"),
        };
        if ui
            .add_enabled(settings > 0, egui::Button::new("Settings"))
            .on_hover_text(count(settings, "settings"))
            .on_disabled_hover_text(count(settings, "settings"))
            .clicked()
        {
            self.script_window = Some(ScriptWindow::Settings);
        }
        if ui
            .add_enabled(controls > 0, egui::Button::new("Controls"))
            .on_hover_text(count(controls, "controls"))
            .on_disabled_hover_text(count(controls, "controls"))
            .clicked()
        {
            self.script_window = Some(ScriptWindow::Controls);
        }
        let failing = self.script.error().is_some();
        let label = if failing {
            egui::RichText::new("Debug (error)").color(ui.visuals().error_fg_color)
        } else {
            egui::RichText::new("Debug")
        };
        if ui
            .button(label)
            .on_hover_text("The script's variables, its debug_locals(), its log, and how long it takes to draw")
            .clicked()
        {
            self.script_window = Some(ScriptWindow::Debug);
        }
    }

    /// The Settings, Controls or Debug window, if one is open.
    pub(super) fn script_window_ui(&mut self, ctx: &egui::Context) {
        // The script's variables are only read while they're on screen.
        let watching = self.script_window == Some(ScriptWindow::Debug)
            && self.script_window_tab[ScriptWindow::Debug.slot()] == VARIABLES;
        self.script.watch_state(watching.then_some(&self.state_request));
        let Some(window) = self.script_window else { return };
        let script = self.script.path().map(display_name).unwrap_or_else(|| "script".to_string());
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("script_window")).show(ctx, |ui| {
            ui.set_width(600.0);
            match window {
                ScriptWindow::Settings => {
                    ui.heading(format!("{script}: settings"));
                    ui.add_space(6.0);
                    self.script_settings_body_ui(ui);
                }
                ScriptWindow::Controls => {
                    ui.heading(format!("{script}: controls"));
                    ui.weak(
                        "Click a binding to remove it, right-click it to bind something else in its place, or \
                         + to add one; then press the key or controller button, or click with a mouse button. \
                         Keys and the mouse work while the visualizer has focus; controllers while \
                         synththing's window does.",
                    );
                    let pads = &self.pad_frame.connected;
                    ui.weak(if pads.is_empty() {
                        "No controller connected.".to_string()
                    } else {
                        format!("Controllers: {}", pads.join(", "))
                    });
                    ui.add_space(6.0);
                    self.script_controls_body_ui(ui);
                }
                ScriptWindow::Debug => {
                    ui.heading(format!("{script}: debug"));
                    ui.add_space(6.0);
                    self.script_debug_body_ui(ui);
                }
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        // (Escape while choosing a binding cancels that, not the window.)
        let escape_cancels = self.binding_capture.is_some() && response.should_close() && !close;
        if escape_cancels {
            self.binding_capture = None;
        } else if close || response.should_close() {
            self.script_window = None;
            self.binding_capture = None;
        }
    }

    fn script_settings_body_ui(&mut self, ui: &mut egui::Ui) {
        let descriptors = self.script.settings();
        if descriptors.is_empty() {
            ui.weak("This script has no settings.");
            return;
        }
        self.presets_row_ui(ui, &descriptors);
        ui.separator();
        let groups = groups(descriptors.iter().map(|d| &d.labels));
        let mut changed: Option<(String, SettingValue)> = None;
        let mut index = self.script_window_tab[ScriptWindow::Settings.slot()];
        categories_body(ui, "script_settings", &groups, &mut index, |ui, index| {
            let group = &groups[index];
            egui::Grid::new(("script_settings_grid", index)).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                for d in descriptors.iter().filter(|d| in_group(&d.labels, group)) {
                    name_ui(ui, &d.key, &d.labels);
                    settings_row_ui(ui, d, &mut changed);
                    ui.end_row();
                }
            });
        });
        self.script_window_tab[ScriptWindow::Settings.slot()] = index;
        if let Some((key, value)) = changed {
            self.script.set_setting(key, value);
        }
    }

    /// Presets: pick one (Defaults, the script's, or yours) to set every
    /// setting it has; save the settings as one of yours; delete one.
    fn presets_row_ui(&mut self, ui: &mut egui::Ui, descriptors: &[SettingDescriptor]) {
        let path = self.script.path().map(std::path::Path::to_path_buf);
        if self.user_presets.as_ref().map(|(p, _)| p) != path.as_ref() {
            self.user_presets = path.as_ref().map(|p| (p.clone(), lua_visualizer::load_user_presets(p)));
            self.preset_picked = None;
        }
        let script_presets = self.script.presets();
        let yours = self.user_presets.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        let mut apply: Option<Vec<(String, SettingValue)>> = None;
        let mut picked = self.preset_picked.clone();
        let values_of = |preset: &SettingsPreset| -> Vec<(String, SettingValue)> {
            descriptors
                .iter()
                .filter_map(|d| {
                    let value = lua_visualizer::setting_from_json(&d.kind, preset.values.get(&d.key)?)?;
                    Some((d.key.clone(), value))
                })
                .collect()
        };
        let mut save = false;
        let mut delete = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Preset");
            let shown = picked.as_ref().map_or("Choose...", |(_, name)| name.as_str());
            egui::ComboBox::from_id_salt("settings_preset").selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(false, "Defaults").on_hover_text("Every setting back to the script's default").clicked() {
                    apply = Some(descriptors.iter().map(|d| (d.key.clone(), d.default.clone())).collect());
                    picked = Some((false, "Defaults".into()));
                }
                if !script_presets.is_empty() {
                    ui.label(egui::RichText::new("From the script").weak().small());
                }
                for preset in &script_presets {
                    if ui.selectable_label(picked.as_ref() == Some(&(false, preset.name.clone())), &preset.name).clicked() {
                        apply = Some(values_of(preset));
                        picked = Some((false, preset.name.clone()));
                    }
                }
                if !yours.is_empty() {
                    ui.label(egui::RichText::new("Yours").weak().small());
                }
                for preset in &yours {
                    if ui.selectable_label(picked.as_ref() == Some(&(true, preset.name.clone())), &preset.name).clicked() {
                        apply = Some(values_of(preset));
                        picked = Some((true, preset.name.clone()));
                    }
                }
            });
            if picked.as_ref().is_some_and(|(theirs, _)| *theirs)
                && ui.small_button("Delete").on_hover_text("Delete this preset of yours").clicked()
            {
                delete = true;
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text("name").desired_width(120.0));
            if ui
                .add_enabled(path.is_some() && !self.preset_name.trim().is_empty(), egui::Button::new("Save as preset"))
                .on_hover_text("Keep the settings as they are now as a preset of yours, next to the script")
                .clicked()
            {
                save = true;
            }
        });
        if let Some(values) = apply {
            for (key, value) in values {
                self.script.set_setting(key, value);
            }
        }
        self.preset_picked = picked;
        let Some(path) = path else { return };
        let mut changed = None;
        if save {
            let name = self.preset_name.trim().to_string();
            let values = descriptors.iter().map(|d| (d.key.clone(), lua_visualizer::setting_value_to_json(&d.value))).collect();
            let mut presets = yours.clone();
            let preset = SettingsPreset { name: name.clone(), values };
            match presets.iter_mut().find(|p| p.name == name) {
                Some(existing) => *existing = preset,
                None => presets.push(preset),
            }
            self.preset_picked = Some((true, name));
            self.preset_name.clear();
            changed = Some(presets);
        }
        if delete && let Some((_, name)) = self.preset_picked.take() {
            changed = Some(yours.into_iter().filter(|p| p.name != name).collect());
        }
        if let Some(presets) = changed {
            match lua_visualizer::save_user_presets(&path, &presets) {
                Ok(()) => self.user_presets = Some((path, presets)),
                Err(e) => self.status = format!("Couldn't save the presets: {e}"),
            }
        }
    }

    fn script_controls_body_ui(&mut self, ui: &mut egui::Ui) {
        let actions = self.script.actions();
        if actions.is_empty() {
            self.binding_capture = None;
            ui.weak("This script has no controls.");
            return;
        }
        let groups = groups(actions.iter().map(|a| &a.labels));
        let mut index = self.script_window_tab[ScriptWindow::Controls.slot()];
        categories_body(ui, "script_controls", &groups, &mut index, |ui, index| {
            let shown: Vec<usize> = (0..actions.len()).filter(|&i| in_group(&actions[i].labels, &groups[index])).collect();
            self.controls_rows_ui(ui, &actions, &shown);
        });
        if index != self.script_window_tab[ScriptWindow::Controls.slot()] {
            self.binding_capture = None;
        }
        self.script_window_tab[ScriptWindow::Controls.slot()] = index;
    }

    fn script_debug_body_ui(&mut self, ui: &mut egui::Ui) {
        let mut index = self.script_window_tab[ScriptWindow::Debug.slot()];
        let mut clear_log = false;
        let mut retry = false;
        categories_body(ui, "script_debug", &DEBUG_TABS, &mut index, |ui, index| match index {
            VARIABLES => {
                ui.weak(
                    "The script's variables (its top-level locals that its functions use, and globals it \
                     made), a few times a second. Open a table to see inside it.",
                );
                match self.script.state_snapshot() {
                    None => {
                        ui.weak("(reading...)");
                    }
                    Some(state) => {
                        let mut vars: Vec<&StateVar> = state.locals.iter().chain(&state.globals).collect();
                        vars.sort_by(|a, b| a.name.cmp(&b.name));
                        if vars.is_empty() {
                            ui.weak("(none)");
                        }
                        for var in vars {
                            state_var_ui(ui, var, &mut self.state_request.open);
                        }
                    }
                }
            }
            DEBUG_LOCALS => {
                ui.weak("What was in scope where the script last called debug_locals([label]).");
                match self.script.debug_snapshot() {
                    Some(snapshot) => {
                        if let Some(label) = &snapshot.label {
                            ui.weak(format!("from: {label}"));
                        }
                        for (i, var) in snapshot.vars.iter().enumerate() {
                            debug_var_ui(ui, var, i);
                        }
                    }
                    None => {
                        ui.weak("(nothing captured yet)");
                    }
                }
            }
            PERFORMANCE => {
                self.performance_ui(ui, &mut retry);
            }
            _ => {
                if ui.small_button("Clear").clicked() {
                    clear_log = true;
                }
                egui::ScrollArea::vertical()
                    .id_salt("script_debug_log")
                    .auto_shrink([false, false])
                    .max_height(ui.available_height())
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let entries = self.script.log_entries();
                        if entries.is_empty() {
                            ui.weak("(nothing logged yet)");
                        }
                        for entry in &entries {
                            let color = if entry.level == LogLevel::Error {
                                ui.visuals().error_fg_color
                            } else {
                                ui.visuals().text_color()
                            };
                            let text = if entry.count > 1 {
                                format!("{} (x{})", entry.message, entry.count)
                            } else {
                                entry.message.clone()
                            };
                            ui.colored_label(color, text);
                        }
                    });
            }
        });
        self.script_window_tab[ScriptWindow::Debug.slot()] = index;
        if clear_log {
            self.script.clear_log();
        }
        if retry {
            self.script.retry_full_frame_rate();
        }
    }

    /// Debug > Performance: how long `render()` takes, against 60 fps.
    fn performance_ui(&self, ui: &mut egui::Ui, retry: &mut bool) {
        let perf = self.script.perf_summary();
        if perf.frames == 0 {
            ui.weak("(not drawn yet)");
            return;
        }
        let budget = 1000.0 / 60.0;
        let color = if perf.avg_ms > budget {
            ui.visuals().error_fg_color
        } else if perf.avg_ms > budget * 0.6 {
            ui.visuals().warn_fg_color
        } else {
            ui.visuals().text_color()
        };
        ui.colored_label(color, format!("render() {:.1} ms on average, {:.1} ms at worst", perf.avg_ms, perf.max_ms));
        ui.weak(format!(
            "Over the last second. A 60 fps frame allows {budget:.1} ms; a script averaging more than that \
             for 3 seconds runs at 30 fps until its code changes."
        ));
        if perf.half_rate {
            ui.colored_label(ui.visuals().warn_fg_color, "Running at 30 fps.");
            *retry = ui.button("Try 60 fps again").clicked();
        } else {
            ui.label("Running at 60 fps.");
        }
    }

    /// The actions `shown` and their bindings, each a button: click one to
    /// remove it, right-click it to rebind it in place, + to add one (it
    /// waits for its key straight away), Reset for the script's
    /// defaults. Saved per script.
    fn controls_rows_ui(&mut self, ui: &mut egui::Ui, actions: &[Action], shown: &[usize]) {
        let mut change: Option<(usize, Vec<Binding>)> = None;
        let mut start_capture: Option<Option<(usize, Option<usize>)>> = None;
        // Where this list's buttons are: a press on one is about the button,
        // not a mouse binding.
        let mut buttons: Vec<egui::Rect> = Vec::new();
        egui::Grid::new("script_controls_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            for &i in shown {
                let action = &actions[i];
                name_ui(ui, &action.name, &action.labels);
                ui.horizontal_wrapped(|ui| {
                    for (slot, key) in action.bindings.iter().enumerate() {
                        if self.binding_capture == Some((i, Some(slot))) {
                            // Waiting for its replacement: click to cancel.
                            let waiting = ui.selectable_label(true, "press a key or button...");
                            buttons.push(waiting.rect);
                            if waiting.clicked() {
                                start_capture = Some(None);
                            }
                            continue;
                        }
                        let button = ui
                            .button(key.label())
                            .on_hover_text("Click to remove it, right-click to bind something else in its place");
                        buttons.push(button.rect);
                        if button.clicked() {
                            let mut bindings = action.bindings.clone();
                            bindings.remove(slot);
                            change = Some((i, bindings));
                        } else if button.secondary_clicked() {
                            start_capture = Some(Some((i, Some(slot))));
                        }
                    }
                    let adding = self.binding_capture == Some((i, None));
                    if adding {
                        let waiting = ui.selectable_label(true, "press a key or button...");
                        buttons.push(waiting.rect);
                        if waiting.clicked() {
                            start_capture = Some(None);
                        }
                    } else {
                        let add =
                            ui.button("+").on_hover_text("Add a key, controller button or mouse button: press it next");
                        buttons.push(add.rect);
                        if add.clicked() {
                            start_capture = Some(Some((i, None)));
                        }
                    }
                    if action.bindings != action.defaults {
                        let reset = ui.small_button("Reset").on_hover_text("Back to the script's keys");
                        buttons.push(reset.rect);
                        if reset.clicked() {
                            change = Some((i, action.defaults.clone()));
                        }
                    }
                });
                ui.end_row();
            }
        });

        // Waiting for a key: the first press (not a reserved key) binds it.
        let mut captured = false;
        if let Some((i, slot)) = self.binding_capture
            && let Some(action) = actions.get(i)
        {
            let pressed = ui
                .input(|input| {
                    input.events.iter().find_map(|event| match event {
                        egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                        _ => None,
                    })
                })
                .map(Binding::Key)
                .or_else(|| self.pad_frame.pressed.first().copied().map(Binding::Pad))
                .or_else(|| {
                    // A click on one of these buttons is about them
                    // (cancelling, removing, starting another), not a
                    // binding.
                    if start_capture.is_some() || change.is_some() {
                        return None;
                    }
                    ui.input(|input| {
                        let on_button =
                            input.pointer.press_origin().is_some_and(|at| buttons.iter().any(|r| r.contains(at)));
                        let button = crate::visualizer::MOUSE_BUTTONS.iter().position(|&b| input.pointer.button_pressed(b));
                        button.filter(|_| !on_button)
                    })
                    .map(|i| Binding::Mouse(i as u8))
                });
            if let Some(key) = pressed {
                captured = true;
                self.binding_capture = None;
                let reserved = matches!(key, Binding::Key(k) if crate::visualizer::RESERVED_KEYS.contains(&k));
                if !reserved {
                    let mut bindings = action.bindings.clone();
                    match slot {
                        Some(slot) if slot < bindings.len() => bindings[slot] = key,
                        _ => bindings.push(key),
                    }
                    let mut seen = Vec::new();
                    bindings.retain(|k| {
                        let first = !seen.contains(k);
                        seen.push(*k);
                        first
                    });
                    change = Some((i, bindings));
                }
            }
        }
        // (A key that just got bound can also "click" the focused button;
        // don't let that start capturing again.)
        if let Some(capture) = start_capture
            && !captured
        {
            self.binding_capture = capture;
        }
        if let Some((i, bindings)) = change {
            self.script.set_action_bindings(i, bindings);
        }
    }
}

/// One variable, and if it's an open table, its entries under it; clicking
/// opens or closes it (in `open`, read on the next capture), and "+"
/// shows more of a long one.
fn state_var_ui(ui: &mut egui::Ui, var: &StateVar, open: &mut std::collections::HashMap<Vec<String>, usize>) {
    let text = format!("{} = {}", var.name, var.value);
    if !var.expandable {
        ui.label(text);
        return;
    }
    let is_open = open.contains_key(&var.path);
    let header = egui::CollapsingHeader::new(text).id_salt(&var.path).open(Some(is_open)).show(ui, |ui| {
        if var.children.is_empty() {
            ui.weak("...");
        }
        for child in &var.children {
            state_var_ui(ui, child, open);
        }
        if var.more > 0 {
            let label = format!("+ {} more ({} not shown)", var.more.min(STATE_PAGE), var.more);
            if ui.small_button(label).clicked()
                && let Some(limit) = open.get_mut(&var.path)
            {
                *limit += STATE_PAGE;
            }
        }
    });
    if header.header_response.clicked() {
        if is_open {
            // Closing a table closes everything inside it too.
            open.retain(|path, _| !path.starts_with(&var.path));
        } else {
            open.insert(var.path.clone(), STATE_PAGE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(group: Option<&str>) -> ItemLabels {
        ItemLabels { group: group.map(str::to_string), info: None }
    }

    #[test]
    fn groups_keep_their_order_with_general_first() {
        let items = [labels(Some("Look")), labels(None), labels(Some("Play")), labels(Some("Look"))];
        assert_eq!(groups(items.iter()), ["General", "Look", "Play"]);
        let grouped = [labels(Some("Play")), labels(Some("Look"))];
        assert_eq!(groups(grouped.iter()), ["Play", "Look"], "no General when everything has a group");
        assert!(in_group(&labels(None), "General") && in_group(&labels(Some("Play")), "Play"));
        assert!(!in_group(&labels(Some("Play")), "General"));
    }
}
