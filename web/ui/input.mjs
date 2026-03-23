/**
 * input.mjs — InputHandler for Lumen.
 *
 * Forwards keyboard, pointer, and wheel events from the browser to the
 * compositor via the LumenClient data channel.
 */

import { KEY_MAP, BTN_CODES } from '../lumen-client.mjs';
import { getDisplayScale, toCompositorCoords, compositorToDisplayCoords } from './coords.mjs';

// Evdev scancodes that are modifier keys. Used to distinguish regular keys
// from modifiers when synthesising missing keyup events (see #releaseNonModifierKeys).
const MODIFIER_SCANCODES = new Set([
  29,  // ControlLeft
  42,  // ShiftLeft
  54,  // ShiftRight
  56,  // AltLeft
  97,  // ControlRight
  100, // AltRight
  125, // MetaLeft
  126, // MetaRight
]);

export class InputHandler {
  #videoEl;
  #client;
  #cursor;
  #onUserGesture;
  #macModeEl;     // HTMLInputElement checkbox for Mac Mode (may be null)
  #pointerLocked  = false;
  #vMouseX = 0;   // virtual cursor position in compositor pixel space
  #vMouseY = 0;
  /** When true, pointer events with pointerType==='touch' are ignored so the
   *  TouchHandler can handle them without double-dispatching. */
  #touchActive = false;
  #handlers = {}; // saved bound handler refs for unbind()
  /** Evdev scancodes of keys currently held down. Used to synthesise key-up
   *  events when the browser window loses focus (e.g. Super triggering GNOME
   *  overview steals focus before the keyup event can fire). */
  #pressedKeys = new Set();

  /**
   * @param {HTMLVideoElement} videoEl
   * @param {import('../lumen-client.mjs').LumenClient} client
   * @param {import('./cursor.mjs').CursorManager} cursor
   * @param {(() => void) | null} onUserGesture  Called on first keydown or pointerdown gesture.
   * @param {HTMLInputElement | null} macModeEl  Checkbox that enables Mac Mode (swaps Cmd↔Ctrl).
   */
  constructor(videoEl, client, cursor, onUserGesture = null, macModeEl = null) {
    this.#videoEl       = videoEl;
    this.#client        = client;
    this.#cursor        = cursor;
    this.#onUserGesture = onUserGesture;
    this.#macModeEl     = macModeEl ?? null;
  }

  /** Attach all input event listeners. */
  bind() {
    const video  = this.#videoEl;
    // Pointer events go on the container so the canvas overlay doesn't
    // intercept pointerdown in some browsers.
    const target = video.parentElement ?? video;

    const onKeyDown    = (e) => this.#handleKeyDown(e);
    const onKeyUp      = (e) => this.#handleKeyUp(e);
    const onMove       = (e) => this.#handlePointerMove(e);
    const onDown       = (e) => this.#handlePointerDown(e);
    const onUp         = (e) => this.#handlePointerUp(e);
    const onMenu       = (e) => e.preventDefault();
    const onWheel      = (e) => this.#handleWheel(e);
    // Release all held keys when the browser window loses focus so the
    // compositor never gets stuck thinking a key (e.g. Super) is held down.
    const onBlur       = () => this.#releaseAllKeys();
    const onVisChange  = () => { if (document.hidden) this.#releaseAllKeys(); };

    video.addEventListener('keydown', onKeyDown);
    video.addEventListener('keyup',   onKeyUp);
    target.addEventListener('pointermove',  onMove);
    target.addEventListener('pointerdown',  onDown);
    target.addEventListener('pointerup',    onUp);
    target.addEventListener('contextmenu',  onMenu);
    target.addEventListener('wheel',        onWheel, { passive: false });
    window.addEventListener('blur',         onBlur);
    document.addEventListener('visibilitychange', onVisChange);

    this.#handlers = { video, target, onKeyDown, onKeyUp, onMove, onDown, onUp, onMenu, onWheel, onBlur, onVisChange };
  }

  /** Detach all input event listeners. */
  unbind() {
    const { video, target, onKeyDown, onKeyUp, onMove, onDown, onUp, onMenu, onWheel, onBlur, onVisChange } = this.#handlers;
    if (!video) return;
    this.#releaseAllKeys();
    video.removeEventListener('keydown', onKeyDown);
    video.removeEventListener('keyup',   onKeyUp);
    target.removeEventListener('pointermove',  onMove);
    target.removeEventListener('pointerdown',  onDown);
    target.removeEventListener('pointerup',    onUp);
    target.removeEventListener('contextmenu',  onMenu);
    target.removeEventListener('wheel',        onWheel);
    window.removeEventListener('blur',         onBlur);
    document.removeEventListener('visibilitychange', onVisChange);
    this.#handlers = {};
  }

  /**
   * Called by LumenUI when the browser acquires pointer lock on the video element.
   * Initialises the virtual cursor at the centre of the compositor output.
   * @param {number} vw  Compositor output width in pixels.
   * @param {number} vh  Compositor output height in pixels.
   */
  onPointerLockAcquired(vw, vh) {
    this.#pointerLocked = true;
    this.#vMouseX = vw / 2;
    this.#vMouseY = vh / 2;
    const dp = compositorToDisplayCoords(this.#videoEl, this.#vMouseX, this.#vMouseY);
    this.#cursor.moveTo(dp.x, dp.y);
  }

  /** Called by LumenUI when pointer lock is released. */
  onPointerLockReleased() {
    this.#pointerLocked = false;
  }

  /**
   * Enable or disable touch-event suppression. When enabled, pointer events
   * with pointerType==='touch' are silently dropped so that TouchHandler can
   * handle all touch input without double-dispatching.
   * @param {boolean} active
   */
  setTouchActive(active) {
    this.#touchActive = active;
  }

  /**
   * Return the current virtual cursor position in compositor pixel space.
   * @returns {{ x: number, y: number }}
   */
  getMousePos() {
    return { x: this.#vMouseX, y: this.#vMouseY };
  }

  /**
   * Update the virtual cursor position (called by TouchHandler so the two
   * share a single source of truth for the compositor cursor location).
   * @param {number} x
   * @param {number} y
   */
  setMousePos(x, y) {
    this.#vMouseX = x;
    this.#vMouseY = y;
  }

  // ── private event handlers ────────────────────────────────────────────────────

  /** Swap Meta↔Control scancodes when Mac Mode is active. */
  #applyMacMode(sc) {
    if (!this.#macModeEl?.checked) return sc;
    if (sc === 125) return 29;  // MetaLeft  → ControlLeft
    if (sc === 126) return 97;  // MetaRight → ControlRight
    if (sc === 29)  return 125; // ControlLeft  → MetaLeft
    if (sc === 97)  return 126; // ControlRight → MetaRight
    return sc;
  }

  #handleKeyDown(e) {
    e.preventDefault();
    // Browser auto-repeat events (e.repeat=true) arrive at the OS repeat rate
    // (~30ms intervals) and must not be forwarded.  In Wayland, key repeat is
    // client-side: each client receives wl_keyboard.repeat_info(rate, delay)
    // from the compositor and manages its own repeat timer.  Forwarding browser
    // repeat events as extra key-press messages causes clients to reset their
    // timer on every event, so the timer never fires and repeat never works.
    if (e.repeat) return;
    this.#onUserGesture?.();
    const sc = this.#applyMacMode(KEY_MAP[e.code]);
    if (sc === undefined) return;
    this.#pressedKeys.add(sc);
    this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 1 });
  }

  #handleKeyUp(e) {
    e.preventDefault();
    const sc = this.#applyMacMode(KEY_MAP[e.code]);
    if (sc === undefined) return;
    this.#pressedKeys.delete(sc);
    this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 0 });
    // macOS intercepts Cmd+key shortcuts at the OS level and does not fire
    // keyup for the non-modifier key (e.g. Cmd+C never gets keyup for C).
    // In Mac Mode the Cmd (Meta) key acts as Ctrl, so when it is released we
    // synthesise keyup for any regular keys that were stuck held.
    if (this.#macModeEl?.checked && (e.code === 'MetaLeft' || e.code === 'MetaRight')) {
      this.#releaseNonModifierKeys();
    }
  }

  /** Send key-up for every currently held non-modifier key and remove it from
   *  the tracking set.  Used to recover from macOS swallowing keyup events for
   *  non-modifier keys used in Cmd+key chords (Mac Mode only). */
  #releaseNonModifierKeys() {
    for (const sc of [...this.#pressedKeys]) {
      if (!MODIFIER_SCANCODES.has(sc)) {
        this.#pressedKeys.delete(sc);
        this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 0 });
      }
    }
  }

  /** Send key-up for every currently held key and clear the tracking set.
   *  Called when the browser window loses focus so the compositor never gets
   *  stuck with a key held (e.g. Super triggering the GNOME overview). */
  #releaseAllKeys() {
    for (const sc of this.#pressedKeys) {
      this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 0 });
    }
    this.#pressedKeys.clear();
  }

  #handlePointerMove(e) {
    if (e.pointerType === 'touch' && this.#touchActive) return;
    if (this.#pointerLocked) {
      const { scaleX, scaleY, vw, vh } = getDisplayScale(this.#videoEl);
      // Send raw, unclamped deltas so that fullscreen games (which rely on
      // zwp_relative_pointer_v1) receive accurate motion past screen edges.
      const dx = e.movementX * scaleX;
      const dy = e.movementY * scaleY;
      this.#client.sendInput({ type: 'pointer_motion_relative', dx, dy });
      // Keep the virtual cursor position clamped so that button events
      // (clicks) after a lock session still target the correct surface.
      this.#vMouseX = Math.max(0, Math.min(vw - 1, this.#vMouseX + dx));
      this.#vMouseY = Math.max(0, Math.min(vh - 1, this.#vMouseY + dy));
      const dp = compositorToDisplayCoords(this.#videoEl, this.#vMouseX, this.#vMouseY);
      this.#cursor.moveTo(dp.x, dp.y);
    } else {
      const { x, y } = toCompositorCoords(this.#videoEl, e.clientX, e.clientY);
      this.#vMouseX = x;
      this.#vMouseY = y;
      this.#client.sendInput({ type: 'pointer_motion', x, y });
      const rect = this.#videoEl.getBoundingClientRect();
      this.#cursor.moveTo(e.clientX - rect.left, e.clientY - rect.top);
    }
  }

  #handlePointerDown(e) {
    if (e.pointerType === 'touch' && this.#touchActive) return;
    e.preventDefault();
    this.#videoEl.focus();
    try { this.#videoEl.setPointerCapture(e.pointerId); } catch (err) { console.warn('[lumen] setPointerCapture failed:', err.message); }
    this.#onUserGesture?.();
    const btn = BTN_CODES[e.button];
    if (btn === undefined) { console.warn('[lumen] dropping unknown button', e.button); return; }
    if (!this.#pointerLocked) {
      const { x, y } = toCompositorCoords(this.#videoEl, e.clientX, e.clientY);
      this.#client.sendInput({ type: 'pointer_motion', x, y });
    }
    this.#client.sendInput({ type: 'pointer_button', btn, state: 1 });
  }

  #handlePointerUp(e) {
    if (e.pointerType === 'touch' && this.#touchActive) return;
    const btn = BTN_CODES[e.button];
    if (btn === undefined) return;
    this.#client.sendInput({ type: 'pointer_button', btn, state: 0 });
  }

  #handleWheel(e) {
    e.preventDefault();
    let { deltaX, deltaY, deltaMode } = e;
    let source = 'continuous';
    let v120_x = 0, v120_y = 0;

    if (deltaMode === WheelEvent.DOM_DELTA_LINE) {
      // Classic mouse wheel — each unit is one scroll line (~3 per notch).
      // Multiply to pixels and compute v120 for Wayland axis_value120.
      source = 'wheel';
      v120_x = Math.round(deltaX * 40);   // 3 lines/notch × 40 = 120 per notch
      v120_y = Math.round(deltaY * 40);
      deltaX *= 20;
      deltaY *= 20;
    } else if (deltaMode === WheelEvent.DOM_DELTA_PAGE) {
      source = 'wheel';
      v120_x = Math.sign(deltaX) * 120;
      v120_y = Math.sign(deltaY) * 120;
      deltaX *= 800;
      deltaY *= 800;
    }
    // DOM_DELTA_PIXEL: touchpad or pixel-precise wheel — use values as-is,
    // source stays 'continuous', no v120.

    this.#client.sendInput({ type: 'pointer_axis', x: deltaX, y: deltaY, source, v120_x, v120_y });
  }
}
