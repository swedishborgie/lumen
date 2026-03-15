//! Per-gamepad virtual device backed by Linux uinput.

use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AbsInfo, AttributeSet, InputEvent as EvdevEvent,
    Key, UinputAbsSetup,
};

use crate::{
    mapping::{AXIS_MAP, AXIS_SCALE, BUTTON_MAP, TRIGGER_AXIS_MAP, TRIGGER_SCALE},
    GamepadError,
};

/// A single virtual gamepad device created via uinput.
pub(crate) struct GamepadDevice {
    device: VirtualDevice,
}

impl GamepadDevice {
    /// Create a new virtual gamepad device with `num_buttons` and `num_axes`
    /// capabilities from the standard layout mapping tables.
    pub(crate) fn new(name: &str, num_buttons: u8, num_axes: u8) -> Result<Self, GamepadError> {
        let mut keys = AttributeSet::<Key>::new();
        for &(idx, key) in BUTTON_MAP {
            if (idx as u8) < num_buttons {
                keys.insert(key);
            }
        }

        let mut abs_axes: Vec<UinputAbsSetup> = Vec::new();

        // Standard stick axes.
        for &(idx, axis) in AXIS_MAP {
            if (idx as u8) < num_axes {
                abs_axes.push(UinputAbsSetup::new(
                    axis,
                    AbsInfo::new(0, -32_767, 32_767, 16, 128, 0),
                ));
            }
        }

        // Trigger absolute axes (ABS_Z / ABS_RZ).  Include if the corresponding
        // button (6 or 7) is within the advertised button count.
        for &(btn_idx, axis) in TRIGGER_AXIS_MAP {
            if (btn_idx as u8) < num_buttons {
                abs_axes.push(UinputAbsSetup::new(
                    axis,
                    AbsInfo::new(0, 0, 255, 0, 0, 0),
                ));
            }
        }

        let mut builder = VirtualDeviceBuilder::new()
            .map_err(GamepadError::UinputOpen)?
            .name(name)
            .with_keys(&keys)
            .map_err(GamepadError::DeviceSetup)?;

        for setup in abs_axes {
            builder = builder
                .with_absolute_axis(&setup)
                .map_err(GamepadError::DeviceSetup)?;
        }

        let device = builder.build().map_err(GamepadError::DeviceSetup)?;
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
                evdev::EventType::KEY,
                key.code(),
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
            events.push(EvdevEvent::new(evdev::EventType::ABSOLUTE, axis.0, abs_val));
        }

        if !events.is_empty() {
            self.device.emit(&events).map_err(GamepadError::Emit)?;
        }
        Ok(())
    }

    /// Emit an axis movement event.
    pub(crate) fn send_axis(&mut self, axis_idx: u8, value: f32) -> Result<(), GamepadError> {
        if let Some(&(_, axis)) = AXIS_MAP.iter().find(|&&(i, _)| i == axis_idx as usize) {
            #[allow(clippy::cast_possible_truncation)]
            let scaled = (value.clamp(-1.0, 1.0) * AXIS_SCALE).round() as i32;
            let event = EvdevEvent::new(evdev::EventType::ABSOLUTE, axis.0, scaled);
            self.device.emit(&[event]).map_err(GamepadError::Emit)?;
        }
        Ok(())
    }
}
