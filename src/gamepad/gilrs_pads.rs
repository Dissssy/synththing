//! Controllers read through gilrs (every platform but Android).

use gilrs::{Axis, Button, EventType, Gilrs};

use super::{PadFrame, PadInput};

/// A stick or trigger counts as pressed past this...
const PRESS: f32 = 0.5;
/// ...and lets go below this.
const RELEASE: f32 = 0.35;

/// Stick movement smaller than this reads as 0 (worn sticks rest a little
/// off center).
const DEADZONE: f32 = 0.12;

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
                && let Some(input) = from_button(button)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticks_have_slack_before_letting_go() {
        assert!(!axis(0.45, false));
        assert!(axis(0.6, false));
        assert!(axis(0.45, true), "still held between the thresholds");
        assert!(!axis(0.2, true));
    }
}
