//! Preferences > Keyboard: every command with its keys, to change. A
//! command's keys sync with the other settings (`Config::keymap`) unless
//! it's ticked "This device only", which keeps them in this device's own
//! keymap (`Config::local_keymap`) instead.

use eframe::egui;

use super::App;
use crate::commands::{self, Command, Keymap, Sequence, MAX_PRESSES};

/// How long after a press its sequence is taken as finished, when it
/// could go on.
const NEXT_PRESS_WAIT: f64 = 1.0;

/// Keys being waited for: the command, which of its sequences they replace
/// (`None` adds one), and the presses so far.
#[derive(Clone, PartialEq)]
pub(super) struct Capture {
    command: Command,
    slot: Option<usize>,
    presses: Sequence,
    /// When the last press came (`InputState::time`).
    last: f64,
}

impl Capture {
    fn new(command: Command, slot: Option<usize>) -> Self {
        Self { command, slot, presses: Vec::new(), last: 0.0 }
    }

    fn is(&self, command: Command, slot: Option<usize>) -> bool {
        self.command == command && self.slot == slot
    }

    /// The button it shows as, in place of the keys it replaces.
    fn ui(&self, ui: &mut egui::Ui) {
        let text = match self.presses.is_empty() {
            true => "Press keys...".to_string(),
            false => format!("{} ...", commands::display(ui.ctx(), &self.presses)),
        };
        ui.add(egui::Button::new(text).selected(true));
    }
}

/// What a row asked for.
enum Edit {
    Capture(Capture),
    /// Its keys are now these.
    Set(Command, Vec<Sequence>),
    /// Back to its defaults.
    Reset(Command),
    /// Keep its keys on this device only, or sync them again.
    Local(Command, bool),
}

impl App {
    pub(super) fn keyboard_preferences_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Click a key to change it, or + to add another. For a sequence (like Ctrl+G then S), press its keys \
             one after another: it's set a moment after the last. Esc stops waiting for keys.",
        );
        ui.add_space(4.0);
        let keymap = Keymap::of(ui.ctx());
        let mut edit = None;

        if let Some(capture) = &mut self.keymap_capture {
            let (presses, now) = ui.input_mut(|i| {
                let presses: Vec<(egui::Key, egui::Modifiers)> = i
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. }
                            if !commands::is_modifier(*key) =>
                        {
                            Some((*key, *modifiers))
                        }
                        _ => None,
                    })
                    .collect();
                // Taken, so they don't also close Preferences, press a
                // button or start a sequence.
                for (key, modifiers) in &presses {
                    i.consume_key(*modifiers, *key);
                }
                (presses, i.time)
            });
            let mut cancelled = false;
            for (key, modifiers) in presses {
                if key == egui::Key::Escape && modifiers.is_none() {
                    cancelled = true;
                } else if capture.presses.len() < MAX_PRESSES {
                    capture.presses.push(commands::captured(modifiers, key));
                    capture.last = now;
                }
            }
            let waited = now - capture.last;
            let done = capture.presses.len() == MAX_PRESSES || (!capture.presses.is_empty() && waited >= NEXT_PRESS_WAIT);
            if cancelled {
                self.keymap_capture = None;
            } else if done {
                let mut keys: Vec<Sequence> = keymap.keys(capture.command).cloned().collect();
                let sequence = std::mem::take(&mut capture.presses);
                match capture.slot {
                    Some(slot) if slot < keys.len() => keys[slot] = sequence,
                    _ => keys.push(sequence),
                }
                let mut seen = Vec::new();
                keys.retain(|k| {
                    let first = !seen.contains(k);
                    seen.push(k.clone());
                    first
                });
                edit = Some(Edit::Set(capture.command, keys));
                self.keymap_capture = None;
            } else if !capture.presses.is_empty() {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(NEXT_PRESS_WAIT - waited));
            }
        }

        egui::Grid::new("keyboard_preferences").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
            for command in Command::ALL {
                ui.label(command.name());
                ui.horizontal(|ui| {
                    // One line: wrapped, the grid's column stays as narrow as
                    // it started, splitting "Space" and pushing + under it.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    let keys: Vec<Sequence> = keymap.keys(command).cloned().collect();
                    for (slot, sequence) in keys.iter().enumerate() {
                        if let Some(capture) = self.keymap_capture.as_ref().filter(|c| c.is(command, Some(slot))) {
                            capture.ui(ui);
                            continue;
                        }
                        let others = keymap.clashes(sequence, command);
                        let mut text = egui::RichText::new(commands::display(ui.ctx(), sequence));
                        if !others.is_empty() {
                            text = text.color(ui.visuals().warn_fg_color);
                        }
                        let mut hover = String::from("Click to change it, right-click to remove it");
                        if !others.is_empty() {
                            let names: Vec<&str> = others.iter().map(|c| c.name()).collect();
                            hover = format!(
                                "Clashes with {}: the same keys, or one starts the other, so only one of them works. {hover}",
                                names.join(", ")
                            );
                        }
                        let button = ui.button(text).on_hover_text(hover);
                        if button.clicked() {
                            edit = Some(Edit::Capture(Capture::new(command, Some(slot))));
                        }
                        if button.secondary_clicked() {
                            let rest = keys.iter().enumerate().filter(|&(i, _)| i != slot).map(|(_, k)| k.clone()).collect();
                            edit = Some(Edit::Set(command, rest));
                        }
                    }
                    if let Some(capture) = self.keymap_capture.as_ref().filter(|c| c.is(command, None)) {
                        capture.ui(ui);
                    } else if ui.small_button("+").on_hover_text("Add keys").clicked() {
                        edit = Some(Edit::Capture(Capture::new(command, None)));
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
    let texts = |keys: &[Sequence]| keys.iter().map(|k| commands::to_text(k)).collect::<Vec<_>>();
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
            let keys: Vec<Sequence> = keymap.keys(command).cloned().collect();
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
        Keymap::new(&config.keymap, &config.local_keymap).keys(command).map(|k| commands::to_text(k)).collect()
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
