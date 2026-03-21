/**
 * touch.mjs — TouchHandler for Lumen.
 *
 * Implements trackpad-style touch input for mobile browsers:
 *   - Single-finger drag        → relative cursor movement (1:1 delta mapping)
 *   - Quick tap                 → left click (< TAP_MAX_MS, < TAP_MAX_PX movement)
 *   - Long press (500 ms)       → right click on finger lift
 *   - Two-finger drag           → scroll wheel events
 *   - Double-tap drag           → hold left button while dragging (tap, then tap-and-hold
 *                                 within DOUBLE_TAP_MAX_MS to enter drag-lock; lift to release)
 *
 * Touch events call preventDefault() so they do not also fire as Pointer
 * Events, avoiding double-handling by InputHandler.
 */

import { getDisplayScale } from './coords.mjs';

const BTN_LEFT  = 272;
const BTN_RIGHT = 273;

/** Maximum milliseconds for a touch to be considered a tap. */
const TAP_MAX_MS  = 200;
/** Maximum total pixel movement (CSS px) for a touch to be considered a tap. */
const TAP_MAX_PX  = 10;
/** Hold duration in milliseconds before a stationary press becomes a right-click. */
const LONG_PRESS_MS = 500;
/** Maximum milliseconds between a tap and the start of the second touch to enter drag-lock. */
const DOUBLE_TAP_MAX_MS = 300;
/** Maximum CSS pixel distance between a tap and the second touch to enter drag-lock. */
const DOUBLE_TAP_MAX_PX = 30;

export class TouchHandler {
  #videoEl;
  #container;
  #client;
  #getMousePos;   // () => { x, y }  — reads current compositor cursor coords
  #setMousePos;   // (x, y) => void  — writes updated compositor cursor coords

  // Per-gesture single-touch state.
  #touchId        = null;   // identifier of the tracked single touch
  #startX         = 0;      // clientX at touchstart
  #startY         = 0;      // clientY at touchstart
  #lastX          = 0;      // clientX at previous touchmove
  #lastY          = 0;      // clientY at previous touchmove
  #startTime      = 0;      // Date.now() at touchstart
  #totalMovement  = 0;      // accumulated movement in CSS px
  #longPressTimer = null;   // setTimeout handle
  #isLongPress    = false;  // true when the 500ms timer fired and touch still held

  // Double-tap drag state.
  #lastTapTime    = 0;      // Date.now() when the last quick-tap fired (0 = none recent)
  #lastTapX       = 0;      // clientX of the last tap
  #lastTapY       = 0;      // clientY of the last tap
  #isDragLock     = false;  // true while left button is held for a double-tap drag

  // Two-finger scroll state.
  #scrollTouches = new Map(); // identifier → { x, y }
  #lastScrollMidX = 0;
  #lastScrollMidY = 0;

  #handlers = {};

  /**
   * @param {HTMLElement} container           The video container element.
   * @param {HTMLVideoElement} videoEl        The video element (used for coordinate scaling).
   * @param {import('../lumen-client.mjs').LumenClient} client
   * @param {() => { x: number, y: number }} getMousePos   Returns current compositor cursor position.
   * @param {(x: number, y: number) => void} setMousePos   Updates compositor cursor position.
   */
  constructor(container, videoEl, client, getMousePos, setMousePos) {
    this.#container   = container;
    this.#videoEl     = videoEl;
    this.#client      = client;
    this.#getMousePos = getMousePos;
    this.#setMousePos = setMousePos;
  }

  /** Attach touch event listeners. */
  bind() {
    const onStart  = (e) => this.#handleTouchStart(e);
    const onMove   = (e) => this.#handleTouchMove(e);
    const onEnd    = (e) => this.#handleTouchEnd(e);
    const onCancel = (e) => this.#handleTouchCancel(e);

    // passive:false required so preventDefault() can suppress pointer events
    // and native browser scroll/zoom.
    this.#container.addEventListener('touchstart',  onStart,  { passive: false });
    this.#container.addEventListener('touchmove',   onMove,   { passive: false });
    this.#container.addEventListener('touchend',    onEnd,    { passive: false });
    this.#container.addEventListener('touchcancel', onCancel, { passive: false });

    this.#handlers = { onStart, onMove, onEnd, onCancel };
  }

  /** Detach touch event listeners. */
  unbind() {
    const { onStart, onMove, onEnd, onCancel } = this.#handlers;
    if (!onStart) return;
    this.#container.removeEventListener('touchstart',  onStart);
    this.#container.removeEventListener('touchmove',   onMove);
    this.#container.removeEventListener('touchend',    onEnd);
    this.#container.removeEventListener('touchcancel', onCancel);
    this.#handlers = {};
    this.#cancelLongPress();
  }

  // ── private helpers ───────────────────────────────────────────────────────────

  #cancelLongPress() {
    if (this.#longPressTimer !== null) {
      clearTimeout(this.#longPressTimer);
      this.#longPressTimer = null;
    }
  }

  /** Send a mouse button press then immediate release. */
  #click(btn) {
    this.#client.sendInput({ type: 'pointer_button', btn, state: 1 });
    this.#client.sendInput({ type: 'pointer_button', btn, state: 0 });
  }

  /** Apply a compositor-space delta to the tracked cursor position. */
  #moveCursor(dxClient, dyClient) {
    const { scaleX, scaleY, vw, vh } = getDisplayScale(this.#videoEl);
    const pos = this.#getMousePos();
    const nx  = Math.max(0, Math.min(vw - 1, pos.x + dxClient * scaleX));
    const ny  = Math.max(0, Math.min(vh - 1, pos.y + dyClient * scaleY));
    this.#setMousePos(nx, ny);
    this.#client.sendInput({ type: 'pointer_motion', x: nx, y: ny });
  }

  // ── event handlers ────────────────────────────────────────────────────────────

  #handleTouchStart(e) {
    e.preventDefault();

    const touches = e.changedTouches;

    // ── two-finger scroll entry ───────────────────────────────────────────────
    if (e.touches.length === 2) {
      // Transition: cancel any in-progress single-touch gesture.
      this.#cancelLongPress();
      this.#touchId = null;
      // Clear tap state so a preceding tap doesn't accidentally trigger drag-lock.
      this.#lastTapTime = 0;

      // Record both touch positions for scroll delta computation.
      this.#scrollTouches.clear();
      for (const t of e.touches) {
        this.#scrollTouches.set(t.identifier, { x: t.clientX, y: t.clientY });
      }
      const [a, b] = [...this.#scrollTouches.values()];
      this.#lastScrollMidX = (a.x + b.x) / 2;
      this.#lastScrollMidY = (a.y + b.y) / 2;
      return;
    }

    // ── single-touch start ────────────────────────────────────────────────────
    if (e.touches.length === 1 && this.#touchId === null) {
      const t = touches[0];
      this.#touchId       = t.identifier;
      this.#startX        = t.clientX;
      this.#startY        = t.clientY;
      this.#lastX         = t.clientX;
      this.#lastY         = t.clientY;
      this.#startTime     = Date.now();
      this.#totalMovement = 0;
      this.#isLongPress   = false;

      // ── Double-tap drag detection ─────────────────────────────────────────
      // If this touch starts within the double-tap window (time + distance) of
      // the last recognized tap, enter drag-lock: hold the left button down.
      const timeSinceLastTap = Date.now() - this.#lastTapTime;
      const distFromLastTap  = Math.hypot(t.clientX - this.#lastTapX, t.clientY - this.#lastTapY);
      if (this.#lastTapTime > 0
          && timeSinceLastTap < DOUBLE_TAP_MAX_MS
          && distFromLastTap < DOUBLE_TAP_MAX_PX) {
        this.#isDragLock  = true;
        this.#lastTapTime = 0;  // consume the gesture
        this.#client.sendInput({ type: 'pointer_button', btn: BTN_LEFT, state: 1 });
        // Don't start the long-press timer during drag-lock.
        return;
      }

      // Start long-press timer.
      this.#longPressTimer = setTimeout(() => {
        this.#longPressTimer = null;
        // Only arm long-press if the finger hasn't moved too much.
        if (this.#totalMovement < TAP_MAX_PX) {
          this.#isLongPress = true;
        }
      }, LONG_PRESS_MS);
    }
  }

  #handleTouchMove(e) {
    e.preventDefault();

    // ── two-finger scroll ─────────────────────────────────────────────────────
    if (this.#scrollTouches.size === 2 && e.touches.length >= 2) {
      // Update positions for any changed touches.
      for (const t of e.changedTouches) {
        if (this.#scrollTouches.has(t.identifier)) {
          this.#scrollTouches.set(t.identifier, { x: t.clientX, y: t.clientY });
        }
      }
      const [a, b] = [...this.#scrollTouches.values()];
      const midX = (a.x + b.x) / 2;
      const midY = (a.y + b.y) / 2;
      const dx   = midX - this.#lastScrollMidX;
      const dy   = midY - this.#lastScrollMidY;
      if (dx !== 0 || dy !== 0) {
        // Negate: dragging fingers downward scrolls content upward (natural scroll).
        this.#client.sendInput({ type: 'pointer_axis', x: -dx, y: -dy, source: 'continuous', v120_x: 0, v120_y: 0 });
      }
      this.#lastScrollMidX = midX;
      this.#lastScrollMidY = midY;
      return;
    }

    // ── single-touch trackpad movement ────────────────────────────────────────
    if (this.#touchId === null) return;
    let tracked = null;
    for (const t of e.changedTouches) {
      if (t.identifier === this.#touchId) { tracked = t; break; }
    }
    if (!tracked) return;

    const dx = tracked.clientX - this.#lastX;
    const dy = tracked.clientY - this.#lastY;
    this.#lastX = tracked.clientX;
    this.#lastY = tracked.clientY;

    const dist = Math.hypot(tracked.clientX - this.#startX, tracked.clientY - this.#startY);
    this.#totalMovement = dist;

    // If the finger has moved beyond the tap threshold, cancel the long-press timer.
    if (dist >= TAP_MAX_PX) {
      this.#cancelLongPress();
      this.#isLongPress = false;
    }

    if (dx !== 0 || dy !== 0) {
      this.#moveCursor(dx, dy);
    }
  }

  #handleTouchEnd(e) {
    e.preventDefault();

    // ── two-finger scroll end ─────────────────────────────────────────────────
    if (this.#scrollTouches.size === 2) {
      for (const t of e.changedTouches) {
        this.#scrollTouches.delete(t.identifier);
      }
      // Send a scroll stop frame.
      if (this.#scrollTouches.size < 2) {
        this.#client.sendInput({ type: 'pointer_axis', x: 0, y: 0, source: 'continuous', v120_x: 0, v120_y: 0 });
        this.#scrollTouches.clear();
      }
      return;
    }

    // ── single-touch end ──────────────────────────────────────────────────────
    let tracked = null;
    for (const t of e.changedTouches) {
      if (t.identifier === this.#touchId) { tracked = t; break; }
    }
    if (!tracked) return;

    this.#cancelLongPress();
    const wasLongPress = this.#isLongPress;
    this.#touchId      = null;
    this.#isLongPress  = false;

    // ── Drag-lock end ─────────────────────────────────────────────────────────
    if (this.#isDragLock) {
      this.#isDragLock = false;
      this.#client.sendInput({ type: 'pointer_button', btn: BTN_LEFT, state: 0 });
      return;
    }

    const elapsed = Date.now() - this.#startTime;

    if (wasLongPress) {
      // Long-press: right click on release.
      this.#click(BTN_RIGHT);
    } else if (elapsed < TAP_MAX_MS && this.#totalMovement < TAP_MAX_PX) {
      // Quick tap: left click. Record position for potential double-tap drag.
      this.#click(BTN_LEFT);
      this.#lastTapTime = Date.now();
      this.#lastTapX    = tracked.clientX;
      this.#lastTapY    = tracked.clientY;
    }
    // Otherwise: drag ended — cursor already moved, no click needed.
  }

  #handleTouchCancel(e) {
    e.preventDefault();
    this.#cancelLongPress();
    if (this.#isDragLock) {
      this.#isDragLock = false;
      this.#client.sendInput({ type: 'pointer_button', btn: BTN_LEFT, state: 0 });
    }
    this.#touchId     = null;
    this.#isLongPress = false;
    this.#lastTapTime = 0;
    this.#scrollTouches.clear();
  }
}
