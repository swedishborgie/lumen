use smithay::{
    backend::input::{Axis, AxisSource, ButtonState, KeyState},
    desktop::WindowSurfaceType,
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Point, Serial, SERIAL_COUNTER},
    wayland::seat::WaylandFocus,
};

use crate::state::AppState;

/// Find the actual Wayland surface (including subsurfaces and popups) under `location`.
///
/// `Space::element_under` only matches the top-level window element and returns the window's root
/// wl_surface.  That means popup context-menu surfaces never receive pointer events, which breaks
/// clipboard copy via right-click (the app's `set_selection` serial doesn't match any delivered
/// event).  Using `Window::surface_under(WindowSurfaceType::ALL)` walks the popup tree so popup
/// surfaces receive motion and button events with the correct serials.
fn surface_under_at(
    state: &AppState,
    location: Point<f64, smithay::utils::Logical>,
) -> Option<(WlSurface, Point<i32, smithay::utils::Logical>)> {
    state.space.elements().rev().find_map(|window| {
        let window_loc = state.space.element_location(window)?;
        let relative = location - window_loc.to_f64();
        window.surface_under(relative, WindowSurfaceType::ALL)
    })
}

/// Events sent from the browser to the compositor via the WebRTC data channel.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    KeyboardKey {
        /// Linux evdev scancode (offset by 8 for XKB).
        scancode: u32,
        /// 1 = pressed, 0 = released.
        state: u32,
    },
    PointerMotion {
        x: f64,
        y: f64,
    },
    /// Relative pointer motion, used when the browser has acquired pointer lock
    /// (e.g. fullscreen games). Carries raw, unaccelerated, unclamped deltas in
    /// compositor logical pixel space. The compositor sends these as
    /// `zwp_relative_pointer_v1` events so that games receive the motion they
    /// need, while also delivering a `wl_pointer.motion` at the updated absolute
    /// position for surface-focus purposes.
    PointerMotionRelative {
        dx: f64,
        dy: f64,
    },
    PointerButton {
        /// Linux evdev button code (e.g. BTN_LEFT = 0x110).
        btn: u32,
        /// 1 = pressed, 0 = released.
        state: u32,
    },
    PointerAxis {
        /// Horizontal scroll delta in pixels.
        x: f64,
        /// Vertical scroll delta in pixels.
        y: f64,
        /// Axis source: `"wheel"` for mouse wheel, `"continuous"` for touchpad.
        #[serde(default)]
        source: Option<String>,
        /// High-resolution scroll for horizontal axis (multiples of 120 per notch).
        #[serde(default)]
        v120_x: Option<i32>,
        /// High-resolution scroll for vertical axis (multiples of 120 per notch).
        #[serde(default)]
        v120_y: Option<i32>,
    },
    /// Request to set the compositor clipboard to the given text.
    ClipboardWrite {
        text: String,
    },

    // ── Gamepad events ────────────────────────────────────────────────────────
    // These are dispatched at the orchestration layer (main.rs) to lumen-gamepad
    // and are never injected into the Smithay seat.

    /// A gamepad was connected in the browser.
    GamepadConnected {
        /// Gamepad slot index (0–3).
        index: u8,
        /// Human-readable name from the browser.
        name: String,
        /// The `Gamepad.mapping` string from the browser (`"standard"` or `""`).
        mapping: String,
        /// Per-button capability declarations, indexed by browser button index.
        ///
        /// `None` means no mapping has been provided yet (non-standard controller
        /// that the user hasn't mapped).  Each inner `Option` may also be `None`
        /// for buttons that were skipped during the mapping wizard.
        buttons: Option<Vec<Option<ButtonMapping>>>,
        /// Per-axis capability declarations, indexed by browser axis index.
        ///
        /// `None` when `buttons` is `None`.  Each inner `Option` may be `None`
        /// for axes that were skipped during the mapping wizard.
        axes: Option<Vec<Option<AxisMapping>>>,
    },
    /// A gamepad was disconnected in the browser.
    GamepadDisconnected {
        /// Gamepad slot index (0–3).
        index: u8,
    },
    /// A gamepad button changed state.
    GamepadButton {
        /// Gamepad slot index (0–3).
        index: u8,
        /// Raw browser button index.  The compositor looks up the evdev code
        /// from the capability declaration sent with `GamepadConnected`.
        button: u8,
        /// Analog value in the range 0.0–1.0.
        value: f32,
        /// Whether the button is considered pressed.
        pressed: bool,
    },
    /// A gamepad axis changed value.
    GamepadAxis {
        /// Gamepad slot index (0–3).
        index: u8,
        /// Raw browser axis index.  The compositor looks up the evdev code
        /// from the capability declaration sent with `GamepadConnected`.
        axis: u8,
        /// Normalised value in the range −1.0 to 1.0.
        value: f32,
    },
}

/// Evdev capability declaration for a single gamepad button.
///
/// Sent as part of [`InputEvent::GamepadConnected`] so the compositor can build
/// a virtual uinput device without any hardcoded layout knowledge.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ButtonMapping {
    /// Linux `BTN_*` evdev key code.
    pub btn_code: u16,
    /// If set, the button also drives this `ABS_*` analog axis (e.g. LT/RT
    /// triggers in the standard layout drive `ABS_Z`/`ABS_RZ`).
    pub trigger_abs_code: Option<u16>,
}

/// Evdev capability declaration for a single gamepad axis.
///
/// Sent as part of [`InputEvent::GamepadConnected`] so the compositor can build
/// a virtual uinput device without any hardcoded layout knowledge.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AxisMapping {
    /// Linux `ABS_*` evdev absolute-axis code.
    pub abs_code: u16,
}

/// Inject an [`InputEvent`] into the Smithay seat, dispatching it to the
/// currently focused Wayland surface.
pub fn inject_input(state: &mut AppState, event: InputEvent) {
    let serial = SERIAL_COUNTER.next_serial();
    let time = current_time_ms();

    match event {
        InputEvent::KeyboardKey { scancode, state: key_state } => {
            inject_key(state, serial, time, scancode, key_state);
        }
        InputEvent::PointerMotion { x, y } => {
            inject_pointer_motion(state, serial, time, x, y);
        }
        InputEvent::PointerMotionRelative { dx, dy } => {
            inject_pointer_motion_relative(state, serial, time, dx, dy);
        }
        InputEvent::PointerButton { btn, state: btn_state } => {
            let n_windows = state.space.elements().count();
            tracing::debug!(btn, btn_state, n_windows, "inject_input: PointerButton");
            inject_pointer_button(state, serial, time, btn, btn_state);
        }
        InputEvent::PointerAxis { x, y, source, v120_x, v120_y } => {
            inject_pointer_axis(state, time, x, y, source, v120_x, v120_y);
        }
        // ClipboardWrite is handled at the orchestration layer (main.rs) before
        // reaching inject_input; this arm is a safety fallback.
        InputEvent::ClipboardWrite { .. } => {}
        // Gamepad events are routed to lumen-gamepad by main.rs and never reach
        // the Smithay seat.
        InputEvent::GamepadConnected { .. }
        | InputEvent::GamepadDisconnected { .. }
        | InputEvent::GamepadButton { .. }
        | InputEvent::GamepadAxis { .. } => {}
    }
}

fn inject_key(state: &mut AppState, serial: Serial, time: u32, scancode: u32, key_state: u32) {
    let keyboard = state.seat.get_keyboard().unwrap();

    // XKB keycodes are evdev scancode + 8.
    let keycode = Keycode::from(scancode + 8);
    let smithay_state = if key_state == 1 { KeyState::Pressed } else { KeyState::Released };

    // Set focus to the topmost window surface if we don't already have one.
    let focus_surface: Option<WlSurface> = state.space.elements()
        .rev()
        .find_map(|w| w.wl_surface().map(|s| (*s).clone()));

    tracing::debug!(scancode, xkb_code = scancode + 8, ?smithay_state,
        has_focus = focus_surface.is_some(), "inject_key");

    if let Some(ref surface) = focus_surface {
        keyboard.set_focus(state, Some(surface.clone()), serial);
    }

    keyboard.input::<(), _>(
        state,
        keycode,
        smithay_state,
        serial,
        time,
        |_, _, _| FilterResult::Forward,
    );
}

fn inject_pointer_motion(state: &mut AppState, serial: Serial, time: u32, x: f64, y: f64) {
    let pointer = state.seat.get_pointer().unwrap();
    let location = Point::from((x, y));

    // Keep cursor_pos in sync so relative motion can compute a correct
    // absolute position when it's delivered later.
    state.cursor_pos = location;

    // Resolve the actual surface under the cursor, including popup surfaces.
    let focus = surface_under_at(state, location)
        .map(|(s, offset)| (s, offset.to_f64()));

    if focus.is_none() {
        let n = state.space.elements().count();
        tracing::debug!(x, y, n_windows = n, "inject_pointer_motion: no surface under cursor");
    }

    pointer.motion(state, focus, &MotionEvent { location, serial, time });
    pointer.frame(state);
}

/// Inject a relative pointer motion event.
///
/// Called when the browser has pointer lock active (e.g. a fullscreen game).
/// Delivers both a `zwp_relative_pointer_v1` event (which games consume for
/// camera / aim control) and a `wl_pointer.motion` at the updated absolute
/// position (for surface-focus bookkeeping and non-relative-aware clients).
fn inject_pointer_motion_relative(state: &mut AppState, serial: Serial, time: u32, dx: f64, dy: f64) {
    let pointer = state.seat.get_pointer().unwrap();

    // Advance the virtual cursor position and clamp to the output bounds so
    // that button events after a relative-motion sequence still hit the right
    // surface even when the game has been moving the mouse past screen edges.
    let new_x = (state.cursor_pos.x + dx).clamp(0.0, (state.width - 1).max(0) as f64);
    let new_y = (state.cursor_pos.y + dy).clamp(0.0, (state.height - 1).max(0) as f64);
    state.cursor_pos = Point::from((new_x, new_y));

    // Resolve the surface under the updated cursor position.
    let focus = surface_under_at(state, state.cursor_pos)
        .map(|(s, offset)| (s, offset.to_f64()));

    // Deliver the relative motion event — this is what fullscreen games (e.g.
    // Steam games using SDL) rely on for mouse-look / camera control.
    // `utime` is a microsecond timestamp as required by the protocol.
    let utime = current_time_ms() as u64 * 1000;
    pointer.relative_motion(
        state,
        focus.clone(),
        &RelativeMotionEvent {
            delta: Point::from((dx, dy)),
            delta_unaccel: Point::from((dx, dy)),
            utime,
        },
    );

    // Also deliver an absolute motion event so that the focused surface's
    // wl_pointer.enter / wl_pointer.motion serial stays up to date, and so
    // that any client that does not use relative pointer still gets movement.
    pointer.motion(state, focus, &MotionEvent { location: state.cursor_pos, serial, time });
    pointer.frame(state);
}

fn inject_pointer_button(state: &mut AppState, serial: Serial, time: u32, btn: u32, btn_state: u32) {
    // On press, set keyboard focus to the topmost window's surface.  This is needed for
    // wl_data_device::set_selection (copy) to work when the user copies via right-click
    // menu without any prior keyboard interaction.  We focus the toplevel (not the popup)
    // because keyboard focus belongs to the application window, not its transient popups.
    if btn_state == 1 {
        let keyboard = state.seat.get_keyboard().unwrap();
        let focus_surface: Option<WlSurface> = state.space.elements()
            .rev()
            .find_map(|w| w.wl_surface().map(|s| (*s).clone()));
        if let Some(surface) = focus_surface {
            keyboard.set_focus(state, Some(surface), serial);
        }
    }

    let pointer = state.seat.get_pointer().unwrap();
    let button_state = if btn_state == 1 { ButtonState::Pressed } else { ButtonState::Released };
    pointer.button(state, &ButtonEvent { serial, time, button: btn, state: button_state });
    pointer.frame(state);
}

fn inject_pointer_axis(
    state: &mut AppState,
    time: u32,
    x: f64,
    y: f64,
    source: Option<String>,
    v120_x: Option<i32>,
    v120_y: Option<i32>,
) {
    let pointer = state.seat.get_pointer().unwrap();

    let axis_src = match source.as_deref() {
        Some("wheel")      => Some(AxisSource::Wheel),
        Some("continuous") => Some(AxisSource::Continuous),
        Some("finger")     => Some(AxisSource::Finger),
        _                  => None,
    };

    let mut frame = AxisFrame::new(time);
    if let Some(src) = axis_src {
        frame = frame.source(src);
    }
    if x != 0.0 {
        frame = frame.value(Axis::Horizontal, x);
        if let Some(v) = v120_x { frame = frame.v120(Axis::Horizontal, v); }
    }
    if y != 0.0 {
        frame = frame.value(Axis::Vertical, y);
        if let Some(v) = v120_y { frame = frame.v120(Axis::Vertical, v); }
    }
    pointer.axis(state, frame);
    pointer.frame(state);
}

fn current_time_ms() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Wrapping cast is intentional: Wayland timestamps are u32 milliseconds.
    #[allow(clippy::cast_possible_truncation)]
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0)
}
