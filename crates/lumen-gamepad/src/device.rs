//! Per-gamepad virtual device backed by Linux uinput.

use evdev::{
    uinput::VirtualDevice,
    AbsInfo, AttributeSet, InputEvent as EvdevEvent,
    KeyCode, UinputAbsSetup,
};

use crate::{
    mapping::{
        AXIS_MAP, AXIS_SCALE, BUTTON_MAP,
        TRIGGER_AXIS_MAP, TRIGGER_SCALE,
        TRIGGER_FROM_AXIS_MAP, TRIGGER_FROM_AXIS_SCALE,
        HAT_AXIS_MAP,
    },
    GamepadError,
};

use tracing::trace;

/// A single virtual gamepad device created via uinput.
pub(crate) struct GamepadDevice {
    device: VirtualDevice,
}

impl GamepadDevice {
    /// Create a new virtual gamepad device with `num_buttons` and `num_axes`
    /// capabilities from the standard layout mapping tables.
    pub(crate) fn new(name: &str, num_buttons: u8, num_axes: u8) -> Result<Self, GamepadError> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for &(idx, key) in BUTTON_MAP {
            if (idx as u8) < num_buttons {
                keys.insert(key);
            }
        }

        let mut abs_axes: Vec<UinputAbsSetup> = Vec::new();

        // Stick axes (±32 767).
        for &(idx, axis) in AXIS_MAP {
            if (idx as u8) < num_axes {
                abs_axes.push(UinputAbsSetup::new(
                    axis,
                    AbsInfo::new(0, -32_767, 32_767, 16, 128, 0),
                ));
            }
        }

        // Trigger axes — prefer axis-based triggers (raw evdev layout, e.g.
        // 8BitDo) when axis indices 2 or 5 are present; otherwise fall back to
        // button-based triggers (W3C standard layout).
        let triggers_from_axes = num_axes > 2;
        if triggers_from_axes {
            for &(idx, axis) in TRIGGER_FROM_AXIS_MAP {
                if (idx as u8) < num_axes {
                    abs_axes.push(UinputAbsSetup::new(
                        axis,
                        AbsInfo::new(0, 0, 255, 0, 0, 0),
                    ));
                }
            }
        } else {
            for &(btn_idx, axis) in TRIGGER_AXIS_MAP {
                if (btn_idx as u8) < num_buttons {
                    abs_axes.push(UinputAbsSetup::new(
                        axis,
                        AbsInfo::new(0, 0, 255, 0, 0, 0),
                    ));
                }
            }
        }

        // Hat / D-pad axes (−1, 0, 1).
        for &(idx, axis) in HAT_AXIS_MAP {
            if (idx as u8) < num_axes {
                abs_axes.push(UinputAbsSetup::new(
                    axis,
                    AbsInfo::new(0, -1, 1, 0, 0, 0),
                ));
            }
        }

        let mut builder = VirtualDevice::builder()
            .map_err(GamepadError::UinputOpen)?
            .name(name)
            .with_keys(&keys)
            .map_err(GamepadError::DeviceSetup)?;

        let abs_axes_len = abs_axes.len();
        for setup in abs_axes {
            builder = builder
                .with_absolute_axis(&setup)
                .map_err(GamepadError::DeviceSetup)?;
        }

        let device = builder.build().map_err(GamepadError::DeviceSetup)?;
        trace!(
            "GamepadDevice::new: created {name:?} with {} keys, {} abs axes",
            keys.iter().count(),
            abs_axes_len,
        );
        Ok(Self { device })
    }

    /// Emit a button press or release event.
    pub(crate) fn send_button(
        &mut self,
        button_idx: u8,
        pressed: bool,
        value: f32,
    ) -> Result<(), GamepadError> {
        let mut events: Vec<EvdevEvent> = Vec::with_capacity(3);

        // Digital button event.
        if let Some(&(_, key)) = BUTTON_MAP.iter().find(|&&(i, _)| i == button_idx as usize) {
            events.push(EvdevEvent::new(
                evdev::EventType::KEY.0,
                key.0,
                i32::from(pressed),
            ));
        }

        // Trigger buttons also drive ABS_Z / ABS_RZ.
        if let Some(&(_, axis)) = TRIGGER_AXIS_MAP
            .iter()
            .find(|&&(i, _)| i == button_idx as usize)
        {
            #[allow(clippy::cast_possible_truncation)]
            let abs_val = (value.clamp(0.0, 1.0) * TRIGGER_SCALE).round() as i32;
            events.push(EvdevEvent::new(evdev::EventType::ABSOLUTE.0, axis.0, abs_val));
        }

        if !events.is_empty() {
            self.device.emit(&events).map_err(GamepadError::Emit)?;
        }
        Ok(())
    }

    /// Emit an axis movement event.
    pub(crate) fn send_axis(&mut self, axis_idx: u8, value: f32) -> Result<(), GamepadError> {
        // Stick axes — scale ±1.0 to ±32 767.
        if let Some(&(_, axis)) = AXIS_MAP.iter().find(|&&(i, _)| i == axis_idx as usize) {
            #[allow(clippy::cast_possible_truncation)]
            let scaled = (value.clamp(-1.0, 1.0) * AXIS_SCALE).round() as i32;
            let event = EvdevEvent::new(evdev::EventType::ABSOLUTE.0, axis.0, scaled);
            return self.device.emit(&[event]).map_err(GamepadError::Emit);
        }

        // Trigger axes — browser range −1..1 → evdev 0..255.
        if let Some(&(_, axis)) = TRIGGER_FROM_AXIS_MAP.iter().find(|&&(i, _)| i == axis_idx as usize) {
            #[allow(clippy::cast_possible_truncation)]
            let scaled = ((value.clamp(-1.0, 1.0) + 1.0) / 2.0 * TRIGGER_FROM_AXIS_SCALE).round() as i32;
            let event = EvdevEvent::new(evdev::EventType::ABSOLUTE.0, axis.0, scaled);
            return self.device.emit(&[event]).map_err(GamepadError::Emit);
        }

        // Hat / D-pad axes — values are −1, 0, or 1.
        if let Some(&(_, axis)) = HAT_AXIS_MAP.iter().find(|&&(i, _)| i == axis_idx as usize) {
            let val = value.round() as i32;
            let event = EvdevEvent::new(evdev::EventType::ABSOLUTE.0, axis.0, val);
            return self.device.emit(&[event]).map_err(GamepadError::Emit);
        }

        Ok(())
    }
}
