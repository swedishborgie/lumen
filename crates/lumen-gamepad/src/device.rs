//! Per-gamepad virtual device backed by Linux uinput.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::os::fd::AsRawFd as _;

use evdev::{
    uinput::VirtualDevice,
    AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, EventSummary, FFEffectCode, FFStatusCode,
    InputEvent as EvdevEvent, KeyCode, UInputCode, UinputAbsSetup,
};

use crate::{AxisMapping, ButtonMapping, GamepadError, HapticCommand};

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
    /// Capability declaration, indexed by raw browser button index.
    /// `None` entries represent buttons that were skipped during mapping.
    buttons: Vec<Option<ButtonMapping>>,
    /// Capability declaration, indexed by raw browser axis index.
    /// `None` entries represent axes that were skipped during mapping.
    axes: Vec<Option<AxisMapping>>,
    /// Free effect-ID pool (0..FF_EFFECTS_MAX).
    free_ids: BTreeSet<u16>,
    /// Uploaded effects: effect_id → (strong_raw, weak_raw, duration_ms).
    effects: HashMap<u16, StoredEffect>,
}

impl GamepadDevice {
    /// Create a new virtual gamepad device from a capability declaration.
    ///
    /// `buttons` and `axes` are indexed by browser button/axis index and carry
    /// the evdev codes used to register device capabilities and to look up
    /// codes at event time.  Capabilities are built dynamically from the
    /// declaration, so no hardcoded layout knowledge is needed here.
    pub(crate) fn new(
        name: &str,
        buttons: Vec<Option<ButtonMapping>>,
        axes: Vec<Option<AxisMapping>>,
    ) -> Result<Self, GamepadError> {
        // ── Build key set ─────────────────────────────────────────────────────
        let mut keys = AttributeSet::<KeyCode>::new();
        for btn in buttons.iter().flatten() {
            keys.insert(KeyCode(btn.btn_code));
        }

        // ── Build abs-axis set ────────────────────────────────────────────────
        // Collect all abs codes, deduplicating trigger_abs_code entries that
        // might appear under both buttons (trigger) and axes (analog stick).
        let mut abs_codes: Vec<u16> = Vec::new();
        let mut seen: HashSet<u16> = HashSet::new();

        // Axes from the axis declaration come first.
        for ax in axes.iter().flatten() {
            if seen.insert(ax.abs_code) {
                abs_codes.push(ax.abs_code);
            }
        }
        // Trigger analog axes implied by button trigger_abs_code.
        for btn in buttons.iter().flatten() {
            if let Some(code) = btn.trigger_abs_code {
                if seen.insert(code) {
                    abs_codes.push(code);
                }
            }
        }

        // ── Build FF set ──────────────────────────────────────────────────────
        let mut ff_set = AttributeSet::<FFEffectCode>::new();
        ff_set.insert(FFEffectCode::FF_RUMBLE);

        // ── Assemble virtual device ───────────────────────────────────────────
        let mut builder = VirtualDevice::builder()
            .map_err(GamepadError::UinputOpen)?
            .name(name)
            .with_keys(&keys)
            .map_err(GamepadError::DeviceSetup)?;

        let n_abs = abs_codes.len();
        for &code in &abs_codes {
            let setup = abs_setup_for_code(code);
            builder = builder
                .with_absolute_axis(&setup)
                .map_err(GamepadError::DeviceSetup)?;
        }

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
            "GamepadDevice::new: created {name:?} with {} keys, {} abs axes, FF_RUMBLE",
            keys.iter().count(),
            n_abs,
        );
        Ok(Self { device, buttons, axes, free_ids, effects: HashMap::new() })
    }

    /// Emit a button event.
    ///
    /// `button_idx` is the raw browser button index.  The evdev `BTN_*` code
    /// and optional trigger `ABS_*` code are looked up from the stored
    /// capability declaration.
    pub(crate) fn send_button(
        &mut self,
        button_idx: u8,
        pressed: bool,
        value: f32,
    ) -> Result<(), GamepadError> {
        let Some(Some(mapping)) = self.buttons.get(button_idx as usize) else {
            // Button not mapped (None slot from wizard skip or out of range) — skip silently.
            return Ok(());
        };
        let mapping = mapping.clone();

        let mut events: Vec<EvdevEvent> = Vec::with_capacity(2);

        events.push(EvdevEvent::new(evdev::EventType::KEY.0, mapping.btn_code, i32::from(pressed)));

        if let Some(abs_code) = mapping.trigger_abs_code {
            // Trigger button value is 0..1; scale to 0..255.
            #[allow(clippy::cast_possible_truncation)]
            let abs_val = (value.clamp(0.0, 1.0) * 255.0).round() as i32;
            events.push(EvdevEvent::new(evdev::EventType::ABSOLUTE.0, abs_code, abs_val));
        }

        self.device.emit(&events).map_err(GamepadError::Emit)
    }

    /// Emit an axis movement event.
    ///
    /// `axis_idx` is the raw browser axis index.  The evdev `ABS_*` code is
    /// looked up from the stored capability declaration.  The float `value` is
    /// scaled based on the axis type:
    ///
    /// - Trigger axes (`ABS_Z`=2, `ABS_RZ`=5): browser `−1.0..1.0` → evdev `0..255`
    /// - Hat/D-pad axes (`ABS_HAT0X`=16, `ABS_HAT0Y`=17): rounded to `−1..1`
    /// - All other axes: `−1.0..1.0` → `−32 767..32 767`
    pub(crate) fn send_axis(&mut self, axis_idx: u8, value: f32) -> Result<(), GamepadError> {
        let Some(Some(ax)) = self.axes.get(axis_idx as usize) else {
            // Axis not mapped (None slot from wizard skip or out of range) — skip silently.
            return Ok(());
        };
        let abs_code = ax.abs_code;

        #[allow(clippy::cast_possible_truncation)]
        let scaled: i32 = match abs_code {
            2 | 5   => ((value.clamp(-1.0, 1.0) + 1.0) * 0.5 * 255.0).round() as i32,
            16 | 17 => value.round() as i32,
            _       => (value.clamp(-1.0, 1.0) * 32_767.0).round() as i32,
        };

        let event = EvdevEvent::new(evdev::EventType::ABSOLUTE.0, abs_code, scaled);
        self.device.emit(&[event]).map_err(GamepadError::Emit)
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

// ── Axis capability helpers ───────────────────────────────────────────────────

/// Return the appropriate `UinputAbsSetup` for a given `ABS_*` code.
///
/// - ABS_Z (2) / ABS_RZ (5): trigger axes, range 0–255
/// - ABS_HAT0X (16) / ABS_HAT0Y (17): D-pad hat axes, range ±1
/// - All others: stick axes, range ±32 767
fn abs_setup_for_code(code: u16) -> UinputAbsSetup {
    let abs = Abs(code);
    match code {
        2 | 5   => UinputAbsSetup::new(abs, AbsInfo::new(0, 0, 255, 0, 0, 0)),
        16 | 17 => UinputAbsSetup::new(abs, AbsInfo::new(0, -1, 1, 0, 0, 0)),
        _       => UinputAbsSetup::new(abs, AbsInfo::new(0, -32_767, 32_767, 16, 128, 0)),
    }
}


// (end of file)
