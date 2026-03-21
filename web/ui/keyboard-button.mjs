/**
 * keyboard-button.mjs — FloatingKeyboard for Lumen.
 *
 * Renders a small draggable floating button that, when tapped, focuses a
 * hidden <textarea> to summon the native on-screen keyboard on mobile devices.
 * Keyboard events from the textarea are forwarded to the compositor.
 *
 * Only shown on touch-capable devices and only while connected.
 *
 * Key lookup order:
 *   1. KEY_MAP[e.code]   — works for physical / Bluetooth keyboards and most
 *                          desktop browser virtual keyboards.
 *   2. CHAR_MAP[e.key]   — fallback for mobile virtual keyboards that report
 *                          e.code as '' or 'Unidentified'.
 *   3. input event       — last resort for iOS/Android keyboards that swallow
 *                          keydown entirely; synthesises a press+release pair.
 */

import { KEY_MAP } from '../lumen-client.mjs';

// ── CHAR_MAP: e.key → { sc, shift } ──────────────────────────────────────────
// Maps the string value of KeyboardEvent.key to an evdev scancode and whether
// the Shift modifier needs to be held.  Covers the most common keys produced
// by mobile soft keyboards.

const CHAR_MAP = (() => {
  const m = new Map();

  // Special / control keys (no shift)
  const specials = {
    'Enter': 28, 'Backspace': 14, 'Tab': 15, 'Escape': 1, ' ': 57,
    'ArrowUp': 103, 'ArrowLeft': 105, 'ArrowRight': 106, 'ArrowDown': 108,
    'Delete': 111, 'Home': 102, 'End': 107, 'PageUp': 104, 'PageDown': 109,
    'CapsLock': 58,
  };
  for (const [key, sc] of Object.entries(specials)) m.set(key, { sc, shift: false });

  // Lowercase letters → scancodes (a=30, b=48, c=46, …)
  const letterScancodes = {
    a:30,b:48,c:46,d:32,e:18,f:33,g:34,h:35,i:23,j:36,k:37,l:38,
    m:50,n:49,o:24,p:25,q:16,r:19,s:31,t:20,u:22,v:47,w:17,x:45,y:21,z:44,
  };
  for (const [ch, sc] of Object.entries(letterScancodes)) {
    m.set(ch,           { sc, shift: false });
    m.set(ch.toUpperCase(), { sc, shift: true  });
  }

  // Digits and their shifted symbols (US layout)
  const digitRows = [
    ['1','!',2],['2','@',3],['3','#',4],['4','$',5],['5','%',6],
    ['6','^',7],['7','&',8],['8','*',9],['9','(',10],['0',')',11],
  ];
  for (const [digit, sym, sc] of digitRows) {
    m.set(digit, { sc, shift: false });
    m.set(sym,   { sc, shift: true  });
  }

  // Common punctuation (US layout)
  const punct = [
    ['-','_',12],[  '=','+',13],
    ['[','{',26],  [']','}',27],
    ['\\','|',43],
    [';',':',39],  ["'",'"',40],
    ['`','~',41],
    [',','<',51],  ['.','>',52],  ['/','?',53],
  ];
  for (const [plain, shifted, sc] of punct) {
    m.set(plain,   { sc, shift: false });
    m.set(shifted, { sc, shift: true  });
  }

  return m;
})();

const SHIFT_SC = 42; // KEY_LEFTSHIFT evdev scancode

const LS_POS_KEY = 'lumen.keyboardBtnPos';

export class FloatingKeyboard {
  #client;
  #btn     = null;   // floating <button> element
  #input   = null;   // hidden <textarea> element
  #visible = false;  // whether the button is currently shown

  /** Scancodes of keys currently held by this module (for release-all on blur). */
  #heldKeys = new Set();

  /**
   * @param {import('../lumen-client.mjs').LumenClient} client
   */
  constructor(client) {
    this.#client = client;
    this.#buildDOM();
  }

  /** Show the keyboard button (call when connected). */
  show() {
    this.#visible = true;
    this.#btn.style.display = '';
  }

  /** Hide the keyboard button (call when disconnected). */
  hide() {
    this.#visible = false;
    this.#btn.style.display = 'none';
    this.#releaseAll();
    if (document.activeElement === this.#input) this.#input.blur();
  }

  // ── DOM construction ──────────────────────────────────────────────────────────

  #buildDOM() {
    // Hidden textarea that receives actual keyboard input.
    const ta = document.createElement('textarea');
    ta.setAttribute('autocomplete',    'off');
    ta.setAttribute('autocorrect',     'off');
    ta.setAttribute('autocapitalize',  'none');
    ta.setAttribute('spellcheck',      'false');
    ta.setAttribute('aria-hidden',     'true');
    ta.setAttribute('tabindex',        '-1');
    ta.style.cssText = 'position:fixed;left:-9999px;top:0;width:1px;height:1px;opacity:0;';
    document.body.appendChild(ta);
    this.#input = ta;

    // Floating keyboard button.
    const btn = document.createElement('button');
    btn.id          = 'keyboard-btn';
    btn.textContent = '⌨';
    btn.title       = 'Show keyboard';
    btn.style.display = 'none'; // hidden until connected
    document.body.appendChild(btn);
    this.#btn = btn;

    this.#restorePosition();
    this.#bindDrag();
    this.#bindKeyboardEvents();
  }

  // ── position persistence ──────────────────────────────────────────────────────

  #restorePosition() {
    try {
      const raw = localStorage.getItem(LS_POS_KEY);
      if (!raw) return;
      const { right, bottom } = JSON.parse(raw);
      if (typeof right === 'number' && typeof bottom === 'number') {
        this.#btn.style.right  = `${right}px`;
        this.#btn.style.bottom = `${bottom}px`;
      }
    } catch { /* ignore malformed saved value */ }
  }

  #savePosition() {
    const right  = parseFloat(this.#btn.style.right)  || 16;
    const bottom = parseFloat(this.#btn.style.bottom) || 16;
    try { localStorage.setItem(LS_POS_KEY, JSON.stringify({ right, bottom })); } catch { /* quota */ }
  }

  // ── drag behaviour ────────────────────────────────────────────────────────────

  #bindDrag() {
    const btn = this.#btn;
    let startX = 0, startY = 0;
    let startRight = 0, startBottom = 0;
    let dragging = false;
    const DRAG_THRESHOLD = 6; // px

    // Use Pointer Events for drag so it works with both mouse and touch.
    btn.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      btn.setPointerCapture(e.pointerId);
      startX      = e.clientX;
      startY      = e.clientY;
      startRight  = parseFloat(btn.style.right)  || 16;
      startBottom = parseFloat(btn.style.bottom) || 16;
      dragging    = false;
    });

    btn.addEventListener('pointermove', (e) => {
      if (!btn.hasPointerCapture(e.pointerId)) return;
      const dx = e.clientX - startX;
      const dy = e.clientY - startY;
      if (!dragging && Math.hypot(dx, dy) > DRAG_THRESHOLD) {
        dragging = true;
        btn.classList.add('dragging');
      }
      if (dragging) {
        // right/bottom anchoring keeps the button visible when viewport resizes.
        const newRight  = Math.max(0, startRight  - dx);
        const newBottom = Math.max(0, startBottom + dy);
        btn.style.right  = `${newRight}px`;
        btn.style.bottom = `${newBottom}px`;
      }
    });

    btn.addEventListener('pointerup', (e) => {
      if (!btn.hasPointerCapture(e.pointerId)) return;
      btn.releasePointerCapture(e.pointerId);
      btn.classList.remove('dragging');
      if (dragging) {
        this.#savePosition();
        dragging = false;
      } else {
        // It was a tap, not a drag — open the keyboard.
        this.#openKeyboard();
      }
    });
  }

  #openKeyboard() {
    const ta = this.#input;
    // Reset value so mobile keyboards don't pre-select existing text.
    ta.value = '';
    ta.focus({ preventScroll: true });
    // Some iOS versions need a programmatic select after focus.
    try { ta.setSelectionRange(0, 0); } catch { /* ignore */ }
  }

  // ── keyboard event forwarding ─────────────────────────────────────────────────

  #bindKeyboardEvents() {
    const ta = this.#input;

    ta.addEventListener('keydown', (e) => {
      e.preventDefault();
      if (e.repeat) return;
      const sc = this.#resolveKey(e);
      if (sc === null) return;
      this.#pressKey(sc, e.key);
    });

    ta.addEventListener('keyup', (e) => {
      e.preventDefault();
      const sc = this.#resolveKey(e);
      if (sc === null) return;
      this.#releaseKey(sc, e.key);
    });

    // Fallback for mobile browsers that swallow keydown (common on iOS):
    // synthesise a full press+release from the composed character.
    ta.addEventListener('input', (e) => {
      const data = e.data;
      if (!data) return;
      for (const ch of data) {
        const entry = CHAR_MAP.get(ch);
        if (!entry) continue;
        if (entry.shift) {
          this.#sendKey(SHIFT_SC, 1);
          this.#sendKey(entry.sc, 1);
          this.#sendKey(entry.sc, 0);
          this.#sendKey(SHIFT_SC, 0);
        } else {
          this.#sendKey(entry.sc, 1);
          this.#sendKey(entry.sc, 0);
        }
      }
      // Keep textarea empty so accumulated text doesn't confuse future input events.
      ta.value = '';
    });

    ta.addEventListener('blur', () => this.#releaseAll());
  }

  /** Resolve a KeyboardEvent to a scancode, or null if unknown. */
  #resolveKey(e) {
    // Primary: e.code → KEY_MAP (reliable for physical/Bluetooth keyboards).
    if (e.code && e.code !== 'Unidentified') {
      const sc = KEY_MAP[e.code];
      if (sc !== undefined) return sc;
    }
    // Fallback: e.key → CHAR_MAP (mobile virtual keyboards).
    const entry = CHAR_MAP.get(e.key);
    return entry ? entry.sc : null;
  }

  /** Send key-down, tracking shift if the CHAR_MAP entry requires it. */
  #pressKey(sc, key) {
    const entry = CHAR_MAP.get(key);
    if (entry?.shift) this.#sendKey(SHIFT_SC, 1);
    this.#sendKey(sc, 1);
    this.#heldKeys.add(sc);
  }

  /** Send key-up, releasing shift if needed. */
  #releaseKey(sc, key) {
    this.#sendKey(sc, 0);
    this.#heldKeys.delete(sc);
    const entry = CHAR_MAP.get(key);
    if (entry?.shift) this.#sendKey(SHIFT_SC, 0);
  }

  #sendKey(sc, state) {
    this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state });
  }

  /** Release all currently held keys. Called on blur and on hide(). */
  #releaseAll() {
    for (const sc of this.#heldKeys) {
      this.#sendKey(sc, 0);
    }
    this.#heldKeys.clear();
    // Always release shift in case it was left held.
    this.#sendKey(SHIFT_SC, 0);
  }
}
