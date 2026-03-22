//! Mapping from Web Gamepad Standard Layout button/axis indices to Linux evdev codes.
//!
//! Reference: <https://w3c.github.io/gamepad/#dfn-standard-gamepad-layout>
//!
//! # Axis layout
//!
//! The W3C Standard Gamepad Layout defines only 4 axes (0–3: left X/Y, right
//! X/Y) with triggers as buttons.  Many controllers, including most 8BitDo
//! devices, expose raw evdev axes to the browser in kernel code order:
//!
//! | Browser axis | evdev axis  | Role                          |
//! |--------------|-------------|-------------------------------|
//! | 0            | ABS_X       | Left stick X                  |
//! | 1            | ABS_Y       | Left stick Y                  |
//! | 2            | ABS_Z       | Left trigger  (−1 → 1)        |
//! | 3            | ABS_RX      | Right stick X                 |
//! | 4            | ABS_RY      | Right stick Y                 |
//! | 5            | ABS_RZ      | Right trigger (−1 → 1)        |
//! | 6            | ABS_HAT0X   | D-pad X                       |
//! | 7            | ABS_HAT0Y   | D-pad Y                       |
//!
//! `AXIS_MAP` covers stick axes only (scaled ±32 767).
//! `TRIGGER_FROM_AXIS_MAP` covers trigger axes reported as browser axes
//! (browser range −1..1 → evdev range 0..255).
//! `TRIGGER_AXIS_MAP` covers the standard-layout case where triggers are
//! reported as buttons (indices 6/7) with an analog value.
//! `HAT_AXIS_MAP` covers hat/D-pad axes (browser −1..1 → evdev −1..1).

use evdev::AbsoluteAxisCode as Abs;
use evdev::KeyCode;

/// Maps a Web Gamepad button index (0–16) to a Linux `BTN_*` evdev key code.
///
/// Used for W3C standard-layout controllers where triggers are reported as
/// buttons (indices 6/7) with an analog value.
pub const BUTTON_MAP: &[(usize, KeyCode)] = &[
    (0,  KeyCode::BTN_SOUTH),    // A / Cross
    (1,  KeyCode::BTN_EAST),     // B / Circle
    (2,  KeyCode::BTN_WEST),     // X / Square
    (3,  KeyCode::BTN_NORTH),    // Y / Triangle
    (4,  KeyCode::BTN_TL),       // LB
    (5,  KeyCode::BTN_TR),       // RB
    (6,  KeyCode::BTN_TL2),      // LT digital (standard layout only; analog via TRIGGER_AXIS_MAP)
    (7,  KeyCode::BTN_TR2),      // RT digital (standard layout only; analog via TRIGGER_AXIS_MAP)
    (8,  KeyCode::BTN_SELECT),   // Select / Back
    (9,  KeyCode::BTN_START),    // Start
    (10, KeyCode::BTN_THUMBL),   // L3 – left stick click
    (11, KeyCode::BTN_THUMBR),   // R3 – right stick click
    (12, KeyCode::BTN_DPAD_UP),
    (13, KeyCode::BTN_DPAD_DOWN),
    (14, KeyCode::BTN_DPAD_LEFT),
    (15, KeyCode::BTN_DPAD_RIGHT),
    (16, KeyCode::BTN_MODE),     // Home / Guide
];

/// Maps a Web Gamepad button index to a Linux `BTN_*` evdev key code for
/// controllers that expose triggers as analog axes (raw evdev layout).
///
/// In the raw layout the browser has no digital trigger buttons at indices 6/7,
/// so those slots shift to Back/Start.  The face-button and bumper entries
/// (0–5) are identical to [`BUTTON_MAP`].
pub const RAW_BUTTON_MAP: &[(usize, KeyCode)] = &[
    (0,  KeyCode::BTN_SOUTH),   // A / Cross
    (1,  KeyCode::BTN_EAST),    // B / Circle
    (2,  KeyCode::BTN_WEST),    // X / Square
    (3,  KeyCode::BTN_NORTH),   // Y / Triangle
    (4,  KeyCode::BTN_TL),      // LB
    (5,  KeyCode::BTN_TR),      // RB
    (6,  KeyCode::BTN_SELECT),  // Select / Back (no digital triggers in raw layout)
    (7,  KeyCode::BTN_START),   // Start
    (8,  KeyCode::BTN_MODE),    // Home / Guide
    (9,  KeyCode::BTN_THUMBL),  // L3 – left stick click
    (10, KeyCode::BTN_THUMBR),  // R3 – right stick click
];

/// Stick axes: Web axis index → ABS axis.  Scaled by [`AXIS_SCALE`] (±32 767).
pub const AXIS_MAP: &[(usize, Abs)] = &[
    (0, Abs::ABS_X),   // Left stick X
    (1, Abs::ABS_Y),   // Left stick Y
    (3, Abs::ABS_RX),  // Right stick X
    (4, Abs::ABS_RY),  // Right stick Y
];

/// Trigger axes reported as browser axes (range −1.0 to 1.0).
/// Converted to evdev range 0–255 via [`TRIGGER_FROM_AXIS_SCALE`].
/// Used when the controller exposes triggers as axes (e.g. raw evdev layout).
pub const TRIGGER_FROM_AXIS_MAP: &[(usize, Abs)] = &[
    (2, Abs::ABS_Z),   // Left trigger
    (5, Abs::ABS_RZ),  // Right trigger
];

/// Trigger buttons that also drive ABS_Z / ABS_RZ (W3C standard layout).
/// Used when the controller reports triggers as buttons (indices 6/7).
pub const TRIGGER_AXIS_MAP: &[(usize, Abs)] = &[
    (6, Abs::ABS_Z),   // LT → ABS_Z
    (7, Abs::ABS_RZ),  // RT → ABS_RZ
];

/// Hat/D-pad axes: Web axis index → ABS axis.  Values are −1, 0, or 1.
pub const HAT_AXIS_MAP: &[(usize, Abs)] = &[
    (6, Abs::ABS_HAT0X),
    (7, Abs::ABS_HAT0Y),
];

/// Scale factor: normalised stick value (−1.0 to 1.0) → evdev i32 (−32 767 to 32 767).
pub const AXIS_SCALE: f32 = 32_767.0;

/// Scale factor: trigger axis value after shifting from −1..1 to 0..1 → evdev 0..255.
pub const TRIGGER_FROM_AXIS_SCALE: f32 = 255.0;

/// Scale factor: trigger button value (0.0 to 1.0) → evdev 0..255.
pub const TRIGGER_SCALE: f32 = 255.0;
