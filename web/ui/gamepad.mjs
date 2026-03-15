/**
 * gamepad.mjs — GamepadController for Lumen.
 *
 * Manages browser gamepad connections and a requestAnimationFrame polling
 * loop that sends button/axis delta events to the compositor.
 */

export class GamepadController {
  #client;
  #rafHandle = null;    // requestAnimationFrame handle for the poll loop
  #state     = new Map(); // gamepad index → { buttons: Float32Array, axes: Float32Array }

  /**
   * @param {import('../lumen-client.mjs').LumenClient} client
   */
  constructor(client) {
    this.#client = client;
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

  // ── private helpers ──────────────────────────────────────────────────────────

  #handleConnected(e) {
    const gp    = e.gamepad;
    const index = gp.index;
    this.#state.set(index, {
      buttons: new Float32Array(gp.buttons.length),
      axes:    new Float32Array(gp.axes.length),
    });
    this.#client.sendInput({
      type:        'gamepad_connected',
      index,
      name:        gp.id,
      num_axes:    gp.axes.length,
      num_buttons: gp.buttons.length,
    });
    if (this.#rafHandle === null) {
      this.#startPoll();
    }
  }

  #handleDisconnected(e) {
    const index = e.gamepad.index;
    this.#state.delete(index);
    this.#client.sendInput({ type: 'gamepad_disconnected', index });
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
   * send events only for changed buttons/axes.
   */
  #poll() {
    const gamepads = navigator.getGamepads();
    for (const gp of gamepads) {
      if (!gp) continue;
      const prev = this.#state.get(gp.index);
      if (!prev) continue;

      for (let i = 0; i < gp.buttons.length; i++) {
        const btn     = gp.buttons[i];
        const curVal  = btn.value;
        const prevVal = prev.buttons[i] ?? 0;
        if (curVal !== prevVal) {
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
        const raw     = gp.axes[i];
        const curVal  = Math.abs(raw) < 0.05 ? 0 : raw;
        const prevVal = prev.axes[i] ?? 0;
        if (curVal !== prevVal) {
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
