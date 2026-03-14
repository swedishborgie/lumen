use smithay::{
    backend::input::{Axis, AxisSource, ButtonState, KeyState},
    desktop::WindowSurfaceType,
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
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
