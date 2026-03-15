//! Mapping from Web Gamepad Standard Layout button/axis indices to Linux evdev codes.
//!
//! Reference: <https://w3c.github.io/gamepad/#dfn-standard-gamepad-layout>

use evdev::AbsoluteAxisType as Abs;
use evdev::Key;

/// Maps a Web Gamepad button index (0–16) to a Linux `BTN_*` evdev key code.
///
/// Returns `None` for indices that have no button mapping (e.g. trigger buttons
/// that are only reported as absolute axes).
pub const BUTTON_MAP: &[(usize, Key)] = &[
    (0,  Key::BTN_SOUTH),    // A / Cross
    (1,  Key::BTN_EAST),     // B / Circle
    (2,  Key::BTN_WEST),     // X / Square   (note: evdev BTN_WEST = 0x134)
    (3,  Key::BTN_NORTH),    // Y / Triangle (note: evdev BTN_NORTH = 0x133)
    (4,  Key::BTN_TL),       // LB
    (5,  Key::BTN_TR),       // RB
    (6,  Key::BTN_TL2),      // LT (digital; also emits ABS_Z)
    (7,  Key::BTN_TR2),      // RT (digital; also emits ABS_RZ)
    (8,  Key::BTN_SELECT),   // Select / Back
    (9,  Key::BTN_START),    // Start
    (10, Key::BTN_THUMBL),   // L3 – left stick click
    (11, Key::BTN_THUMBR),   // R3 – right stick click
    (12, Key::BTN_DPAD_UP),
    (13, Key::BTN_DPAD_DOWN),
    (14, Key::BTN_DPAD_LEFT),
    (15, Key::BTN_DPAD_RIGHT),
    (16, Key::BTN_MODE),     // Home / Guide
];

/// Maps a Web Gamepad axis index (0–3) to a Linux `ABS_*` evdev axis code.
pub const AXIS_MAP: &[(usize, Abs)] = &[
    (0, Abs::ABS_X),   // Left stick X
    (1, Abs::ABS_Y),   // Left stick Y
    (2, Abs::ABS_RX),  // Right stick X
    (3, Abs::ABS_RY),  // Right stick Y
];

/// Trigger button indices that additionally emit absolute axis events.
/// Web button index → ABS axis code.
pub const TRIGGER_AXIS_MAP: &[(usize, Abs)] = &[
    (6, Abs::ABS_Z),   // LT → ABS_Z
    (7, Abs::ABS_RZ),  // RT → ABS_RZ
];

/// Scale factor converting a normalised axis value (−1.0 to 1.0) to the
/// i32 range expected by evdev (−32 767 to 32 767).
pub const AXIS_SCALE: f32 = 32_767.0;

/// Scale factor converting a trigger value (0.0 to 1.0) to the i32 range
/// used for ABS_Z / ABS_RZ (0 to 255).
pub const TRIGGER_SCALE: f32 = 255.0;
