//! Per-gamepad virtual device backed by Linux uinput.

use std::collections::{BTreeSet, HashMap};
use std::os::fd::AsRawFd as _;

use evdev::{
    uinput::VirtualDevice,
    AbsInfo, AttributeSet, EventSummary, FFEffectCode, FFStatusCode,
    InputEvent as EvdevEvent, KeyCode, UInputCode, UinputAbsSetup,
};

use crate::{
    mapping::{
        AXIS_MAP, AXIS_SCALE, BUTTON_MAP, RAW_BUTTON_MAP,
        TRIGGER_AXIS_MAP, TRIGGER_SCALE,
        TRIGGER_FROM_AXIS_MAP, TRIGGER_FROM_AXIS_SCALE,
        HAT_AXIS_MAP,
    },
    GamepadError, HapticCommand,
};

use tracing::trace;

/// Maximum number of force-feedback effect slots per virtual device.
const FF_EFFECTS_MAX: u16 = 16;

/// Raw evdev magnitude range (u16 0–65535) → normalized float.
const FF_MAG_SCALE: f32 = 65535.0;

/// A stored FF_RUMBLE effect: (strong_magnitude_raw, weak_magnitude_raw, duration_ms).
type StoredEffect = (u16, u16, u32);

/// A single virtual gamepad device created via uinput.
pub(crate) struct GamepadDevice {
    device: VirtualDevice,
    /// True when the controller uses raw evdev layout (triggers as analog axes,
    /// no digital trigger buttons at indices 6/7).
    raw_layout: bool,
    /// Free effect-ID pool (0..FF_EFFECTS_MAX).
    free_ids: BTreeSet<u16>,
    /// Uploaded effects: effect_id → (strong_raw, weak_raw, duration_ms).
    effects: HashMap<u16, StoredEffect>,
}

impl GamepadDevice {
    /// Create a new virtual gamepad device with `num_buttons` and `num_axes`
    /// capabilities from the standard layout mapping tables.
    pub(crate) fn new(name: &str, num_buttons: u8, num_axes: u8) -> Result<Self, GamepadError> {
        // Detect layout: controllers with more than 2 axes expose triggers as
        // analog axes (raw evdev layout); others use the W3C standard layout
        // where triggers appear as digital buttons at indices 6/7.
        let raw_layout = num_axes > 2;
        let button_map = if raw_layout { RAW_BUTTON_MAP } else { BUTTON_MAP };

        let mut keys = AttributeSet::<KeyCode>::new();
        for &(idx, key) in button_map {
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
        if raw_layout {
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

        let mut ff_set = AttributeSet::<FFEffectCode>::new();
        ff_set.insert(FFEffectCode::FF_RUMBLE);
        builder = builder
            .with_ff(&ff_set)
            .map_err(GamepadError::DeviceSetup)?
            .with_ff_effects_max(FF_EFFECTS_MAX as u32);

        let device = builder.build().map_err(GamepadError::DeviceSetup)?;

        // Set the uinput fd to non-blocking so poll_ff_events() never stalls.
        let fd = device.as_raw_fd();
        // SAFETY: fd is valid for the lifetime of `device`.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL, 0);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let free_ids = (0..FF_EFFECTS_MAX).collect();
        trace!(
            "GamepadDevice::new: created {name:?} with {} keys, {} abs axes, FF_RUMBLE enabled",
            keys.iter().count(),
            abs_axes_len,
        );
        Ok(Self { device, raw_layout, free_ids, effects: HashMap::new() })
    }

    /// Emit a button press or release event.
    pub(crate) fn send_button(
        &mut self,
        button_idx: u8,
        pressed: bool,
        value: f32,
    ) -> Result<(), GamepadError> {
        let mut events: Vec<EvdevEvent> = Vec::with_capacity(3);

        // Select the correct button map for this controller's layout.
        let button_map = if self.raw_layout { RAW_BUTTON_MAP } else { BUTTON_MAP };

        // Digital button event.
        if let Some(&(_, key)) = button_map.iter().find(|&&(i, _)| i == button_idx as usize) {
            events.push(EvdevEvent::new(
                evdev::EventType::KEY.0,
                key.0,
                i32::from(pressed),
            ));
        }

        // Trigger buttons also drive ABS_Z / ABS_RZ — standard layout only.
        // Raw-layout controllers send triggers as axis events, not button events.
        if !self.raw_layout {
            if let Some(&(_, axis)) = TRIGGER_AXIS_MAP
                .iter()
                .find(|&&(i, _)| i == button_idx as usize)
            {
                #[allow(clippy::cast_possible_truncation)]
                let abs_val = (value.clamp(0.0, 1.0) * TRIGGER_SCALE).round() as i32;
                events.push(EvdevEvent::new(evdev::EventType::ABSOLUTE.0, axis.0, abs_val));
            }
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

    /// Poll pending force-feedback events from the uinput fd (non-blocking).
    ///
    /// Returns a list of `HapticCommand`s for any `FF_RUMBLE` effects that
    /// an application has started playing since the last call.  Returns an
    /// empty `Vec` when no events are pending.
    pub(crate) fn poll_ff_events(&mut self) -> Vec<HapticCommand> {
        let events = match self.device.fetch_events() {
            Ok(iter) => iter.collect::<Vec<_>>(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return vec![],
            Err(e) => {
                tracing::warn!("gamepad: fetch_events error: {e}");
                return vec![];
            }
        };

        const PLAYING: i32 = FFStatusCode::FF_STATUS_PLAYING.0 as i32;

        let mut commands = Vec::new();
        for event in events {
            match event.destructure() {
                EventSummary::UInput(ev, UInputCode::UI_FF_UPLOAD, ..) => {
                    match self.device.process_ff_upload(ev) {
                        Ok(mut upload) => {
                            if let Some(id) = self.free_ids.iter().next().copied() {
                                self.free_ids.remove(&id);
                                let effect = upload.effect();
                                let (strong, weak, duration) =
                                    if let evdev::FFEffectKind::Rumble { strong_magnitude, weak_magnitude } = effect.kind {
                                        (strong_magnitude, weak_magnitude, effect.replay.length)
                                    } else {
                                        (0, 0, 0)
                                    };
                                self.effects.insert(id, (strong, weak, duration as u32));
                                upload.set_effect_id(id as i16);
                                upload.set_retval(0);
                                trace!("gamepad FF: upload effect id={id} strong={strong} weak={weak} duration={duration}ms");
                            } else {
                                upload.set_retval(-1);
                                tracing::warn!("gamepad FF: no free effect slots");
                            }
                        }
                        Err(e) => tracing::warn!("gamepad FF: process_ff_upload error: {e}"),
                    }
                }
                EventSummary::UInput(ev, UInputCode::UI_FF_ERASE, ..) => {
                    match self.device.process_ff_erase(ev) {
                        Ok(erase) => {
                            let id = erase.effect_id() as u16;
                            self.effects.remove(&id);
                            self.free_ids.insert(id);
                            trace!("gamepad FF: erase effect id={id}");
                        }
                        Err(e) => tracing::warn!("gamepad FF: process_ff_erase error: {e}"),
                    }
                }
                EventSummary::ForceFeedback(_, effect_id, PLAYING) => {
                    if let Some(&(strong, weak, duration_ms)) = self.effects.get(&effect_id.0) {
                        commands.push(HapticCommand {
                            strong_magnitude: strong as f32 / FF_MAG_SCALE,
                            weak_magnitude: weak as f32 / FF_MAG_SCALE,
                            duration_ms,
                        });
                        trace!("gamepad FF: play effect id={} strong={strong} weak={weak}", effect_id.0);
                    }
                }
                _ => {}
            }
        }
        commands
    }
}
