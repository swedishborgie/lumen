//! Virtual gamepad device management for the Lumen compositor.
//!
//! Creates up to [`MAX_GAMEPADS`] virtual Linux input devices via `uinput` — one
//! per browser-connected gamepad.  Applications (SDL2, libinput, raw evdev) see
//! these as ordinary `/dev/input/eventXXX` devices without any LD_PRELOAD magic.
//!
//! # Prerequisites
//!
//! The process must have write access to `/dev/uinput`.  On most systems this
//! requires membership of the `input` group or a udev rule such as:
//!
//! ```text
//! KERNEL=="uinput", MODE="0660", GROUP="input"
//! ```
//!
//! If `/dev/uinput` is inaccessible, [`GamepadManager::new`] returns
//! `Err(GamepadError::UinputOpen)` and the caller should log a warning and
//! continue without gamepad support.

mod device;
mod mapping;

use std::collections::HashMap;

use thiserror::Error;
use tracing::{debug, info, warn};

use device::GamepadDevice;

/// Maximum number of simultaneously connected virtual gamepads.
pub const MAX_GAMEPADS: u8 = 4;

/// Errors that can occur in the gamepad subsystem.
#[derive(Debug, Error)]
pub enum GamepadError {
    #[error("failed to open /dev/uinput: {0}")]
    UinputOpen(#[source] std::io::Error),

    #[error("failed to configure uinput device: {0}")]
    DeviceSetup(#[source] std::io::Error),

    #[error("failed to emit input event: {0}")]
    Emit(#[source] std::io::Error),

    #[error("gamepad index {0} is out of range (max {MAX_GAMEPADS})")]
    IndexOutOfRange(u8),

    #[error("gamepad index {0} is not connected")]
    NotConnected(u8),

    #[error("gamepad index {0} is already connected")]
    AlreadyConnected(u8),
}

/// An event from the browser's Web Gamepad API to be applied to the virtual device.
#[derive(Debug, Clone)]
pub enum GamepadEvent {
    /// A new gamepad was connected in the browser.
    Connected {
        /// Gamepad slot index (0–3).
        index: u8,
        /// Human-readable name from the browser (e.g. `"Xbox 360 Controller"`).
        name: String,
        /// Number of axes reported by the browser.
        num_axes: u8,
        /// Number of buttons reported by the browser.
        num_buttons: u8,
    },
    /// A gamepad was disconnected in the browser.
    Disconnected {
        /// Gamepad slot index (0–3).
        index: u8,
    },
    /// A button changed state.
    Button {
        /// Gamepad slot index (0–3).
        index: u8,
        /// Web Gamepad button index (0–16 for the standard layout).
        button: u8,
        /// Analog value in the range 0.0–1.0.
        value: f32,
        /// Whether the button is considered pressed.
        pressed: bool,
    },
    /// An axis changed value.
    Axis {
        /// Gamepad slot index (0–3).
        index: u8,
        /// Web Gamepad axis index (0–3 for the standard layout).
        axis: u8,
        /// Normalised value in the range −1.0 to 1.0.
        value: f32,
    },
}

/// Manages up to [`MAX_GAMEPADS`] virtual uinput gamepad devices.
///
/// This type is `!Send`; run it inside [`tokio::task::spawn_blocking`].
pub struct GamepadManager {
    devices: HashMap<u8, GamepadDevice>,
}

impl GamepadManager {
    /// Create a new manager.  This does **not** open `/dev/uinput` yet; devices
    /// are created on demand when [`GamepadEvent::Connected`] is received.
    pub fn new() -> Self {
        Self { devices: HashMap::new() }
    }

    /// Process a single gamepad event, updating or creating virtual devices as
    /// needed.  Non-fatal errors (e.g. unknown button/axis index) are logged as
    /// warnings and silently dropped so that one bad message cannot stall input.
    pub fn handle_event(&mut self, event: GamepadEvent) {
        if let Err(err) = self.try_handle_event(event) {
            warn!("gamepad: {err}");
        }
    }

    fn try_handle_event(&mut self, event: GamepadEvent) -> Result<(), GamepadError> {
        match event {
            GamepadEvent::Connected { index, name, num_axes, num_buttons } => {
                self.connect(index, &name, num_axes, num_buttons)
            }
            GamepadEvent::Disconnected { index } => self.disconnect(index),
            GamepadEvent::Button { index, button, value, pressed } => {
                self.button(index, button, value, pressed)
            }
            GamepadEvent::Axis { index, axis, value } => self.axis(index, axis, value),
        }
    }

    fn connect(
        &mut self,
        index: u8,
        name: &str,
        num_axes: u8,
        num_buttons: u8,
    ) -> Result<(), GamepadError> {
        if index >= MAX_GAMEPADS {
            return Err(GamepadError::IndexOutOfRange(index));
        }
        if self.devices.contains_key(&index) {
            // Browser may re-send connected; treat as reconnect.
            warn!("gamepad {index}: already connected, replacing device");
            self.devices.remove(&index);
        }
        let device_name = format!("Lumen Gamepad {index} ({name})");
        let device = GamepadDevice::new(&device_name, num_buttons, num_axes)?;
        info!("gamepad {index}: virtual device created — {num_buttons} buttons, {num_axes} axes");
        self.devices.insert(index, device);
        Ok(())
    }

    fn disconnect(&mut self, index: u8) -> Result<(), GamepadError> {
        if self.devices.remove(&index).is_some() {
            info!("gamepad {index}: virtual device removed");
        } else {
            debug!("gamepad {index}: disconnect received but device was not present");
        }
        Ok(())
    }

    fn button(&mut self, index: u8, button: u8, value: f32, pressed: bool) -> Result<(), GamepadError> {
        let dev = self
            .devices
            .get_mut(&index)
            .ok_or(GamepadError::NotConnected(index))?;
        dev.send_button(button, pressed, value)
    }

    fn axis(&mut self, index: u8, axis: u8, value: f32) -> Result<(), GamepadError> {
        let dev = self
            .devices
            .get_mut(&index)
            .ok_or(GamepadError::NotConnected(index))?;
        dev.send_axis(axis, value)
    }
}

impl Default for GamepadManager {
    fn default() -> Self {
        Self::new()
    }
}
