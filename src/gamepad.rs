//! Game controllers, as inputs script actions can be bound to
//! (`input_register`, the Controls window).
//!
//! Every connected pad counts the same: pressing A on any of them presses
//! "pad_a". Buttons, the d-pad, the triggers and each stick pushed one way
//! are all on/off inputs: a stick or trigger counts as pressed past
//! halfway (with a little slack before it lets go, so it doesn't flicker
//! at the edge). Names follow the Xbox layout: A is the bottom face button
//! whatever the pad prints on it (Cross on a PlayStation pad).

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
}

/// The analogue axes scripts can read (`pad_axis`), in `PadFrame::axes`
/// order: sticks -1 to 1 (x right, y down, like screen coordinates),
/// triggers 0 to 1.
pub const AXIS_NAMES: [&str; 6] = ["lstick_x", "lstick_y", "rstick_x", "rstick_y", "lt", "rt"];

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

#[cfg(not(target_os = "android"))]
mod gilrs_pads;
#[cfg(not(target_os = "android"))]
pub use gilrs_pads::Gamepads;

/// Controllers on Android: none. gilrs has no Android backend, and the
/// native activity hands every input event to winit, which drops the
/// controller ones.
#[cfg(target_os = "android")]
pub struct Gamepads;

#[cfg(target_os = "android")]
impl Gamepads {
    pub fn new() -> Self {
        Self
    }

    pub fn poll(&mut self, _focused: bool) -> PadFrame {
        PadFrame::default()
    }
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
}
