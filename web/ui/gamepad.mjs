/**
 * gamepad.mjs — GamepadController for Lumen.
 *
 * Manages browser gamepad connections and a requestAnimationFrame polling
 * loop that sends button/axis delta events to the compositor.
 *
 * For controllers with `Gamepad.mapping === "standard"`, a capability
 * declaration (button and axis evdev codes) is computed once at connect time
 * and sent as part of the `gamepad_connected` message.  The Rust side uses this
 * declaration to build the virtual uinput device and to look up evdev codes when
 * poll events arrive.
 *
 * For non-standard controllers the declaration is `null`, which tells the
 * compositor that no virtual device should be created yet.  This leaves the
 * architecture open for a future user-defined mapping UI.
 */

// ── Linux evdev button codes (BTN_*) ─────────────────────────────────────────
const BTN_SOUTH      = 0x130; // A / Cross
const BTN_EAST       = 0x131; // B / Circle
const BTN_NORTH      = 0x133; // Y / Triangle
const BTN_WEST       = 0x134; // X / Square
const BTN_TL         = 0x136; // LB / L1
const BTN_TR         = 0x137; // RB / R1
const BTN_TL2        = 0x138; // LT / L2
const BTN_TR2        = 0x139; // RT / R2
const BTN_SELECT     = 0x13a; // Back / Select
const BTN_START      = 0x13b; // Start
const BTN_MODE       = 0x13c; // Guide / Home
const BTN_THUMBL     = 0x13d; // L3 (left stick click)
const BTN_THUMBR     = 0x13e; // R3 (right stick click)
const BTN_DPAD_UP    = 0x220;
const BTN_DPAD_DOWN  = 0x221;
const BTN_DPAD_LEFT  = 0x222;
const BTN_DPAD_RIGHT = 0x223;

// ── Linux evdev absolute axis codes (ABS_*) ───────────────────────────────────
const ABS_X  = 0x00; // Left stick X
const ABS_Y  = 0x01; // Left stick Y
const ABS_Z  = 0x02; // Left trigger analog (0–255 in evdev)
const ABS_RX = 0x03; // Right stick X
const ABS_RY = 0x04; // Right stick Y
const ABS_RZ = 0x05; // Right trigger analog (0–255 in evdev)

// ── W3C Standard Gamepad Layout mapping tables ────────────────────────────────
// Reference: https://w3c.github.io/gamepad/#dfn-standard-gamepad-layout

/**
 * Browser button index → { btnCode, triggerAbsCode? }.
 *
 * `triggerAbsCode` is set for LT/RT: those buttons also drive an analog axis
 * on the virtual uinput device.
 */
const STANDARD_BTN_MAP = [
  { btnCode: BTN_SOUTH },                         // 0  A / Cross
  { btnCode: BTN_EAST },                          // 1  B / Circle
  { btnCode: BTN_WEST },                          // 2  X / Square
  { btnCode: BTN_NORTH },                         // 3  Y / Triangle
  { btnCode: BTN_TL },                            // 4  LB
  { btnCode: BTN_TR },                            // 5  RB
  { btnCode: BTN_TL2, triggerAbsCode: ABS_Z  },  // 6  LT (digital + analog)
  { btnCode: BTN_TR2, triggerAbsCode: ABS_RZ },  // 7  RT (digital + analog)
  { btnCode: BTN_SELECT },                        // 8  Back / Select
  { btnCode: BTN_START },                         // 9  Start
  { btnCode: BTN_THUMBL },                        // 10 L3
  { btnCode: BTN_THUMBR },                        // 11 R3
  { btnCode: BTN_DPAD_UP },                       // 12 D-pad Up
  { btnCode: BTN_DPAD_DOWN },                     // 13 D-pad Down
  { btnCode: BTN_DPAD_LEFT },                     // 14 D-pad Left
  { btnCode: BTN_DPAD_RIGHT },                    // 15 D-pad Right
  { btnCode: BTN_MODE },                          // 16 Guide / Home
];

/** Browser axis index → evdev ABS_* code. */
const STANDARD_AXIS_MAP = [
  ABS_X,  // 0 Left stick X
  ABS_Y,  // 1 Left stick Y
  ABS_RX, // 2 Right stick X
  ABS_RY, // 3 Right stick Y
];

/**
 * Build a capability declaration for the given gamepad.
 *
 * Returns `{ buttons, axes }` for known layouts, or `{ buttons: null, axes: null }`
 * for controllers the browser has not normalized to a standard layout.
 *
 * The returned arrays are indexed by browser button/axis index and contain the
 * Linux evdev codes that Rust will use to register and drive the uinput device.
 *
 * @param {Gamepad} gp
 * @returns {{ buttons: Array<{btn_code:number, trigger_abs_code:number|null}>|null,
 *             axes:    Array<{abs_code:number}>|null }}
 */
function buildMappingDeclaration(gp) {
  if (gp.mapping !== 'standard') {
    // Non-standard controller — no mapping known yet.
    return { buttons: null, axes: null };
  }

  const numButtons = Math.min(gp.buttons.length, STANDARD_BTN_MAP.length);
  const buttons = [];
  for (let i = 0; i < numButtons; i++) {
    const m = STANDARD_BTN_MAP[i];
    buttons.push({
      btn_code:         m.btnCode,
      trigger_abs_code: m.triggerAbsCode ?? null,
    });
  }

  const numAxes = Math.min(gp.axes.length, STANDARD_AXIS_MAP.length);
  const axes = [];
  for (let i = 0; i < numAxes; i++) {
    axes.push({ abs_code: STANDARD_AXIS_MAP[i] });
  }

  return { buttons, axes };
}

export class GamepadController {
  #client;
  #rafHandle    = null;    // requestAnimationFrame handle for the poll loop
  #state        = new Map(); // gamepad index → { buttons: Float32Array, axes: Float32Array }
  #onConnect    = null;    // optional (index, name) => void
  #onDisconnect = null;    // optional (index) => void

  /**
   * @param {import('../lumen-client.mjs').LumenClient} client
   * @param {{ onConnect?: (index: number, name: string) => void,
   *           onDisconnect?: (index: number) => void }} [callbacks]
   */
  constructor(client, { onConnect = null, onDisconnect = null } = {}) {
    this.#client      = client;
    this.#onConnect    = onConnect;
    this.#onDisconnect = onDisconnect;
  }

  /** Attach gamepadconnected / gamepaddisconnected event listeners. */
  bind() {
    window.addEventListener('gamepadconnected',    (e) => this.#handleConnected(e));
    window.addEventListener('gamepaddisconnected', (e) => this.#handleDisconnected(e));
  }

  /** Stop the RAF polling loop (called when the session goes idle). */
  stop() {
    if (this.#rafHandle !== null) {
      cancelAnimationFrame(this.#rafHandle);
      this.#rafHandle = null;
    }
  }

  /**
   * Re-send gamepad_connected for all currently tracked gamepads and restart
   * the poll loop if needed.  Call this whenever the data channel (re)opens so
   * the backend is in sync even when gamepadconnected fired before the channel
   * was ready, or after a session reconnect.
   */
  resync() {
    if (this.#state.size === 0) return;
    const gamepads = navigator.getGamepads();
    for (const gp of gamepads) {
      if (!gp || !this.#state.has(gp.index)) continue;
      const { buttons, axes } = buildMappingDeclaration(gp);
      this.#client.sendInput({
        type:    'gamepad_connected',
        index:   gp.index,
        name:    gp.id,
        mapping: gp.mapping,
        buttons,
        axes,
      });
    }
    if (this.#rafHandle === null) {
      this.#startPoll();
    }
  }

  // ── private helpers ──────────────────────────────────────────────────────────

  /**
   * Apply a haptic (rumble) command received from the compositor.
   *
   * Called by the UI layer when a `{ type: "haptic", ... }` data channel
   * message arrives.  Supports both the W3C `vibrationActuator` API (Chrome)
   * and the older `hapticActuators` array API (Firefox).  Silently no-ops
   * when the browser or physical controller does not support rumble.
   *
   * @param {{ index: number, strong_magnitude: number, weak_magnitude: number, duration_ms: number }} msg
   */
  handleHaptic({ index, strong_magnitude, weak_magnitude, duration_ms }) {
    const gp = navigator.getGamepads()[index];
    if (!gp) return;

    // W3C GamepadHapticActuator (Chrome 68+, Edge 79+).
    if (gp.vibrationActuator) {
      gp.vibrationActuator.playEffect('dual-rumble', {
        duration:        duration_ms,
        strongMagnitude: strong_magnitude,
        weakMagnitude:   weak_magnitude,
      }).catch(() => {});
      return;
    }

    // Legacy hapticActuators array (Firefox, some older browsers).
    // pulse() takes a single combined magnitude and a duration in milliseconds.
    const actuator = gp.hapticActuators?.[0];
    if (actuator) {
      const magnitude = Math.max(strong_magnitude, weak_magnitude);
      actuator.pulse(magnitude, duration_ms).catch(() => {});
    }
  }

  #handleConnected(e) {
    const gp    = e.gamepad;
    const index = gp.index;
    this.#state.set(index, {
      buttons: new Float32Array(gp.buttons.length),
      axes:    new Float32Array(gp.axes.length),
    });
    const { buttons, axes } = buildMappingDeclaration(gp);
    this.#client.sendInput({
      type:    'gamepad_connected',
      index,
      name:    gp.id,
      mapping: gp.mapping,
      buttons,
      axes,
    });
    this.#onConnect?.(index, gp.id);
    if (this.#rafHandle === null) {
      this.#startPoll();
    }
  }

  #handleDisconnected(e) {
    const index = e.gamepad.index;
    this.#state.delete(index);
    this.#client.sendInput({ type: 'gamepad_disconnected', index });
    this.#onDisconnect?.(index);
    if (this.#state.size === 0) {
      this.stop();
    }
  }

  #startPoll() {
    const poll = () => {
      this.#poll();
      this.#rafHandle = requestAnimationFrame(poll);
    };
    this.#rafHandle = requestAnimationFrame(poll);
  }

  /**
   * Read the current gamepad state, diff against the previous snapshot, and
   * send events only for changed buttons/axes.  Raw browser button/axis indices
   * are sent as-is; the Rust side looks up evdev codes from the capability
   * declaration received at connect time.
   */
  #poll() {
    const gamepads = navigator.getGamepads();
    for (const gp of gamepads) {
      if (!gp) continue;
      const prev = this.#state.get(gp.index);
      if (!prev) continue;

      for (let i = 0; i < gp.buttons.length; i++) {
        const btn    = gp.buttons[i];
        const curVal = btn.value;
        if (curVal !== (prev.buttons[i] ?? 0)) {
          prev.buttons[i] = curVal;
          this.#client.sendInput({
            type:    'gamepad_button',
            index:   gp.index,
            button:  i,
            value:   curVal,
            pressed: btn.pressed,
          });
        }
      }

      // Axes — apply dead-zone filtering.
      for (let i = 0; i < gp.axes.length; i++) {
        const raw    = gp.axes[i];
        const curVal = Math.abs(raw) < 0.05 ? 0 : raw;
        if (curVal !== (prev.axes[i] ?? 0)) {
          prev.axes[i] = curVal;
          this.#client.sendInput({
            type:  'gamepad_axis',
            index: gp.index,
            axis:  i,
            value: curVal,
          });
        }
      }
    }
  }
}
