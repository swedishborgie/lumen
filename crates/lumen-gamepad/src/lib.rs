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

use std::collections::HashMap;

use thiserror::Error;
use tracing::{debug, info, trace, warn};

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

/// Evdev capability declaration for a single gamepad button.
///
/// Received once at connect time as part of [`GamepadEvent::Connected`].
/// Stored by the device to look up evdev codes when raw-index poll events arrive.
#[derive(Debug, Clone)]
pub struct ButtonMapping {
    /// Linux `BTN_*` evdev key code.
    pub btn_code: u16,
    /// If set, the button also drives this `ABS_*` analog axis (e.g. LT/RT
    /// triggers in the standard layout drive `ABS_Z`/`ABS_RZ`).
    pub trigger_abs_code: Option<u16>,
}

/// Evdev capability declaration for a single gamepad axis.
///
/// Received once at connect time as part of [`GamepadEvent::Connected`].
/// Stored by the device to look up evdev codes when raw-index poll events arrive.
#[derive(Debug, Clone)]
pub struct AxisMapping {
    /// Linux `ABS_*` evdev absolute-axis code.
    pub abs_code: u16,
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
        /// The `Gamepad.mapping` string from the browser (`"standard"` or `""`).
        mapping: String,
        /// Per-button capability declarations, indexed by browser button index.
        ///
        /// `None` means the controller layout is unknown; no virtual device will
        /// be created until a mapping is provided (future user-defined mapping UI).
        buttons: Option<Vec<ButtonMapping>>,
        /// Per-axis capability declarations, indexed by browser axis index.
        ///
        /// `None` when `buttons` is `None`.
        axes: Option<Vec<AxisMapping>>,
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
        /// Raw browser button index.  Looked up against the stored capability
        /// declaration to obtain the evdev `BTN_*` code.
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
        /// Raw browser axis index.  Looked up against the stored capability
        /// declaration to obtain the evdev `ABS_*` code.
        axis: u8,
        /// Normalised value in the range −1.0 to 1.0.
        value: f32,
    },
}

/// A haptic (rumble) command produced when a Linux application plays a
/// force-feedback effect on one of the virtual gamepad devices.
///
/// Magnitudes are in the range `0.0` (no vibration) to `1.0` (maximum).
#[derive(Debug, Clone)]
pub struct HapticCommand {
    /// Low-frequency (strong) motor magnitude, 0.0–1.0.
    pub strong_magnitude: f32,
    /// High-frequency (weak) motor magnitude, 0.0–1.0.
    pub weak_magnitude: f32,
    /// Requested effect duration in milliseconds.
    pub duration_ms: u32,
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
            let connected: Vec<u8> = self.devices.keys().copied().collect();
            warn!("gamepad: {err} (connected indices: {connected:?})");
        }
    }

    fn try_handle_event(&mut self, event: GamepadEvent) -> Result<(), GamepadError> {
        match event {
            GamepadEvent::Connected { index, name, mapping, buttons, axes } => {
                debug!("GamepadManager: Connected index={index} name={name:?} mapping={mapping:?}");
                self.connect(index, &name, &mapping, buttons, axes)
            }
            GamepadEvent::Disconnected { index } => {
                debug!("GamepadManager: Disconnected index={index}");
                self.disconnect(index)
            }
            GamepadEvent::Button { index, button, value, pressed } => {
                trace!("GamepadManager: Button index={index} button={button} value={value} pressed={pressed}");
                self.button(index, button, value, pressed)
            }
            GamepadEvent::Axis { index, axis, value } => {
                trace!("GamepadManager: Axis index={index} axis={axis} value={value}");
                self.axis(index, axis, value)
            }
        }
    }

    fn connect(
        &mut self,
        index: u8,
        name: &str,
        mapping: &str,
        buttons: Option<Vec<ButtonMapping>>,
        axes: Option<Vec<AxisMapping>>,
    ) -> Result<(), GamepadError> {
        if index >= MAX_GAMEPADS {
            return Err(GamepadError::IndexOutOfRange(index));
        }
        let (Some(buttons), Some(axes)) = (buttons, axes) else {
            warn!("gamepad {index}: no mapping declaration (non-standard controller); skipping device creation");
            return Ok(());
        };
        if self.devices.contains_key(&index) {
            // Browser may re-send connected; treat as reconnect.
            warn!("gamepad {index}: already connected, replacing device");
            self.devices.remove(&index);
        }
        let device_name = {
            // uinput enforces UINPUT_MAX_NAME_SIZE = 80: name.len() + 1 < 80,
            // so the name must be at most 78 bytes.  Truncate on a UTF-8
            // boundary so the evdev crate never panics on long controller names.
            const MAX: usize = 78;
            let full = format!("Lumen Gamepad {index} ({name})");
            if full.len() <= MAX {
                full
            } else {
                // Find the last valid char boundary at or before MAX.
                let boundary = (0..=MAX).rev().find(|&i| full.is_char_boundary(i)).unwrap_or(0);
                full[..boundary].to_string()
            }
        };
        let n_buttons = buttons.len();
        let n_axes = axes.len();
        let device = GamepadDevice::new(&device_name, buttons, axes)?;
        info!("gamepad {index}: virtual device created (mapping={mapping:?}, {n_buttons} buttons, {n_axes} axes)");
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

    /// Poll all connected devices for pending force-feedback play events.
    ///
    /// Returns `(gamepad_index, command)` pairs for each `FF_RUMBLE` effect
    /// that a Linux application has triggered since the last call.  Should be
    /// called periodically (e.g. every ~16 ms) from the gamepad manager task.
    pub fn poll_haptic_commands(&mut self) -> Vec<(u8, HapticCommand)> {
        let mut out = Vec::new();
        for (&index, device) in &mut self.devices {
            for cmd in device.poll_ff_events() {
                out.push((index, cmd));
            }
        }
        out
    }
}

impl Default for GamepadManager {
    fn default() -> Self {
        Self::new()
    }
}
