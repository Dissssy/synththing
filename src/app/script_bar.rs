//! The script bar and the script's own settings: loading, renaming and
//! rescanning scripts, channels, debug variables.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;

use crate::commands::{self, Command};
use crate::audio::AudioCommand;
use crate::layout::Section;
use crate::lua_visualizer::{
    self, DebugVar, SettingDescriptor, SettingKind, SettingValue,
};
use crate::visualizer::NotesSnapshot;

use super::{App, colored_icon_button, editor, fit_width, icon_button, on_color, previews, toast, toggle_chip, tour};

impl App {
    pub(super) fn load_script(&mut self, idx: usize) {
        let Some(path) = self.available_scripts.get(idx).cloned() else {
            return;
        };
        // Edits still waiting go to the script they were made in.
        self.flush_editor();
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                self.editor_text = source.clone();
                self.completion.request(source.clone());
                self.editor_hover_info = None;
                self.script.load(Some(path.clone()), source);
                // (What was shared was for the script before.)
                crate::shared_data::clear();
                self.active_script = Some(idx);
                self.editor_opened(&path);
            }
            Err(e) => self.status = format!("Failed to read script: {e}"),
        }
    }

    pub(super) fn new_script_from_template(&mut self, content: &str) {
        match lua_visualizer::new_script_from_template(&self.scripts_dir, content) {
            Ok(path) => {
                self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
                if let Some(idx) = self.available_scripts.iter().position(|p| *p == path) {
                    self.load_script(idx);
                    self.set_open(Section::Editor, true);
                }
            }
            Err(e) => self.status = format!("Failed to create script: {e}"),
        }
    }

    /// The Rename script modal: a new name for the running script's file.
    pub(super) fn rename_ui(&mut self, ctx: &egui::Context) {
        let Some(rename) = &mut self.rename else {
            return;
        };
        let answer = rename.waiting.as_ref().and_then(|waiting| waiting.try_recv().ok());
        let waiting = rename.waiting.is_some() && answer.is_none();
        let mut submit = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("rename_script")).show(ctx, |ui| {
            ui.set_width(fit_width(ui, 380.0));
            ui.heading("Rename script");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut rename.name).desired_width(260.0));
                ui.label(".lua");
                if !rename.focused_once {
                    edit.request_focus();
                    rename.focused_once = true;
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
            });
            if let Some(error) = &rename.error {
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
            }
            if waiting {
                ui.weak("Renaming, once the script finishes what it's doing...");
            } else {
                ui.weak("Its settings file is renamed along with it.");
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.add_enabled(!waiting, egui::Button::new("Rename")).clicked() {
                    submit = true;
                }
                if ui.add_enabled(!waiting, egui::Button::new("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if let Some(answer) = answer {
            rename.waiting = None;
            let path = rename.path.clone();
            match answer {
                Ok(new_path) => {
                    editor::rename_history(&path, &new_path);
                    self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
                    self.active_script = self.available_scripts.iter().position(|p| *p == new_path);
                    self.script.renamed(new_path.clone());
                    self.status = format!("Renamed script to {}", lua_visualizer::display_name(&new_path));
                    self.rename = None;
                }
                Err(e) => rename.error = Some(e),
            }
            return;
        }
        if waiting {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        if cancel || response.should_close() {
            self.rename = None;
            return;
        }
        if !submit {
            return;
        }
        let name = rename.name.clone();
        // Edits still waiting are saved under the old name first; the
        // script thread saves the script's data, then moves it all along.
        self.flush_editor();
        let answer = self.script.rename(name);
        if let Some(rename) = &mut self.rename {
            rename.waiting = Some(answer);
        }
    }

    /// The running script's error, if any, with a "Go to line" button when
    /// it points at a line. `open_editor`: the button also opens the editor
    /// (for the copy shown above the visualizer).
    pub(super) fn script_error_ui(&mut self, ui: &mut egui::Ui, open_editor: bool) {
        let Some(error) = self.script.error() else {
            return;
        };
        let line = lua_visualizer::error_line(&error);
        ui.horizontal_wrapped(|ui| {
            if let Some(line) = line
                && ui.small_button(format!("Go to line {line}")).clicked()
            {
                self.editor_goto_line = Some(line);
                if open_editor {
                    self.set_open(Section::Editor, true);
                }
            }
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), error);
        });
    }

    /// The scripts folder changed: refresh the picker, and if the running
    /// script's own file changed (edited in another editor, say), load the
    /// new version into the editor and visualizer. The app's own saves
    /// leave the file matching the editor, so they don't reload anything.
    pub(super) fn rescan_scripts(&mut self) {
        let current = self.script.path().map(Path::to_path_buf);
        self.available_scripts = lua_visualizer::list_scripts(&self.scripts_dir);
        self.active_script = current.as_ref().and_then(|p| self.available_scripts.iter().position(|q| q == p));
        let Some(path) = current else {
            return;
        };
        let name = lua_visualizer::display_name(&path);
        // Compared with what was last saved, not the editor: the editor
        // can hold edits that aren't applied (saved) yet.
        match std::fs::read_to_string(&path) {
            Ok(text) if text != self.editor_saved && self.editor_dirty() => {
                self.editor_saved = text;
                self.status = format!(
                    "{name} changed on disk; kept your edits that aren't applied yet (applying them overwrites the file)."
                );
            }
            Ok(text) if text != self.editor_saved => {
                self.editor_text = text.clone();
                self.editor_saved = text.clone();
                self.completion.request(text.clone());
                self.script.set_source(text, None);
                self.status = format!("Reloaded {name} (changed on disk).");
            }
            Ok(_) => {}
            Err(_) if !path.exists() => {
                self.status = format!("{name}.lua was moved or deleted; it keeps running until another script is picked.");
            }
            Err(e) => log::warn!("couldn't re-read {}: {e}", path.display()),
        }
    }

    /// The bar above the visualizer, or above the editor when the
    /// visualizer is off: the script picker (which also holds what's done
    /// to a script's file), Restart, and the script's Settings, Controls and
    /// Debug; with `visualizer`, Fullscreen and recording too.
    pub(super) fn script_bar_ui(&mut self, ui: &mut egui::Ui, visualizer: bool) {
        let mut picked = None;
        // Wrapping onto a second row in a narrow tab, instead of cut off.
        ui.horizontal_wrapped(|ui| {
            let current = self
                .active_script
                .and_then(|i| self.available_scripts.get(i))
                .map(|p| lua_visualizer::display_name(p))
                .unwrap_or_else(|| "(none)".to_string());
            let picker = egui::ComboBox::from_id_salt("visualizer_script_picker")
                .selected_text(current)
                .height(f32::INFINITY)
                // Its sidebar has buttons (two-click ones among them).
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    picked = self.script_list_ui(ui, true);
                    if picked.is_some() || self.any_modal_open() {
                        ui.close();
                    }
                })
                .response
                .on_hover_text("The script drawing the visualizer; open it for more scripts, a new one, and this one's file");
            if ui
                .add(icon_button("arrow-clockwise"))
                .on_hover_text(format!(
                    "Restart: start the script over from scratch; its state resets, its settings stay{}",
                    commands::hint(ui.ctx(), Command::RestartScript)
                ))
                .clicked()
            {
                self.restart_script();
            }
            #[cfg(not(target_os = "android"))]
            if self.script.options().assets
                && let Some(path) = self.active_script.and_then(|i| self.available_scripts.get(i))
            {
                let folder = lua_visualizer::assets_path(path);
                if ui
                    .add(icon_button("folder-open"))
                    .on_hover_text(format!("Open the script's assets folder ({}), for the files it reads", folder.display()))
                    .clicked()
                {
                    super::file_manager::open_folder(&folder);
                }
            }
            if visualizer {
                self.tour.mark(tour::Target::ScriptPicker, picker.rect);
                let fullscreen = ui.add(icon_button("corners-out")).on_hover_text(
                    "Fullscreen visualizer: just the visualizer, filling the screen, with mouse and keyboard going to the script. F11 toggles it, Esc exits.",
                );
                self.tour.mark(tour::Target::Fullscreen, fullscreen.rect);
                if fullscreen.clicked() {
                    let ctx = ui.ctx().clone();
                    self.enter_dedicated(&ctx);
                }
                let see_through = self.config.visualizer_see_through;
                if ui
                    .add(icon_button("checkerboard").selected(see_through))
                    .on_hover_text(
                        "See-through: show what's behind a visualizer with see-through parts, instead of black. \
                         The local API's PNG stream keeps them see-through either way.",
                    )
                    .clicked()
                {
                    self.config.visualizer_see_through = !see_through;
                    toast::show(ui.ctx(), if see_through { "See-through: off" } else { "See-through: on" });
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save the preferences: {e}");
                    }
                }
            }
            ui.separator();
            self.script_window_buttons_ui(ui, |name, color| match color {
                Some(color) => colored_icon_button(name, color),
                None => icon_button(name),
            });
            if visualizer {
                self.recording_buttons_ui(ui);
            }
        });
        match picked {
            Some(previews::Picked::Script(idx)) => self.load_script(idx),
            Some(previews::Picked::New(content)) => self.new_script_from_template(&content),
            None => {}
        }
    }

    /// In the picker's sidebar, for the running script (at `path`): what
    /// can be done to its file.
    pub(super) fn script_file_buttons_ui(&mut self, ui: &mut egui::Ui, path: &Path) {
        // A re-sync when it's a bundled default (they're only written on
        // first run, so an older copy on disk won't have newer host
        // features).
        if let Some(bundled) = path.file_name().and_then(|n| n.to_str()).and_then(lua_visualizer::bundled_default)
            && !crate::library::installed_path(path).exists()
            && ui.button("Restore default").on_hover_text("Back to the script as synththing ships it").clicked()
        {
            self.editor_opened(path);
            self.editor_text = bundled.to_string();
            self.completion.request(self.editor_text.clone());
            self.apply_editor();
        }
        if ui.button("Rename...").on_hover_text("Rename this script's file (its settings come along)").clicked() {
            let name = lua_visualizer::display_name(path);
            self.rename = Some(RenameScript { path: path.to_path_buf(), name, error: None, focused_once: false, waiting: None });
        }
        #[cfg(not(target_os = "android"))]
        super::file_manager::show_in_folder_button(ui, path);
        self.script_share_button(ui);
    }

    /// Per-channel mute toggles for the current MIDI file (left-click
    /// toggle, right-click solo, "All" to re-enable everything). `notes` is
    /// updated optimistically so this frame's preview already reflects it.
    /// Hovering one says what it plays at `position` and its track's name.
    pub(super) fn channels_ui(&mut self, ui: &mut egui::Ui, notes: &mut NotesSnapshot, position: f64) {
        if !notes.detected_channels.is_empty() {
            let mut toggle: Option<(u8, bool)> = None;
            let mut solo: Option<u8> = None;
            let mut enable_all = false;

            // In the colors the script draws them in, if it said; else
            // the keyboard visualizer's.
            let colors = self.script.channel_colors();
            ui.horizontal_wrapped(|ui| {
                ui.label("Channels:")
                    .on_hover_text("Left-click to toggle, right-click to solo");
                if ui.small_button("All").clicked() {
                    enable_all = true;
                }
                for &channel in &notes.detected_channels {
                    let mut enabled = notes.enabled_channels[channel as usize];
                    let [r, g, b] = colors[channel as usize].unwrap_or_else(|| lua_visualizer::default_channel_color(channel));
                    let color = egui::Color32::from_rgb(r, g, b);
                    let song = self.song_notes.as_deref().filter(|n| n.facts().tracks > 0);
                    let instrument =
                        song.map(|song| crate::midi_notes::program_name(channel, song.program_at(channel, position)));
                    let about = |ui: &mut egui::Ui| {
                        match song {
                            Some(song) => {
                                ui.strong(format!("Channel {}: {}", channel + 1, instrument.unwrap_or_default()));
                                if let Some(name) = song.channel_name(channel) {
                                    ui.label(format!("Track: {name}"));
                                }
                            }
                            None => {
                                ui.strong(format!("Channel {}", channel + 1));
                            }
                        }
                        ui.weak("Left-click to toggle, right-click to solo");
                    };
                    let response = if self.config.channel_chips {
                        let text = match instrument {
                            Some(instrument) => format!("{} \u{b7} {instrument}", channel + 1),
                            None => format!("Channel {}", channel + 1),
                        };
                        let response = toggle_chip(ui, enabled, &text, color).on_hover_ui(about);
                        if response.clicked() {
                            toggle = Some((channel, !enabled));
                        }
                        response
                    } else {
                        let response =
                            channel_toggle(ui, &mut enabled, &format!("{}", channel + 1), color).on_hover_ui(about);
                        if response.changed() {
                            toggle = Some((channel, enabled));
                        }
                        response
                    };
                    if response.secondary_clicked() {
                        solo = Some(channel);
                    }
                }
            });

            if enable_all {
                for &c in &notes.detected_channels {
                    notes.enabled_channels[c as usize] = true;
                }
                self.send(AudioCommand::EnableAllChannels);
            } else if let Some(channel) = solo {
                for (i, slot) in notes.enabled_channels.iter_mut().enumerate() {
                    *slot = i == channel as usize;
                }
                self.send(AudioCommand::SoloChannel(channel));
            } else if let Some((channel, enabled)) = toggle {
                notes.enabled_channels[channel as usize] = enabled;
                self.send(AudioCommand::SetChannelEnabled(channel, enabled));
            }
        }
    }
}

/// The Rename script modal's state.
pub(super) struct RenameScript {
    path: PathBuf,
    name: String,
    error: Option<String>,
    /// Focus the text box on the first frame only.
    focused_once: bool,
    /// Sent to the script thread (which saves the script's data first):
    /// the answer, once it comes.
    waiting: Option<std::sync::mpsc::Receiver<Result<PathBuf, String>>>,
}

/// One row of a `debug_locals()` snapshot: a leaf shows `name = value`, a
/// table gets a `CollapsingHeader` nesting its entries the same way.
/// `index` only matters for giving siblings with identical name+value text
/// distinct widget ids (position within the parent's own children list, not
/// a global counter).
pub(super) fn debug_var_ui(ui: &mut egui::Ui, var: &DebugVar, index: usize) {
    if var.children.is_empty() {
        ui.label(format!("{} = {}", var.name, var.value));
        return;
    }
    egui::CollapsingHeader::new(format!("{} = {}", var.name, var.value))
        .id_salt((index, &var.name))
        .show(ui, |ui| {
            for (i, child) in var.children.iter().enumerate() {
                debug_var_ui(ui, child, i);
            }
        });
}

/// One setting's widget, matching `d.kind` to the right egui control.
pub(super) fn settings_row_ui(ui: &mut egui::Ui, d: &SettingDescriptor, changed: &mut Option<(String, SettingValue)>) {
    match (&d.kind, &d.value) {
        (SettingKind::Bool, SettingValue::Bool(v)) => {
            let mut v = *v;
            if ui.checkbox(&mut v, "").changed() {
                *changed = Some((d.key.clone(), SettingValue::Bool(v)));
            }
        }
        (SettingKind::Int { min, max }, SettingValue::Int(v)) => {
            let mut v = *v;
            if ui.add(egui::Slider::new(&mut v, *min..=*max)).changed() {
                *changed = Some((d.key.clone(), SettingValue::Int(v)));
            }
        }
        (SettingKind::Float { min, max }, SettingValue::Float(v)) => {
            let mut v = *v;
            if ui.add(egui::Slider::new(&mut v, *min..=*max)).changed() {
                *changed = Some((d.key.clone(), SettingValue::Float(v)));
            }
        }
        (SettingKind::Color { alpha }, SettingValue::Color(rgba)) => {
            let mut rgba = *rgba;
            let edited = if *alpha {
                ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed()
            } else {
                let mut rgb = [rgba[0], rgba[1], rgba[2]];
                let edited = ui.color_edit_button_srgb(&mut rgb).changed();
                rgba = [rgb[0], rgb[1], rgb[2], 255];
                edited
            };
            if edited {
                *changed = Some((d.key.clone(), SettingValue::Color(rgba)));
            }
        }
        (SettingKind::String, SettingValue::String(s)) => {
            let mut s = s.clone();
            if ui.text_edit_singleline(&mut s).changed() {
                *changed = Some((d.key.clone(), SettingValue::String(s)));
            }
        }
        (SettingKind::Selection { options, max_selections }, SettingValue::Selection(selected)) => {
            ui.vertical(|ui| {
                for option in options {
                    let mut checked = selected.contains(option);
                    if ui.checkbox(&mut checked, option).changed() {
                        let mut next = selected.clone();
                        if checked {
                            next.push(option.clone());
                            if next.len() > *max_selections {
                                next.remove(0); // evict the oldest selection
                            }
                        } else {
                            next.retain(|s| s != option);
                        }
                        *changed = Some((d.key.clone(), SettingValue::Selection(next)));
                    }
                }
            });
        }
        // Descriptor/value kind mismatch can't happen, both always come
        // from the same `SettingsStore` entry, but match exhaustively
        // rather than unwrap.
        _ => {
            ui.weak("(unsupported setting type)");
        }
    }
}

/// A channel's toggle in its color: egui's own checkbox (so it's the
/// same size and shape as any other), filled with the color while the
/// channel's on (the check mark black or white, whichever reads better on
/// it), outlined in it while it's off.
pub(super) fn channel_toggle(ui: &mut egui::Ui, on: &mut bool, label: &str, color: egui::Color32) -> egui::Response {
    ui.scope(|ui| {
        let mark = on_color(color);
        let visuals = ui.visuals_mut();
        // (The label keeps its usual color: the check mark's is the widget's.)
        visuals.override_text_color = Some(visuals.widgets.inactive.text_color());
        let fill = if *on { color } else { color.gamma_multiply(0.15) };
        for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
            state.bg_fill = fill;
            state.bg_stroke = egui::Stroke::new(state.bg_stroke.width.max(1.0), color);
            state.fg_stroke.color = mark;
        }
        ui.checkbox(on, label)
    })
    .inner
}
