//! What the app can be told to do from outside its widgets, and the keys
//! that do it.
//!
//! A [`Command`] says nothing about where it came from: the keymap here
//! is one source and the command palette (`app/palette.rs`) another;
//! `App::run_command` carries any of them out. Media keys or system-wide
//! hotkeys would be more sources of the same commands.

use std::collections::BTreeMap;
use std::sync::Arc;

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
    CommandPalette,
}

impl Command {
    /// Every command, in the order the palette lists them.
    pub const ALL: [Command; 15] = [
        Self::TogglePause,
        Self::NextTrack,
        Self::PreviousTrack,
        Self::SeekBack,
        Self::SeekForward,
        Self::RestartSong,
        Self::ToggleVisualizer,
        Self::ToggleFullscreen,
        Self::ExitFullscreen,
        Self::DedicatedVisualizer,
        Self::RestartScript,
        Self::ToggleRecording,
        Self::PauseRecording,
        Self::CommandPalette,
        Self::Quit,
    ];

    /// What the keymap in the config calls it.
    pub fn id(self) -> &'static str {
        match self {
            Self::TogglePause => "toggle_pause",
            Self::SeekBack => "seek_back",
            Self::SeekForward => "seek_forward",
            Self::RestartSong => "restart_song",
            Self::NextTrack => "next_track",
            Self::PreviousTrack => "previous_track",
            Self::ToggleVisualizer => "toggle_visualizer",
            Self::ToggleFullscreen => "toggle_fullscreen",
            Self::ExitFullscreen => "exit_fullscreen",
            Self::DedicatedVisualizer => "dedicated_visualizer",
            Self::RestartScript => "restart_script",
            Self::ToggleRecording => "toggle_recording",
            Self::PauseRecording => "pause_recording",
            Self::Quit => "quit",
            Self::CommandPalette => "command_palette",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::TogglePause => "Play/pause",
            Self::SeekBack => "Seek back 5 seconds",
            Self::SeekForward => "Seek forward 5 seconds",
            Self::RestartSong => "Restart song",
            Self::NextTrack => "Next track",
            Self::PreviousTrack => "Previous track",
            Self::ToggleVisualizer => "Show/hide visualizer",
            Self::ToggleFullscreen => "Fullscreen on/off",
            Self::ExitFullscreen => "Exit fullscreen",
            Self::DedicatedVisualizer => "Fullscreen visualizer",
            Self::RestartScript => "Restart script",
            Self::ToggleRecording => "Start/stop recording",
            Self::PauseRecording => "Pause/resume recording",
            Self::Quit => "Quit",
            Self::CommandPalette => "Command palette",
        }
    }
}

/// The keys each command has until it's given others.
const DEFAULTS: [(Command, KeyboardShortcut); 15] = [
    (Command::CommandPalette, KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::P)),
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

pub fn defaults(command: Command) -> Vec<KeyboardShortcut> {
    DEFAULTS.iter().filter(|(c, _)| *c == command).map(|(_, s)| *s).collect()
}

/// A shortcut as the config keeps it: "Ctrl+Shift+P", "Space", "F5". Ctrl
/// is Cmd on a Mac, so one keymap works on every device.
pub fn to_text(shortcut: &KeyboardShortcut) -> String {
    let m = shortcut.modifiers;
    let mut parts = Vec::new();
    if m.command || m.ctrl || m.mac_cmd {
        parts.push("Ctrl");
    }
    if m.alt {
        parts.push("Alt");
    }
    if m.shift {
        parts.push("Shift");
    }
    parts.push(shortcut.logical_key.name());
    parts.join("+")
}

/// `to_text`'s shortcut back; `None` for one this build can't read.
pub fn from_text(text: &str) -> Option<KeyboardShortcut> {
    let mut parts: Vec<&str> = text.split('+').collect();
    let key = Key::from_name(parts.pop()?)?;
    let mut modifiers = Modifiers::NONE;
    for part in parts {
        modifiers = modifiers.plus(match part {
            "Ctrl" => Modifiers::COMMAND,
            "Alt" => Modifiers::ALT,
            "Shift" => Modifiers::SHIFT,
            _ => return None,
        });
    }
    Some(KeyboardShortcut::new(modifiers, key))
}

/// The keys pressed with `modifiers` as a shortcut to bind: Ctrl on Windows
/// and Linux, Cmd on a Mac, both as `Modifiers::COMMAND`.
pub fn captured(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    let command = modifiers.command || modifiers.ctrl || modifiers.mac_cmd;
    let modifiers = Modifiers { alt: modifiers.alt, shift: modifiers.shift, ..if command { Modifiers::COMMAND } else { Modifiers::NONE } };
    KeyboardShortcut::new(modifiers, key)
}

/// Every command's keys: the defaults, unless the synced keymap gives a
/// command others, unless this device's own does.
pub type Bindings = BTreeMap<String, Vec<String>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    /// Most specific first: egui ignores extra Shift and Alt when
    /// matching, so Shift+Left after Left would never fire.
    bindings: Vec<(Command, KeyboardShortcut)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(&Bindings::new(), &Bindings::new())
    }
}

impl Keymap {
    pub fn new(synced: &Bindings, local: &Bindings) -> Self {
        let mut bindings: Vec<(Command, KeyboardShortcut)> = Command::ALL
            .into_iter()
            .flat_map(|command| {
                let given = local.get(command.id()).or_else(|| synced.get(command.id()));
                let keys = match given {
                    Some(texts) => texts.iter().filter_map(|t| from_text(t)).collect(),
                    None => defaults(command),
                };
                keys.into_iter().map(move |k| (command, k))
            })
            .collect();
        let held = |m: Modifiers| [m.command || m.ctrl || m.mac_cmd, m.alt, m.shift].into_iter().filter(|&b| b).count();
        bindings.sort_by_key(|(_, s)| std::cmp::Reverse(held(s.modifiers)));
        Self { bindings }
    }

    /// The keys bound to `command`.
    pub fn keys(&self, command: Command) -> impl Iterator<Item = &KeyboardShortcut> {
        self.bindings.iter().filter(move |(c, _)| *c == command).map(|(_, s)| s)
    }

    /// The commands other than `command` that `shortcut` is bound to.
    pub fn others_on(&self, shortcut: &KeyboardShortcut, command: Command) -> Vec<Command> {
        self.bindings.iter().filter(|(c, s)| s == shortcut && *c != command).map(|(c, _)| *c).collect()
    }

    /// The commands whose keys were pressed this frame, taking those key
    /// presses. While `typing`, only shortcuts that work while typing
    /// count. A command `available` says no to leaves its key for whatever
    /// else wants it (Escape outside fullscreen closes popups as usual).
    pub fn pressed(&self, input: &mut egui::InputState, typing: bool, available: impl Fn(Command) -> bool) -> Vec<Command> {
        let mut pressed: Vec<Command> = self
            .bindings
            .iter()
            .filter(|(command, shortcut)| (!typing || works_while_typing(shortcut)) && available(*command))
            .filter(|(_, shortcut)| input.consume_shortcut(shortcut))
            .map(|(command, _)| *command)
            .collect();
        pressed.dedup();
        pressed
    }

    /// Keys the app keeps for itself, never reported to a visualizer
    /// script: Escape (`ESCAPE`), and the unmodified keys bound here that
    /// work while typing (function keys).
    pub fn reserved(&self, key: Key) -> bool {
        key == ESCAPE
            || self.bindings.iter().any(|(_, s)| s.logical_key == key && s.modifiers.is_none() && works_while_typing(s))
    }

    /// The keymap in use, for what draws without the app at hand.
    pub fn of(ctx: &egui::Context) -> Arc<Keymap> {
        ctx.data(|d| d.get_temp(keymap_id())).unwrap_or_default()
    }

    /// Put this keymap in use (`of`).
    pub fn install(self: &Arc<Self>, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(keymap_id(), self.clone()));
    }
}

/// Always the app's, whatever the keymap says: the way out of the
/// fullscreen visualizer, so a script can never be bound to it.
pub const ESCAPE: Key = Key::Escape;

fn keymap_id() -> egui::Id {
    egui::Id::new("keymap")
}

/// Whether a shortcut still works while a text field has the keyboard:
/// with Ctrl, Alt or Cmd held, or on a key that never types anything (the
/// function keys, Escape). The rest would type into the field instead.
pub fn works_while_typing(shortcut: &KeyboardShortcut) -> bool {
    let m = shortcut.modifiers;
    m.ctrl || m.alt || m.command || m.mac_cmd || is_function_key(shortcut.logical_key) || shortcut.logical_key == Key::Escape
}

fn is_function_key(key: Key) -> bool {
    key.name().strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok())
}

/// The command's first key, as the OS writes it ("Ctrl+Shift+P"), if it
/// has one.
pub fn shortcut_text(ctx: &egui::Context, command: Command) -> Option<String> {
    Keymap::of(ctx).keys(command).next().map(|shortcut| ctx.format_shortcut(shortcut))
}

/// " (F5)": the command's key for the end of a hover text, or nothing
/// when it has none.
pub fn hint(ctx: &egui::Context, command: Command) -> String {
    shortcut_text(ctx, command).map(|text| format!(" ({text})")).unwrap_or_default()
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
        let keymap = Keymap::default();
        assert_eq!(keymap.pressed(&mut press(Key::Space, Modifiers::NONE), false, |_| true), [Command::TogglePause]);
        assert_eq!(keymap.pressed(&mut press(Key::Space, Modifiers::NONE), true, |_| true), []);
        assert_eq!(keymap.pressed(&mut press(Key::F5, Modifiers::NONE), true, |_| true), [Command::RestartScript]);
        assert_eq!(keymap.pressed(&mut press(Key::Q, Modifiers::COMMAND), true, |_| true), [Command::Quit]);
        let palette = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(keymap.pressed(&mut press(Key::P, palette), true, |_| true), [Command::CommandPalette]);
    }

    #[test]
    fn unavailable_commands_leave_the_key() {
        let mut input = press(Key::Escape, Modifiers::NONE);
        assert_eq!(Keymap::default().pressed(&mut input, false, |c| c != Command::ExitFullscreen), []);
        assert!(input.key_pressed(Key::Escape));
    }

    #[test]
    fn reserved_keys() {
        let keymap = Keymap::default();
        for key in [Key::Escape, Key::F5, Key::F9, Key::F10, Key::F11] {
            assert!(keymap.reserved(key), "{key:?}");
        }
        for key in [Key::Space, Key::Q, Key::F1] {
            assert!(!keymap.reserved(key), "{key:?}");
        }
    }

    #[test]
    fn this_devices_keys_win_over_synced_ones() {
        let synced = Bindings::from([("toggle_pause".into(), vec!["K".into()]), ("restart_script".into(), vec!["F6".into()])]);
        let local = Bindings::from([("toggle_pause".into(), vec!["Ctrl+Alt+J".into()])]);
        let keymap = Keymap::new(&synced, &local);
        let pause: Vec<String> = keymap.keys(Command::TogglePause).map(to_text).collect();
        assert_eq!(pause, ["Ctrl+Alt+J"]);
        assert_eq!(keymap.keys(Command::RestartScript).map(to_text).collect::<Vec<_>>(), ["F6"]);
        assert!(keymap.reserved(Key::F6) && !keymap.reserved(Key::F5), "reserved keys follow the keymap");
        let no_escape = Keymap::new(&Bindings::from([("exit_fullscreen".into(), vec![])]), &Bindings::new());
        assert!(no_escape.reserved(Key::Escape), "Escape stays the app's");
        assert_eq!(keymap.keys(Command::Quit).map(to_text).collect::<Vec<_>>(), ["Ctrl+Q"]);
        let unbound = Keymap::new(&Bindings::from([("quit".into(), vec![])]), &Bindings::new());
        assert_eq!(unbound.keys(Command::Quit).count(), 0);
    }

    #[test]
    fn shortcuts_read_back() {
        for &key in Key::ALL {
            for modifiers in [Modifiers::NONE, Modifiers::COMMAND, Modifiers::COMMAND | Modifiers::SHIFT | Modifiers::ALT] {
                let shortcut = KeyboardShortcut::new(modifiers, key);
                assert_eq!(from_text(&to_text(&shortcut)), Some(shortcut), "{}", to_text(&shortcut));
            }
        }
        assert_eq!(from_text("Hyper+K"), None);
    }
}
