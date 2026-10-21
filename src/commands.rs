//! What the app can be told to do from outside its widgets, and the keys
//! that do it.
//!
//! A [`Command`] says nothing about where it came from: the keymap here
//! is one source, `App::run_command` carries any of them out. Media keys,
//! system-wide hotkeys or a command palette would be more sources of the
//! same commands.

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    TogglePause,
    SeekBack,
    SeekForward,
    RestartSong,
    NextTrack,
    PreviousTrack,
    ToggleVisualizer,
    ToggleFullscreen,
    /// Back to windowed, from either fullscreen.
    ExitFullscreen,
    DedicatedVisualizer,
    RestartScript,
    ToggleRecording,
    PauseRecording,
    Quit,
}

/// Every binding, most specific first: egui ignores extra Shift and Alt
/// when matching, so Shift+Left listed after Left would never fire.
const BINDINGS: [(Command, KeyboardShortcut); 14] = [
    (Command::Quit, KeyboardShortcut::new(Modifiers::COMMAND, Key::Q)),
    (Command::TogglePause, key(Key::Space)),
    (Command::SeekBack, key(Key::ArrowLeft)),
    (Command::SeekForward, key(Key::ArrowRight)),
    (Command::RestartSong, key(Key::R)),
    (Command::NextTrack, key(Key::N)),
    (Command::PreviousTrack, key(Key::P)),
    (Command::ToggleVisualizer, key(Key::V)),
    (Command::ToggleFullscreen, key(Key::F)),
    (Command::ExitFullscreen, key(Key::Escape)),
    (Command::DedicatedVisualizer, key(Key::F11)),
    (Command::RestartScript, key(Key::F5)),
    (Command::ToggleRecording, key(Key::F9)),
    (Command::PauseRecording, key(Key::F10)),
];

const fn key(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::NONE, key)
}

/// Whether a shortcut still works while a text field has the keyboard:
/// with Ctrl, Alt or Cmd held, or on a key that never types anything (the
/// function keys, Escape). The rest would type into the field instead.
fn works_while_typing(shortcut: &KeyboardShortcut) -> bool {
    let m = shortcut.modifiers;
    m.ctrl || m.alt || m.command || m.mac_cmd || is_function_key(shortcut.logical_key) || shortcut.logical_key == Key::Escape
}

fn is_function_key(key: Key) -> bool {
    key.name().strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok())
}

/// The commands whose keys were pressed this frame, taking those key
/// presses. While `typing`, only shortcuts that work while typing count.
/// A command `available` says no to leaves its key for whatever else
/// wants it (Escape outside fullscreen closes popups as usual).
pub fn pressed(input: &mut egui::InputState, typing: bool, available: impl Fn(Command) -> bool) -> Vec<Command> {
    BINDINGS
        .iter()
        .filter(|(command, shortcut)| (!typing || works_while_typing(shortcut)) && available(*command))
        .filter(|(_, shortcut)| input.consume_shortcut(shortcut))
        .map(|(command, _)| *command)
        .collect()
}

/// Keys the app always keeps for itself, never reported to a visualizer
/// script: the unmodified ones that work while typing (Escape, the
/// function keys bound here).
pub fn reserved(key: Key) -> bool {
    BINDINGS
        .iter()
        .any(|(_, s)| s.logical_key == key && s.modifiers.is_none() && works_while_typing(s))
}

/// " (F5)": the command's key for the end of a hover text, or nothing
/// when it has none.
pub fn hint(ctx: &egui::Context, command: Command) -> String {
    BINDINGS
        .iter()
        .find(|(c, _)| *c == command)
        .map(|(_, shortcut)| format!(" ({})", ctx.format_shortcut(shortcut)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, modifiers: Modifiers) -> egui::InputState {
        let raw = egui::RawInput {
            events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
            ..Default::default()
        };
        egui::InputState::default().begin_pass(raw, false, 1.0, egui::InputOptions::default())
    }

    #[test]
    fn single_keys_wait_while_typing() {
        assert_eq!(pressed(&mut press(Key::Space, Modifiers::NONE), false, |_| true), [Command::TogglePause]);
        assert_eq!(pressed(&mut press(Key::Space, Modifiers::NONE), true, |_| true), []);
        assert_eq!(pressed(&mut press(Key::F5, Modifiers::NONE), true, |_| true), [Command::RestartScript]);
        assert_eq!(pressed(&mut press(Key::Q, Modifiers::COMMAND), true, |_| true), [Command::Quit]);
    }

    #[test]
    fn unavailable_commands_leave_the_key() {
        let mut input = press(Key::Escape, Modifiers::NONE);
        assert_eq!(pressed(&mut input, false, |c| c != Command::ExitFullscreen), []);
        assert!(input.key_pressed(Key::Escape));
    }

    #[test]
    fn reserved_keys() {
        for key in [Key::Escape, Key::F5, Key::F9, Key::F10, Key::F11] {
            assert!(reserved(key), "{key:?}");
        }
        for key in [Key::Space, Key::Q, Key::F1] {
            assert!(!reserved(key), "{key:?}");
        }
    }
}
