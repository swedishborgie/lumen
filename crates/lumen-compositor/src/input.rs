use smithay::{
    backend::input::{Axis, ButtonState, KeyState},
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Point, Serial, SERIAL_COUNTER},
    wayland::seat::WaylandFocus,
};

use crate::state::AppState;

/// Events sent from the browser to the compositor via the WebRTC data channel.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
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
        /// Horizontal scroll delta.
        x: f64,
        /// Vertical scroll delta.
        y: f64,
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
            inject_pointer_button(state, serial, time, btn, btn_state);
        }
        InputEvent::PointerAxis { x, y } => {
            inject_pointer_axis(state, time, x, y);
        }
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

    // Resolve which surface is under the cursor.
    let focus = state.space.element_under(location)
        .and_then(|(w, loc)| {
            w.wl_surface().map(|s| ((*s).clone(), loc.to_f64()))
        });

    pointer.motion(state, focus, &MotionEvent { location, serial, time });
    pointer.frame(state);
}

fn inject_pointer_button(state: &mut AppState, serial: Serial, time: u32, btn: u32, btn_state: u32) {
    let pointer = state.seat.get_pointer().unwrap();
    let button_state = if btn_state == 1 { ButtonState::Pressed } else { ButtonState::Released };
    pointer.button(state, &ButtonEvent { serial, time, button: btn, state: button_state });
    pointer.frame(state);
}

fn inject_pointer_axis(state: &mut AppState, time: u32, x: f64, y: f64) {
    let pointer = state.seat.get_pointer().unwrap();
    let mut frame = AxisFrame::new(time);
    if x != 0.0 { frame = frame.value(Axis::Horizontal, x); }
    if y != 0.0 { frame = frame.value(Axis::Vertical, y); }
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
