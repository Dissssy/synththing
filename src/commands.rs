//! What the app can be told to do from outside its widgets, and the keys
//! that do it.
//!
//! A [`Command`] says nothing about where it came from: the keymap here
//! is one source and the command palette (`app/palette.rs`) another;
//! `App::run_command` carries any of them out. Media keys or system-wide
//! hotkeys would be more sources of the same commands.
//!
//! A command's keys are a sequence of up to three presses (`Ctrl+G` then
//! `S`). After the first press of a longer one, the app waits for the
//! next (`Keymap::pressed` keeps where it's got to in `Pending`) and,
//! after a moment, shows what can follow (the which-key popup).

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

use crate::layout::Section;

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
    /// Open the section if it's closed, and bring it to the front.
    GoTo(Section),
    /// The next (or previous) open section, in `Section::ALL`'s order.
    NextSection,
    PreviousSection,
}

impl Command {
    /// Every command, in the order the palette and Preferences list them.
    pub const ALL: [Command; 25] = [
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
        Self::GoTo(Section::Songs),
        Self::GoTo(Section::Groupings),
        Self::GoTo(Section::Soundfonts),
        Self::GoTo(Section::Visualizer),
        Self::GoTo(Section::Editor),
        Self::GoTo(Section::Reference),
        Self::GoTo(Section::Sprites),
        Self::GoTo(Section::Library),
        Self::NextSection,
        Self::PreviousSection,
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
            Self::GoTo(section) => match section {
                Section::Songs => "go_to_songs",
                Section::Groupings => "go_to_groupings",
                Section::Soundfonts => "go_to_soundfonts",
                Section::Visualizer => "go_to_visualizer",
                Section::Editor => "go_to_editor",
                Section::Reference => "go_to_reference",
                Section::Sprites => "go_to_sprites",
                Section::Library => "go_to_library",
                Section::Removed => "go_to_removed",
            },
            Self::NextSection => "next_section",
            Self::PreviousSection => "previous_section",
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
            Self::GoTo(section) => match section {
                Section::Songs => "Go to Songs",
                Section::Groupings => "Go to Groupings",
                Section::Soundfonts => "Go to Soundfonts",
                Section::Visualizer => "Go to Visualizer",
                Section::Editor => "Go to Script Editor",
                Section::Reference => "Go to Scripting Reference",
                Section::Sprites => "Go to Sprite Editor",
                Section::Library => "Go to Online Library",
                Section::Removed => "Go to Removed",
            },
            Self::NextSection => "Next tab",
            Self::PreviousSection => "Previous tab",
        }
    }
}

/// Presses in order: one for most commands, up to `MAX_PRESSES`.
pub type Sequence = Vec<KeyboardShortcut>;

/// The longest sequence a command can have.
pub const MAX_PRESSES: usize = 3;

const COMMAND: Modifiers = Modifiers::COMMAND;
const COMMAND_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
/// The first press of the go-to sequences.
const GO: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::G);

/// The keys each command has until it's given others.
const DEFAULTS: &[(Command, &[KeyboardShortcut])] = &[
    (Command::CommandPalette, &[KeyboardShortcut::new(COMMAND_SHIFT, Key::P)]),
    (Command::Quit, &[KeyboardShortcut::new(COMMAND, Key::Q)]),
    (Command::TogglePause, &[key(Key::Space)]),
    (Command::SeekBack, &[key(Key::ArrowLeft)]),
    (Command::SeekForward, &[key(Key::ArrowRight)]),
    (Command::RestartSong, &[key(Key::R)]),
    (Command::NextTrack, &[key(Key::N)]),
    (Command::PreviousTrack, &[key(Key::P)]),
    (Command::ToggleVisualizer, &[key(Key::V)]),
    (Command::ToggleFullscreen, &[key(Key::F)]),
    (Command::ExitFullscreen, &[key(Key::Escape)]),
    (Command::DedicatedVisualizer, &[key(Key::F11)]),
    (Command::RestartScript, &[key(Key::F5)]),
    (Command::ToggleRecording, &[key(Key::F9)]),
    (Command::PauseRecording, &[key(Key::F10)]),
    (Command::GoTo(Section::Songs), &[GO, key(Key::S)]),
    (Command::GoTo(Section::Groupings), &[GO, key(Key::G)]),
    (Command::GoTo(Section::Soundfonts), &[GO, key(Key::F)]),
    (Command::GoTo(Section::Visualizer), &[GO, key(Key::V)]),
    (Command::GoTo(Section::Editor), &[GO, key(Key::E)]),
    (Command::GoTo(Section::Reference), &[GO, key(Key::R)]),
    (Command::GoTo(Section::Sprites), &[GO, key(Key::P)]),
    (Command::GoTo(Section::Library), &[GO, key(Key::L)]),
    (Command::NextSection, &[KeyboardShortcut::new(COMMAND, Key::Tab)]),
    (Command::PreviousSection, &[KeyboardShortcut::new(COMMAND_SHIFT, Key::Tab)]),
];

const fn key(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::NONE, key)
}

/// A command's default keys: one sequence, or none.
pub fn defaults(command: Command) -> Vec<Sequence> {
    DEFAULTS.iter().filter(|(c, _)| *c == command).map(|(_, keys)| keys.to_vec()).collect()
}

/// A sequence as the config keeps it: its presses with spaces between,
/// each like "Ctrl+Shift+P", "Space" or "F5" ("Ctrl+G S"). Ctrl is Cmd on
/// a Mac, so one keymap works on every device.
pub fn to_text(sequence: &[KeyboardShortcut]) -> String {
    sequence.iter().map(press_text).collect::<Vec<_>>().join(" ")
}

fn press_text(shortcut: &KeyboardShortcut) -> String {
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

/// `to_text`'s sequence back; `None` for one this build can't read.
pub fn from_text(text: &str) -> Option<Sequence> {
    let sequence: Option<Sequence> = text.split_whitespace().map(press_from_text).collect();
    sequence.filter(|s| (1..=MAX_PRESSES).contains(&s.len()))
}

fn press_from_text(text: &str) -> Option<KeyboardShortcut> {
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

/// A sequence the way the OS writes keys ("Ctrl+G S", "⌘G S").
pub fn display(ctx: &egui::Context, sequence: &[KeyboardShortcut]) -> String {
    sequence.iter().map(|s| ctx.format_shortcut(s)).collect::<Vec<_>>().join(" ")
}

/// A key pressed with `modifiers` as a press to bind: Ctrl on Windows and
/// Linux, Cmd on a Mac, both as `Modifiers::COMMAND`.
pub fn captured(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    let command = modifiers.command || modifiers.ctrl || modifiers.mac_cmd;
    let modifiers = Modifiers { alt: modifiers.alt, shift: modifiers.shift, ..if command { COMMAND } else { Modifiers::NONE } };
    KeyboardShortcut::new(modifiers, key)
}

/// Every command's keys by command id: the defaults, unless the synced
/// keymap gives a command others, unless this device's own does.
pub type Bindings = BTreeMap<String, Vec<String>>;

/// What follows the presses so far, in the which-key popup.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    /// The press finishes this command.
    Command(Command),
    /// The press leads on to this many commands.
    More(usize),
}

/// The presses of a sequence made so far, waiting for the next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    pub presses: Sequence,
    /// When the first was pressed (`InputState::time`).
    pub since: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    bindings: Vec<(Command, Sequence)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(&Bindings::new(), &Bindings::new())
    }
}

fn held(m: Modifiers) -> usize {
    [m.command || m.ctrl || m.mac_cmd, m.alt, m.shift].into_iter().filter(|&b| b).count()
}

/// Whether the key event matches `press` the way egui's shortcuts do
/// (extra Shift and Alt are fine).
fn matches(press: &KeyboardShortcut, key: Key, modifiers: Modifiers) -> bool {
    press.logical_key == key && modifiers.matches_logically(press.modifiers)
}

impl Keymap {
    pub fn new(synced: &Bindings, local: &Bindings) -> Self {
        let bindings = Command::ALL
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
        Self { bindings }
    }

    /// The keys bound to `command`.
    pub fn keys(&self, command: Command) -> impl Iterator<Item = &Sequence> {
        self.bindings.iter().filter(move |(c, _)| *c == command).map(|(_, s)| s)
    }

    /// The other commands `sequence` can't live with: theirs is the same,
    /// or one starts the other, so only one of them can be pressed.
    pub fn clashes(&self, sequence: &[KeyboardShortcut], command: Command) -> Vec<Command> {
        self.bindings
            .iter()
            .filter(|(c, s)| *c != command && (s.starts_with(sequence) || sequence.starts_with(s)))
            .map(|(c, _)| *c)
            .collect()
    }

    /// What can follow `presses`: each next press, with the command it
    /// finishes or how many it leads on to.
    pub fn next(&self, presses: &[KeyboardShortcut]) -> Vec<(KeyboardShortcut, Next)> {
        let mut next: Vec<(KeyboardShortcut, Next)> = Vec::new();
        for (command, sequence) in &self.bindings {
            if sequence.len() <= presses.len() || !sequence.starts_with(presses) {
                continue;
            }
            let press = sequence[presses.len()];
            let finishes = sequence.len() == presses.len() + 1;
            match next.iter_mut().find(|(p, _)| *p == press) {
                Some((_, Next::More(n))) => *n += 1,
                Some((_, found @ Next::Command(_))) => *found = Next::More(2),
                None => next.push((press, if finishes { Next::Command(*command) } else { Next::More(1) })),
            }
        }
        next
    }

    /// The commands whose keys were pressed this frame, taking those key
    /// presses. A first press that starts a longer sequence goes in
    /// `pending`, and the next press carries on from there (a press that
    /// doesn't, Escape say, just gives up on it). While `typing`, only
    /// sequences that start with a press that works while typing count, and
    /// a sequence underway takes the text its presses would type. A command
    /// `available` says no to leaves its key for whatever else wants it
    /// (Escape outside fullscreen closes popups as usual).
    pub fn pressed(
        &self,
        input: &mut egui::InputState,
        typing: bool,
        available: impl Fn(Command) -> bool,
        pending: &mut Option<Pending>,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut taken = Vec::new();
        let mut in_sequence = false;
        for (index, event) in input.events.iter().enumerate() {
            let egui::Event::Key { key, pressed: true, repeat, modifiers, .. } = *event else { continue };
            if repeat && pending.is_some() {
                // (A held first press repeating mustn't give up on it.)
                taken.push(index);
                continue;
            }
            let presses = pending.as_ref().map_or(&[][..], |p| &p.presses[..]);
            // Of the sequences this press carries on, the one with the most
            // modifiers held for it (Shift+Left over Left), then the
            // shortest (one that finishes over one that goes on).
            let found = self
                .bindings
                .iter()
                .filter(|(command, sequence)| {
                    sequence.len() > presses.len()
                        && sequence.starts_with(presses)
                        && matches(&sequence[presses.len()], key, modifiers)
                        && (!presses.is_empty() || !typing || works_while_typing(&sequence[0]))
                        && (!repeat || sequence.len() == 1)
                        && available(*command)
                })
                .max_by_key(|(_, s)| (held(s[presses.len()].modifiers), std::cmp::Reverse(s.len())));
            match found {
                Some((command, sequence)) if sequence.len() == presses.len() + 1 => {
                    in_sequence |= !presses.is_empty();
                    *pending = None;
                    if !commands.contains(command) {
                        commands.push(*command);
                    }
                    taken.push(index);
                }
                Some((_, sequence)) => {
                    in_sequence = true;
                    let since = pending.as_ref().map_or(input.time, |p| p.since);
                    *pending = Some(Pending { presses: sequence[..=presses.len()].to_vec(), since });
                    taken.push(index);
                }
                None if pending.is_some() => {
                    in_sequence = true;
                    *pending = None;
                    taken.push(index);
                }
                None => {}
            }
        }
        let mut index = 0;
        input.events.retain(|event| {
            let keep = !taken.contains(&index) && !(in_sequence && matches!(event, egui::Event::Text(_)));
            index += 1;
            keep
        });
        commands
    }

    /// Keys the app keeps for itself, never reported to a visualizer
    /// script: Escape (`ESCAPE`), and the unmodified keys bound here that
    /// work while typing (function keys) on their own.
    pub fn reserved(&self, key: Key) -> bool {
        key == ESCAPE
            || self.bindings.iter().any(|(_, s)| {
                s.len() == 1 && s[0].logical_key == key && s[0].modifiers.is_none() && works_while_typing(&s[0])
            })
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

/// Whether a press still works while a text field has the keyboard: with
/// Ctrl, Alt or Cmd held, or on a key that never types anything (the
/// function keys, Escape). The rest would type into the field instead.
pub fn works_while_typing(shortcut: &KeyboardShortcut) -> bool {
    let m = shortcut.modifiers;
    m.ctrl || m.alt || m.command || m.mac_cmd || is_function_key(shortcut.logical_key) || shortcut.logical_key == Key::Escape
}

fn is_function_key(key: Key) -> bool {
    key.name().strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok())
}

/// The command's first keys, as the OS writes them ("Ctrl+G S"), if it
/// has any.
pub fn shortcut_text(ctx: &egui::Context, command: Command) -> Option<String> {
    Keymap::of(ctx).keys(command).next().map(|sequence| display(ctx, sequence))
}

/// " (F5)": the command's keys for the end of a hover text, or nothing
/// when it has none.
pub fn hint(ctx: &egui::Context, command: Command) -> String {
    shortcut_text(ctx, command).map(|text| format!(" ({text})")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(events: Vec<egui::Event>) -> egui::InputState {
        let raw = egui::RawInput { events, ..Default::default() };
        egui::InputState::default().begin_pass(raw, false, 1.0, egui::InputOptions::default())
    }

    fn key_event(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    fn press(key: Key, modifiers: Modifiers) -> egui::InputState {
        input(vec![key_event(key, modifiers)])
    }

    fn pressed(keymap: &Keymap, input: &mut egui::InputState, typing: bool) -> Vec<Command> {
        keymap.pressed(input, typing, |_| true, &mut None)
    }

    #[test]
    fn single_keys_wait_while_typing() {
        let keymap = Keymap::default();
        assert_eq!(pressed(&keymap, &mut press(Key::Space, Modifiers::NONE), false), [Command::TogglePause]);
        assert_eq!(pressed(&keymap, &mut press(Key::Space, Modifiers::NONE), true), []);
        assert_eq!(pressed(&keymap, &mut press(Key::F5, Modifiers::NONE), true), [Command::RestartScript]);
        assert_eq!(pressed(&keymap, &mut press(Key::Q, Modifiers::COMMAND), true), [Command::Quit]);
        assert_eq!(pressed(&keymap, &mut press(Key::P, COMMAND_SHIFT), true), [Command::CommandPalette]);
        assert_eq!(pressed(&keymap, &mut press(Key::Tab, COMMAND_SHIFT), true), [Command::PreviousSection]);
        assert_eq!(pressed(&keymap, &mut press(Key::Tab, COMMAND), true), [Command::NextSection]);
    }

    #[test]
    fn unavailable_commands_leave_the_key() {
        let mut input = press(Key::Escape, Modifiers::NONE);
        assert_eq!(Keymap::default().pressed(&mut input, false, |c| c != Command::ExitFullscreen, &mut None), []);
        assert!(input.key_pressed(Key::Escape));
    }

    #[test]
    fn sequences_wait_for_their_next_press() {
        let keymap = Keymap::default();
        let mut pending = None;
        let mut first = press(Key::G, COMMAND);
        assert_eq!(keymap.pressed(&mut first, true, |_| true, &mut pending), []);
        assert_eq!(pending.as_ref().map(|p| to_text(&p.presses)).as_deref(), Some("Ctrl+G"));
        assert!(first.events.is_empty(), "the press is taken");

        // While typing, the next press finishes it without typing its letter.
        let mut second = input(vec![key_event(Key::S, Modifiers::NONE), egui::Event::Text("s".into())]);
        assert_eq!(keymap.pressed(&mut second, true, |_| true, &mut pending), [Command::GoTo(Section::Songs)]);
        assert!(pending.is_none());
        assert!(second.events.is_empty());

        // A press that leads nowhere gives up, and does nothing else.
        keymap.pressed(&mut press(Key::G, COMMAND), false, |_| true, &mut pending);
        let mut stray = press(Key::Space, Modifiers::NONE);
        assert_eq!(keymap.pressed(&mut stray, false, |_| true, &mut pending), []);
        assert!(pending.is_none() && stray.events.is_empty());

        // Holding the first press down doesn't give up on it.
        let mut down = press(Key::G, COMMAND);
        keymap.pressed(&mut down, false, |_| true, &mut pending);
        // (egui marks a press of a key that's down as a repeat itself.)
        let raw = egui::RawInput { events: vec![key_event(Key::G, COMMAND)], ..Default::default() };
        let mut held = down.begin_pass(raw, false, 1.0, egui::InputOptions::default());
        keymap.pressed(&mut held, false, |_| true, &mut pending);
        assert!(pending.is_some());
        pending = None;

        // Both presses in one frame work too.
        let mut both = input(vec![key_event(Key::G, COMMAND), key_event(Key::V, Modifiers::NONE)]);
        assert_eq!(keymap.pressed(&mut both, false, |_| true, &mut pending), [Command::GoTo(Section::Visualizer)]);
    }

    #[test]
    fn the_popup_lists_what_follows() {
        let keymap = Keymap::default();
        let next = keymap.next(&[GO]);
        assert_eq!(next.len(), 8);
        assert!(next.contains(&(key(Key::S), Next::Command(Command::GoTo(Section::Songs)))));
        assert!(keymap.next(&[]).contains(&(GO, Next::More(8))));
    }

    #[test]
    fn reserved_keys() {
        let keymap = Keymap::default();
        for key in [Key::Escape, Key::F5, Key::F9, Key::F10, Key::F11] {
            assert!(keymap.reserved(key), "{key:?}");
        }
        for key in [Key::Space, Key::Q, Key::F1, Key::G] {
            assert!(!keymap.reserved(key), "{key:?}");
        }
    }

    #[test]
    fn this_devices_keys_win_over_synced_ones() {
        let synced = Bindings::from([("toggle_pause".into(), vec!["K".into()]), ("restart_script".into(), vec!["F6".into()])]);
        let local = Bindings::from([("toggle_pause".into(), vec!["Ctrl+Alt+J".into()])]);
        let keymap = Keymap::new(&synced, &local);
        let texts = |c| keymap.keys(c).map(|s| to_text(s)).collect::<Vec<_>>();
        assert_eq!(texts(Command::TogglePause), ["Ctrl+Alt+J"]);
        assert_eq!(texts(Command::RestartScript), ["F6"]);
        assert!(keymap.reserved(Key::F6) && !keymap.reserved(Key::F5), "reserved keys follow the keymap");
        let no_escape = Keymap::new(&Bindings::from([("exit_fullscreen".into(), vec![])]), &Bindings::new());
        assert!(no_escape.reserved(Key::Escape), "Escape stays the app's");
        assert_eq!(texts(Command::Quit), ["Ctrl+Q"]);
        let unbound = Keymap::new(&Bindings::from([("quit".into(), vec![])]), &Bindings::new());
        assert_eq!(unbound.keys(Command::Quit).count(), 0);
    }

    #[test]
    fn clashes_include_one_starting_the_other() {
        let synced = Bindings::from([("toggle_pause".into(), vec!["Ctrl+G".into()])]);
        let keymap = Keymap::new(&synced, &Bindings::new());
        let clashes = keymap.clashes(&[GO], Command::TogglePause);
        assert_eq!(clashes.len(), 8, "every go-to starts with Ctrl+G");
        assert_eq!(keymap.clashes(&[GO, key(Key::S)], Command::GoTo(Section::Songs)), [Command::TogglePause]);
    }

    #[test]
    fn sequences_read_back() {
        for &key in Key::ALL {
            for modifiers in [Modifiers::NONE, Modifiers::COMMAND, Modifiers::COMMAND | Modifiers::SHIFT | Modifiers::ALT] {
                let sequence = vec![KeyboardShortcut::new(modifiers, key), self::key(Key::S)];
                assert_eq!(from_text(&to_text(&sequence)), Some(sequence.clone()), "{}", to_text(&sequence));
            }
        }
        assert_eq!(from_text("Hyper+K"), None);
        assert_eq!(from_text(""), None);
        assert_eq!(from_text("A B C D"), None, "at most three presses");
    }
}
