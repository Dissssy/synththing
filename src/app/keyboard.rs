//! Preferences > Keyboard: every command with its keys, to change. A
//! command's keys sync with the other settings (`Config::keymap`) unless
//! it's ticked "This device only", which keeps them in this device's own
//! keymap (`Config::local_keymap`) instead.

use eframe::egui;

use super::App;
use crate::commands::{self, Command, Keymap};

/// A key being waited for: the command, and which of its keys it replaces
/// (`None` adds one).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Capture {
    command: Command,
    slot: Option<usize>,
}

/// What a row asked for.
enum Edit {
    Capture(Capture),
    /// Its keys are now these.
    Set(Command, Vec<egui::KeyboardShortcut>),
    /// Back to its defaults.
    Reset(Command),
    /// Keep its keys on this device only, or sync them again.
    Local(Command, bool),
}

impl App {
    pub(super) fn keyboard_preferences_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Click a key to change it, or + to add another. Esc stops waiting for a key.");
        ui.add_space(4.0);
        let keymap = Keymap::of(ui.ctx());
        let mut edit = None;

        if let Some(capture) = self.keymap_capture {
            let pressed = ui.input_mut(|i| {
                let pressed = i.events.iter().find_map(|event| match event {
                    egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                });
                // Taken, so it doesn't also close Preferences or press a button.
                if let Some((key, modifiers)) = pressed {
                    i.consume_key(modifiers, key);
                }
                pressed
            });
            match pressed {
                Some((egui::Key::Escape, modifiers)) if modifiers.is_none() => self.keymap_capture = None,
                Some((key, modifiers)) => {
                    let mut keys: Vec<_> = keymap.keys(capture.command).copied().collect();
                    let shortcut = commands::captured(modifiers, key);
                    match capture.slot {
                        Some(slot) if slot < keys.len() => keys[slot] = shortcut,
                        _ => keys.push(shortcut),
                    }
                    let mut seen = Vec::new();
                    keys.retain(|k| {
                        let first = !seen.contains(k);
                        seen.push(*k);
                        first
                    });
                    edit = Some(Edit::Set(capture.command, keys));
                    self.keymap_capture = None;
                }
                None => {}
            }
        }

        egui::Grid::new("keyboard_preferences").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
            for command in Command::ALL {
                ui.label(command.name());
                ui.horizontal_wrapped(|ui| {
                    let keys: Vec<_> = keymap.keys(command).copied().collect();
                    for (slot, shortcut) in keys.iter().enumerate() {
                        let capture = Capture { command, slot: Some(slot) };
                        if self.keymap_capture == Some(capture) {
                            ui.add(egui::Button::new("Press a key...").selected(true));
                            continue;
                        }
                        let others = keymap.others_on(shortcut, command);
                        let mut text = egui::RichText::new(ui.ctx().format_shortcut(shortcut));
                        if !others.is_empty() {
                            text = text.color(ui.visuals().warn_fg_color);
                        }
                        let mut hover = String::from("Click to change it, right-click to remove it");
                        if !others.is_empty() {
                            let names: Vec<&str> = others.iter().map(|c| c.name()).collect();
                            hover = format!("Also {}: whichever comes first in the list wins. {hover}", names.join(", "));
                        }
                        let button = ui.button(text).on_hover_text(hover);
                        if button.clicked() {
                            edit = Some(Edit::Capture(capture));
                        }
                        if button.secondary_clicked() {
                            let rest = keys.iter().enumerate().filter(|&(i, _)| i != slot).map(|(_, k)| *k).collect();
                            edit = Some(Edit::Set(command, rest));
                        }
                    }
                    let adding = Capture { command, slot: None };
                    if self.keymap_capture == Some(adding) {
                        ui.add(egui::Button::new("Press a key...").selected(true));
                    } else if ui.small_button("+").on_hover_text("Add a key").clicked() {
                        edit = Some(Edit::Capture(adding));
                    }
                    if keys != commands::defaults(command) && ui.small_button("Reset").on_hover_text("Back to its default keys").clicked() {
                        edit = Some(Edit::Reset(command));
                    }
                });
                let mut local = self.config.local_keymap.contains_key(command.id());
                if ui
                    .checkbox(&mut local, "This device only")
                    .on_hover_text("Keep this command's keys on this device, not synced with your other devices")
                    .changed()
                {
                    edit = Some(Edit::Local(command, local));
                }
                ui.end_row();
            }
        });

        let Some(edit) = edit else { return };
        if let Edit::Capture(capture) = edit {
            self.keymap_capture = Some(capture);
            return;
        }
        apply(&mut self.config, &keymap, edit);
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save preferences: {e}");
        }
    }
}

/// Make `edit` to the config's keymaps: into this device's own for a
/// command kept there, else into the synced one.
fn apply(config: &mut crate::config::Config, keymap: &Keymap, edit: Edit) {
    let texts = |keys: &[egui::KeyboardShortcut]| keys.iter().map(commands::to_text).collect::<Vec<_>>();
    match edit {
        Edit::Capture(_) => {}
        Edit::Set(command, keys) => {
            let map = if config.local_keymap.contains_key(command.id()) { &mut config.local_keymap } else { &mut config.keymap };
            map.insert(command.id().to_string(), texts(&keys));
        }
        Edit::Reset(command) => {
            if let Some(local) = config.local_keymap.get_mut(command.id()) {
                *local = texts(&commands::defaults(command));
            } else {
                config.keymap.remove(command.id());
            }
        }
        Edit::Local(command, true) => {
            let keys: Vec<_> = keymap.keys(command).copied().collect();
            config.local_keymap.insert(command.id().to_string(), texts(&keys));
        }
        Edit::Local(command, false) => {
            config.local_keymap.remove(command.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn keys(config: &Config, command: Command) -> Vec<String> {
        Keymap::new(&config.keymap, &config.local_keymap).keys(command).map(commands::to_text).collect()
    }

    fn set(config: &mut Config, command: Command, texts: &[&str]) {
        let keymap = Keymap::new(&config.keymap, &config.local_keymap);
        let keys = texts.iter().map(|t| commands::from_text(t).unwrap()).collect();
        apply(config, &keymap, Edit::Set(command, keys));
    }

    fn edit(config: &mut Config, edit: Edit) {
        let keymap = Keymap::new(&config.keymap, &config.local_keymap);
        apply(config, &keymap, edit);
    }

    #[test]
    fn edits_go_to_the_synced_keymap_unless_kept_here() {
        let mut config = Config::default();
        set(&mut config, Command::TogglePause, &["K"]);
        assert_eq!(config.keymap["toggle_pause"], ["K"]);
        assert_eq!(keys(&config, Command::TogglePause), ["K"]);

        edit(&mut config, Edit::Local(Command::TogglePause, true));
        assert_eq!(config.local_keymap["toggle_pause"], ["K"], "kept here as it was");
        set(&mut config, Command::TogglePause, &["J"]);
        assert_eq!(config.local_keymap["toggle_pause"], ["J"]);
        assert_eq!(config.keymap["toggle_pause"], ["K"], "the synced keys stay for the other devices");

        edit(&mut config, Edit::Reset(Command::TogglePause));
        assert_eq!(keys(&config, Command::TogglePause), ["Space"], "reset here, still kept here");
        assert!(config.local_keymap.contains_key("toggle_pause"));

        edit(&mut config, Edit::Local(Command::TogglePause, false));
        assert_eq!(keys(&config, Command::TogglePause), ["K"], "synced again: the synced keys");
        edit(&mut config, Edit::Reset(Command::TogglePause));
        assert!(config.keymap.is_empty(), "a reset synced command leaves the keymap");

        set(&mut config, Command::Quit, &[]);
        assert!(keys(&config, Command::Quit).is_empty(), "every key removed: unbound");
    }
}
