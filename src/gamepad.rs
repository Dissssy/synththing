//! Game controllers (gilrs), as inputs script actions can be bound to
//! (`input_register`, the Controls list in Script Settings).
//!
//! Every connected pad counts the same: pressing A on any of them presses
//! "pad_a". Buttons, the d-pad, the triggers and each stick pushed one way
//! are all on/off inputs: a stick or trigger counts as pressed past
//! halfway (with a little slack before it lets go, so it doesn't flicker
//! at the edge). Names follow the Xbox layout: A is the bottom face button
//! whatever the pad prints on it (Cross on a PlayStation pad).

use gilrs::{Axis, Button, EventType, Gilrs};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PadInput {
    A,
    B,
    X,
    Y,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Back,
    Start,
    Guide,
    LeftStickClick,
    RightStickClick,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
}

impl PadInput {
    pub const ALL: [PadInput; 25] = [
        PadInput::A,
        PadInput::B,
        PadInput::X,
        PadInput::Y,
        PadInput::LeftBumper,
        PadInput::RightBumper,
        PadInput::LeftTrigger,
        PadInput::RightTrigger,
        PadInput::Back,
        PadInput::Start,
        PadInput::Guide,
        PadInput::LeftStickClick,
        PadInput::RightStickClick,
        PadInput::DpadUp,
        PadInput::DpadDown,
        PadInput::DpadLeft,
        PadInput::DpadRight,
        PadInput::LeftStickUp,
        PadInput::LeftStickDown,
        PadInput::LeftStickLeft,
        PadInput::LeftStickRight,
        PadInput::RightStickUp,
        PadInput::RightStickDown,
        PadInput::RightStickLeft,
        PadInput::RightStickRight,
    ];

    /// The name scripts use: `"pad_a"`, `"pad_dpad_up"`, `"pad_lstick_left"`.
    pub fn name(self) -> &'static str {
        match self {
            PadInput::A => "pad_a",
            PadInput::B => "pad_b",
            PadInput::X => "pad_x",
            PadInput::Y => "pad_y",
            PadInput::LeftBumper => "pad_lb",
            PadInput::RightBumper => "pad_rb",
            PadInput::LeftTrigger => "pad_lt",
            PadInput::RightTrigger => "pad_rt",
            PadInput::Back => "pad_back",
            PadInput::Start => "pad_start",
            PadInput::Guide => "pad_guide",
            PadInput::LeftStickClick => "pad_lstick_click",
            PadInput::RightStickClick => "pad_rstick_click",
            PadInput::DpadUp => "pad_dpad_up",
            PadInput::DpadDown => "pad_dpad_down",
            PadInput::DpadLeft => "pad_dpad_left",
            PadInput::DpadRight => "pad_dpad_right",
            PadInput::LeftStickUp => "pad_lstick_up",
            PadInput::LeftStickDown => "pad_lstick_down",
            PadInput::LeftStickLeft => "pad_lstick_left",
            PadInput::LeftStickRight => "pad_lstick_right",
            PadInput::RightStickUp => "pad_rstick_up",
            PadInput::RightStickDown => "pad_rstick_down",
            PadInput::RightStickLeft => "pad_rstick_left",
            PadInput::RightStickRight => "pad_rstick_right",
        }
    }

    /// How it's shown in the Controls list.
    pub fn label(self) -> &'static str {
        match self {
            PadInput::A => "Pad A",
            PadInput::B => "Pad B",
            PadInput::X => "Pad X",
            PadInput::Y => "Pad Y",
            PadInput::LeftBumper => "Pad LB",
            PadInput::RightBumper => "Pad RB",
            PadInput::LeftTrigger => "Pad LT",
            PadInput::RightTrigger => "Pad RT",
            PadInput::Back => "Pad Back",
            PadInput::Start => "Pad Start",
            PadInput::Guide => "Pad Guide",
            PadInput::LeftStickClick => "Pad left stick click",
            PadInput::RightStickClick => "Pad right stick click",
            PadInput::DpadUp => "Pad d-pad up",
            PadInput::DpadDown => "Pad d-pad down",
            PadInput::DpadLeft => "Pad d-pad left",
            PadInput::DpadRight => "Pad d-pad right",
            PadInput::LeftStickUp => "Pad left stick up",
            PadInput::LeftStickDown => "Pad left stick down",
            PadInput::LeftStickLeft => "Pad left stick left",
            PadInput::LeftStickRight => "Pad left stick right",
            PadInput::RightStickUp => "Pad right stick up",
            PadInput::RightStickDown => "Pad right stick down",
            PadInput::RightStickLeft => "Pad right stick left",
            PadInput::RightStickRight => "Pad right stick right",
        }
    }

    /// By name, case-insensitively; the face buttons also by position
    /// (`pad_south` is `pad_a`, east `pad_b`, west `pad_x`, north `pad_y`).
    pub fn from_name(name: &str) -> Option<PadInput> {
        let name = name.to_ascii_lowercase();
        let alias = match name.as_str() {
            "pad_south" => Some(PadInput::A),
            "pad_east" => Some(PadInput::B),
            "pad_west" => Some(PadInput::X),
            "pad_north" => Some(PadInput::Y),
            _ => None,
        };
        alias.or_else(|| PadInput::ALL.into_iter().find(|p| p.name() == name))
    }

    fn from_button(button: Button) -> Option<PadInput> {
        Some(match button {
            Button::South => PadInput::A,
            Button::East => PadInput::B,
            Button::West => PadInput::X,
            Button::North => PadInput::Y,
            Button::LeftTrigger => PadInput::LeftBumper,
            Button::RightTrigger => PadInput::RightBumper,
            Button::LeftTrigger2 => PadInput::LeftTrigger,
            Button::RightTrigger2 => PadInput::RightTrigger,
            Button::Select => PadInput::Back,
            Button::Start => PadInput::Start,
            Button::Mode => PadInput::Guide,
            Button::LeftThumb => PadInput::LeftStickClick,
            Button::RightThumb => PadInput::RightStickClick,
            Button::DPadUp => PadInput::DpadUp,
            Button::DPadDown => PadInput::DpadDown,
            Button::DPadLeft => PadInput::DpadLeft,
            Button::DPadRight => PadInput::DpadRight,
            _ => return None,
        })
    }
}

/// A stick or trigger counts as pressed past this...
const PRESS: f32 = 0.5;
/// ...and lets go below this.
const RELEASE: f32 = 0.35;

/// The analogue axes scripts can read (`pad_axis`), in `PadFrame::axes`
/// order: sticks -1 to 1 (x right, y down, like screen coordinates),
/// triggers 0 to 1.
pub const AXIS_NAMES: [&str; 6] = ["lstick_x", "lstick_y", "rstick_x", "rstick_y", "lt", "rt"];

/// Stick movement smaller than this reads as 0 (worn sticks rest a little
/// off center).
const DEADZONE: f32 = 0.12;

/// This frame's pad input: what's held, what went down or up since the
/// last frame, the analogue axes, and the connected controllers' names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadFrame {
    pub down: Vec<PadInput>,
    pub pressed: Vec<PadInput>,
    pub released: Vec<PadInput>,
    pub axes: [f32; 6],
    pub connected: Vec<String>,
}

impl PadFrame {
    /// How far `input` is pressed, 0 to 1: a trigger how far it's pulled,
    /// a stick direction how far the stick is pushed that way (past the
    /// dead zone), anything else 0 or 1.
    pub fn value(&self, input: PadInput) -> f32 {
        let a = &self.axes;
        let analogue = match input {
            PadInput::LeftStickLeft => Some(-a[0]),
            PadInput::LeftStickRight => Some(a[0]),
            PadInput::LeftStickUp => Some(-a[1]),
            PadInput::LeftStickDown => Some(a[1]),
            PadInput::RightStickLeft => Some(-a[2]),
            PadInput::RightStickRight => Some(a[2]),
            PadInput::RightStickUp => Some(-a[3]),
            PadInput::RightStickDown => Some(a[3]),
            PadInput::LeftTrigger => Some(a[4]),
            PadInput::RightTrigger => Some(a[5]),
            _ => None,
        };
        let held = if self.down.contains(&input) { 1.0 } else { 0.0 };
        match analogue {
            // A trigger some pads only report as a button counts as fully pulled.
            Some(v) if v > 0.0 => v.min(1.0),
            _ => held,
        }
    }
}

/// Every connected controller, read once a frame.
pub struct Gamepads {
    gilrs: Option<Gilrs>,
    down: Vec<PadInput>,
}

impl Gamepads {
    pub fn new() -> Self {
        let gilrs = match Gilrs::new() {
            Ok(gilrs) => Some(gilrs),
            Err(e) => {
                log::warn!("game controllers unavailable: {e}");
                None
            }
        };
        Self { gilrs, down: Vec::new() }
    }

    /// Read the controllers. `focused`: whether the app's window has focus;
    /// while it doesn't, nothing counts as held (so everything held lets
    /// go), and a game in the background doesn't react to the pad.
    pub fn poll(&mut self, focused: bool) -> PadFrame {
        let Some(gilrs) = &mut self.gilrs else { return PadFrame::default() };
        // Presses from events too, so one shorter than a frame still counts.
        let mut pressed = Vec::new();
        while let Some(event) = gilrs.next_event() {
            if let EventType::ButtonPressed(button, _) = event.event
                && let Some(input) = PadInput::from_button(button)
                && focused
                && !pressed.contains(&input)
            {
                pressed.push(input);
            }
        }
        let mut down = Vec::new();
        if focused {
            for (_, pad) in gilrs.gamepads() {
                for input in PadInput::ALL {
                    let was = self.down.contains(&input);
                    let on = match input {
                        PadInput::LeftStickUp => axis(pad.value(Axis::LeftStickY), was),
                        PadInput::LeftStickDown => axis(-pad.value(Axis::LeftStickY), was),
                        PadInput::LeftStickLeft => axis(-pad.value(Axis::LeftStickX), was),
                        PadInput::LeftStickRight => axis(pad.value(Axis::LeftStickX), was),
                        PadInput::RightStickUp => axis(pad.value(Axis::RightStickY), was),
                        PadInput::RightStickDown => axis(-pad.value(Axis::RightStickY), was),
                        PadInput::RightStickLeft => axis(-pad.value(Axis::RightStickX), was),
                        PadInput::RightStickRight => axis(pad.value(Axis::RightStickX), was),
                        // Some pads report the d-pad as an axis instead.
                        PadInput::DpadUp => pad.is_pressed(Button::DPadUp) || axis(pad.value(Axis::DPadY), was),
                        PadInput::DpadDown => pad.is_pressed(Button::DPadDown) || axis(-pad.value(Axis::DPadY), was),
                        PadInput::DpadLeft => pad.is_pressed(Button::DPadLeft) || axis(-pad.value(Axis::DPadX), was),
                        PadInput::DpadRight => pad.is_pressed(Button::DPadRight) || axis(pad.value(Axis::DPadX), was),
                        PadInput::LeftTrigger => {
                            pad.is_pressed(Button::LeftTrigger2) || axis(pad.value(Axis::LeftZ), was)
                        }
                        PadInput::RightTrigger => {
                            pad.is_pressed(Button::RightTrigger2) || axis(pad.value(Axis::RightZ), was)
                        }
                        other => gilrs_button(other).is_some_and(|b| pad.is_pressed(b)),
                    };
                    if on && !down.contains(&input) {
                        down.push(input);
                    }
                }
            }
        }
        for input in &down {
            if !self.down.contains(input) && !pressed.contains(input) {
                pressed.push(*input);
            }
        }
        let released = self.down.iter().copied().filter(|i| !down.contains(i)).collect();
        self.down = down.clone();

        // Each axis from whichever pad pushes it furthest.
        let mut axes = [0.0f32; 6];
        if focused {
            for (_, pad) in gilrs.gamepads() {
                let stick = |v: f32| if v.abs() < DEADZONE { 0.0 } else { v.clamp(-1.0, 1.0) };
                let trigger = |button: Button, axis: Axis| {
                    let pressed = pad.button_data(button).map_or(0.0, |b| b.value());
                    pressed.max(pad.value(axis)).clamp(0.0, 1.0)
                };
                let values = [
                    stick(pad.value(Axis::LeftStickX)),
                    stick(-pad.value(Axis::LeftStickY)),
                    stick(pad.value(Axis::RightStickX)),
                    stick(-pad.value(Axis::RightStickY)),
                    trigger(Button::LeftTrigger2, Axis::LeftZ),
                    trigger(Button::RightTrigger2, Axis::RightZ),
                ];
                for (slot, value) in axes.iter_mut().zip(values) {
                    if value.abs() > slot.abs() {
                        *slot = value;
                    }
                }
            }
        }
        let connected = gilrs.gamepads().map(|(_, pad)| pad.name().to_string()).collect();
        PadFrame { down, pressed, released, axes, connected }
    }
}

/// An axis value as on/off, with hysteresis around the threshold.
fn axis(value: f32, was: bool) -> bool {
    if was { value > RELEASE } else { value > PRESS }
}

fn gilrs_button(input: PadInput) -> Option<Button> {
    Some(match input {
        PadInput::A => Button::South,
        PadInput::B => Button::East,
        PadInput::X => Button::West,
        PadInput::Y => Button::North,
        PadInput::LeftBumper => Button::LeftTrigger,
        PadInput::RightBumper => Button::RightTrigger,
        PadInput::Back => Button::Select,
        PadInput::Start => Button::Start,
        PadInput::Guide => Button::Mode,
        PadInput::LeftStickClick => Button::LeftThumb,
        PadInput::RightStickClick => Button::RightThumb,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_aliases_work() {
        for input in PadInput::ALL {
            assert_eq!(PadInput::from_name(input.name()), Some(input));
        }
        assert_eq!(PadInput::from_name("PAD_South"), Some(PadInput::A));
        assert_eq!(PadInput::from_name("pad_nope"), None);
    }

    #[test]
    fn sticks_have_slack_before_letting_go() {
        assert!(!axis(0.45, false));
        assert!(axis(0.6, false));
        assert!(axis(0.45, true), "still held between the thresholds");
        assert!(!axis(0.2, true));
    }
}
