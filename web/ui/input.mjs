/**
 * input.mjs — InputHandler for Lumen.
 *
 * Forwards keyboard, pointer, and wheel events from the browser to the
 * compositor via the LumenClient data channel.
 */

import { KEY_MAP, BTN_CODES } from '../lumen-client.mjs';
import { getDisplayScale, toCompositorCoords, compositorToDisplayCoords } from './coords.mjs';

export class InputHandler {
  #videoEl;
  #client;
  #cursor;
  #onUserGesture;
  #pointerLocked = false;
  #vMouseX = 0;   // virtual cursor position in compositor pixel space (pointer-lock only)
  #vMouseY = 0;
  #handlers = {}; // saved bound handler refs for unbind()

  /**
   * @param {HTMLVideoElement} videoEl
   * @param {import('../lumen-client.mjs').LumenClient} client
   * @param {import('./cursor.mjs').CursorManager} cursor
   * @param {(() => void) | null} onUserGesture  Called on first keydown or pointerdown gesture.
   */
  constructor(videoEl, client, cursor, onUserGesture = null) {
    this.#videoEl       = videoEl;
    this.#client        = client;
    this.#cursor        = cursor;
    this.#onUserGesture = onUserGesture;
  }

  /** Attach all input event listeners. */
  bind() {
    const video  = this.#videoEl;
    // Pointer events go on the container so the canvas overlay doesn't
    // intercept pointerdown in some browsers.
    const target = video.parentElement ?? video;

    const onKeyDown = (e) => this.#handleKeyDown(e);
    const onKeyUp   = (e) => this.#handleKeyUp(e);
    const onMove    = (e) => this.#handlePointerMove(e);
    const onDown    = (e) => this.#handlePointerDown(e);
    const onUp      = (e) => this.#handlePointerUp(e);
    const onMenu    = (e) => e.preventDefault();
    const onWheel   = (e) => this.#handleWheel(e);

    video.addEventListener('keydown', onKeyDown);
    video.addEventListener('keyup',   onKeyUp);
    target.addEventListener('pointermove',  onMove);
    target.addEventListener('pointerdown',  onDown);
    target.addEventListener('pointerup',    onUp);
    target.addEventListener('contextmenu',  onMenu);
    target.addEventListener('wheel',        onWheel, { passive: false });

    this.#handlers = { video, target, onKeyDown, onKeyUp, onMove, onDown, onUp, onMenu, onWheel };
  }

  /** Detach all input event listeners. */
  unbind() {
    const { video, target, onKeyDown, onKeyUp, onMove, onDown, onUp, onMenu, onWheel } = this.#handlers;
    if (!video) return;
    video.removeEventListener('keydown', onKeyDown);
    video.removeEventListener('keyup',   onKeyUp);
    target.removeEventListener('pointermove',  onMove);
    target.removeEventListener('pointerdown',  onDown);
    target.removeEventListener('pointerup',    onUp);
    target.removeEventListener('contextmenu',  onMenu);
    target.removeEventListener('wheel',        onWheel);
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

  // ── private event handlers ────────────────────────────────────────────────────

  #handleKeyDown(e) {
    e.preventDefault();
    this.#onUserGesture?.();
    const sc = KEY_MAP[e.code];
    if (sc === undefined) return;
    this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 1 });
  }

  #handleKeyUp(e) {
    e.preventDefault();
    const sc = KEY_MAP[e.code];
    if (sc === undefined) return;
    this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 0 });
  }

  #handlePointerMove(e) {
    if (this.#pointerLocked) {
      const { scaleX, scaleY, vw, vh } = getDisplayScale(this.#videoEl);
      this.#vMouseX = Math.max(0, Math.min(vw - 1, this.#vMouseX + e.movementX * scaleX));
      this.#vMouseY = Math.max(0, Math.min(vh - 1, this.#vMouseY + e.movementY * scaleY));
      this.#client.sendInput({ type: 'pointer_motion', x: this.#vMouseX, y: this.#vMouseY });
      const dp = compositorToDisplayCoords(this.#videoEl, this.#vMouseX, this.#vMouseY);
      this.#cursor.moveTo(dp.x, dp.y);
    } else {
      const { x, y } = toCompositorCoords(this.#videoEl, e.clientX, e.clientY);
      this.#client.sendInput({ type: 'pointer_motion', x, y });
      const rect = this.#videoEl.getBoundingClientRect();
      this.#cursor.moveTo(e.clientX - rect.left, e.clientY - rect.top);
    }
  }

  #handlePointerDown(e) {
    console.log('[lumen] pointerdown', { button: e.button, pointerId: e.pointerId, target: e.target?.tagName, dcState: this.#client.dcReadyState });
    e.preventDefault();
    this.#videoEl.focus();
    try { this.#videoEl.setPointerCapture(e.pointerId); } catch (err) { console.warn('[lumen] setPointerCapture failed:', err.message); }
    this.#onUserGesture?.();
    const btn = BTN_CODES[e.button];
    console.log('[lumen] btn lookup:', { eButton: e.button, btn });
    if (btn === undefined) { console.warn('[lumen] dropping unknown button', e.button); return; }
    if (!this.#pointerLocked) {
      const { x, y } = toCompositorCoords(this.#videoEl, e.clientX, e.clientY);
      console.log('[lumen] sending pointer_motion', { x, y });
      this.#client.sendInput({ type: 'pointer_motion', x, y });
    }
    console.log('[lumen] sending pointer_button', { btn, state: 1 });
    this.#client.sendInput({ type: 'pointer_button', btn, state: 1 });
  }

  #handlePointerUp(e) {
    const btn = BTN_CODES[e.button];
    if (btn === undefined) return;
    console.log('[lumen] sending pointer_button', { btn, state: 0 });
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
