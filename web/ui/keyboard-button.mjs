/**
 * keyboard-button.mjs — FloatingKeyboard for Lumen.
 *
 * Renders a small draggable floating button that, when tapped, focuses a
 * hidden <textarea> to summon the native on-screen keyboard on mobile devices.
 * Keyboard events from the textarea are forwarded to the compositor.
 *
 * Also renders a modifier/special-key overlay that floats above the on-screen
 * keyboard whenever it is open.  Modifier buttons (Ctrl, Alt, Shift, Super)
 * are sticky: they latch on until the next non-modifier key press, at which
 * point the whole chord is sent atomically and modifiers reset.  Special-key
 * buttons (Tab, Esc, F1–F5) send their key immediately (with any latched
 * modifiers applied).
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

// Modifier key definitions for the overlay bar.
const MODIFIERS = [
  { label: 'Shift', sc: 42  },
  { label: 'Ctrl',  sc: 29  },
  { label: 'Alt',   sc: 56  },
  { label: 'Super', sc: 125 },
];

// Special (non-modifier) key definitions for the overlay bar.
const SPECIAL_KEYS = [
  { label: 'Tab', sc: 15 },
  { label: 'Esc', sc: 1  },
  { label: 'F1',  sc: 59 },
  { label: 'F2',  sc: 60 },
  { label: 'F3',  sc: 61 },
  { label: 'F4',  sc: 62 },
  { label: 'F5',  sc: 63 },
];

const LS_POS_KEY = 'lumen.keyboardBtnPos';

export class FloatingKeyboard {
  #client;
  #btn     = null;   // floating <button> element
  #input   = null;   // hidden <textarea> element
  #overlay = null;   // modifier/special-key overlay bar
  #visible = false;  // whether the button is currently shown

  /** Scancodes of keys currently held by this module (for release-all on blur). */
  #heldKeys = new Set();

  /**
   * Scancodes of modifier keys currently latched via the overlay.
   * Cleared automatically after the first non-modifier key press/chord.
   */
  #activeModifiers = new Set();

  /** Map from modifier scancode → overlay button element (for visual state). */
  #modBtns = new Map();

  /** setTimeout handle used to defer blur-triggered cleanup (see #bindKeyboardEvents). */
  #blurTimer = null;

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
    clearTimeout(this.#blurTimer);
    this.#btn.style.display = 'none';
    this.#overlay.style.display = 'none';
    this.#releaseAll();
    if (document.activeElement === this.#input) this.#input.blur();
  }

  // ── DOM construction ──────────────────────────────────────────────────────────

  #buildDOM() {
    // Hidden textarea that receives actual keyboard input.
    // autocomplete="new-password" is the most effective cross-browser trick to
    // suppress the soft keyboard suggestion/autocorrect bar on Android and iOS.
    const ta = document.createElement('textarea');
    ta.setAttribute('autocomplete',    'new-password');
    ta.setAttribute('autocorrect',     'off');
    ta.setAttribute('autocapitalize',  'none');
    ta.setAttribute('spellcheck',      'false');
    ta.setAttribute('inputmode',       'text');
    ta.setAttribute('data-gramm',              'false');
    ta.setAttribute('data-gramm_editor',       'false');
    ta.setAttribute('data-enable-grammarly',   'false');
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

    // Modifier / special-key overlay bar.
    this.#buildOverlay();

    this.#restorePosition();
    this.#bindDrag();
    this.#bindKeyboardEvents();
  }

  // ── modifier overlay ──────────────────────────────────────────────────────────

  #buildOverlay() {
    const bar = document.createElement('div');
    bar.id = 'keyboard-overlay';
    bar.style.display = 'none';

    // Modifier toggle buttons.
    for (const { label, sc } of MODIFIERS) {
      const b = document.createElement('button');
      b.className   = 'kb-mod-btn';
      b.textContent = label;
      b.tabIndex    = -1;
      b.dataset.sc  = sc;
      b.addEventListener('pointerdown', (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.#toggleModifier(sc, b);
        // Return focus to the textarea so the keyboard stays open.
        this.#input.focus({ preventScroll: true });
      });
      bar.appendChild(b);
      this.#modBtns.set(sc, b);
    }

    // Divider.
    const sep = document.createElement('span');
    sep.className = 'kb-overlay-sep';
    bar.appendChild(sep);

    // Special key buttons.
    for (const { label, sc } of SPECIAL_KEYS) {
      const b = document.createElement('button');
      b.className   = 'kb-special-btn';
      b.textContent = label;
      b.tabIndex    = -1;
      b.addEventListener('pointerdown', (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.#sendChord(sc);
        // Return focus to the textarea so the keyboard stays open.
        this.#input.focus({ preventScroll: true });
      });
      bar.appendChild(b);
    }

    document.body.appendChild(bar);
    this.#overlay = bar;

    // Reposition overlay whenever the visual viewport changes (keyboard open/close).
    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', () => this.#repositionOverlay());
      window.visualViewport.addEventListener('scroll', () => this.#repositionOverlay());
    }
  }

  /** Pin the overlay to the top edge of the on-screen keyboard gap. */
  #repositionOverlay() {
    if (!window.visualViewport) return;
    const vv = window.visualViewport;
    const bottom = Math.max(0, window.innerHeight - (vv.offsetTop + vv.height));
    this.#overlay.style.bottom = `${bottom}px`;
  }

  /** Toggle a modifier button on/off. */
  #toggleModifier(sc, btn) {
    if (this.#activeModifiers.has(sc)) {
      this.#activeModifiers.delete(sc);
      btn.classList.remove('active');
    } else {
      this.#activeModifiers.add(sc);
      btn.classList.add('active');
    }
  }

  /**
   * Send a chord: keydown for each latched modifier, then keydown+keyup for sc,
   * then keyup for each modifier in reverse.  Clears all latched modifiers after.
   * @param {number} sc  Evdev scancode for the non-modifier key.
   */
  #sendChord(sc) {
    const mods = [...this.#activeModifiers];
    for (const m of mods)         this.#sendKey(m, 1);
    this.#sendKey(sc, 1);
    this.#sendKey(sc, 0);
    for (const m of [...mods].reverse()) this.#sendKey(m, 0);
    this.#clearModifiers();
  }

  /** Reset all latched modifiers and update button visuals. */
  #clearModifiers() {
    for (const sc of this.#activeModifiers) {
      this.#modBtns.get(sc)?.classList.remove('active');
    }
    this.#activeModifiers.clear();
  }

  // ── position persistence ───────────────────────────────────────────────────────

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

  /** Re-clamp button position after a viewport resize or orientation change. */
  #clampPosition() {
    const btn = this.#btn;
    const maxRight  = window.innerWidth  - btn.offsetWidth;
    const maxBottom = window.innerHeight - btn.offsetHeight;
    const right  = Math.max(0, Math.min(maxRight,  parseFloat(btn.style.right)  || 16));
    const bottom = Math.max(0, Math.min(maxBottom, parseFloat(btn.style.bottom) || 16));
    btn.style.right  = `${right}px`;
    btn.style.bottom = `${bottom}px`;
  }

  #bindDrag() {
    const btn = this.#btn;

    window.addEventListener('resize', () => this.#clampPosition());

    // Blur the hidden textarea when the user taps outside the button so that
    // iOS does not re-show the soft keyboard on the next canvas interaction.
    // Capture phase fires before the canvas pointerdown handler.
    document.addEventListener('pointerdown', (e) => {
      if (e.target !== btn && e.target !== this.#input) {
        this.#input.blur();
      }
    }, { capture: true });

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
        const maxRight  = window.innerWidth  - btn.offsetWidth;
        const maxBottom = window.innerHeight - btn.offsetHeight;
        const newRight  = Math.max(0, Math.min(maxRight,  startRight  - dx));
        const newBottom = Math.max(0, Math.min(maxBottom, startBottom - dy));
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

    ta.addEventListener('focus', () => {
      // Cancel any pending blur-triggered cleanup (e.g. focus returned from
      // an overlay button tap).
      clearTimeout(this.#blurTimer);
      if (!this.#visible) return;
      this.#repositionOverlay();
      this.#overlay.style.display = '';
    });

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
        if (this.#activeModifiers.size > 0) {
          // Modifier chord: ignore the entry.shift flag — the overlay modifiers
          // take precedence.  Send the bare scancode inside the modifier wrap.
          this.#sendChord(entry.sc);
        } else if (entry.shift) {
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

    ta.addEventListener('blur', () => {
      // Defer cleanup so that a button tap (which briefly steals focus and then
      // calls ta.focus() to return it) cancels this timer via the focus handler
      // above.  e.relatedTarget is unreliable on mobile so we don't use it.
      this.#blurTimer = setTimeout(() => {
        if (document.activeElement === ta) return; // focus already returned
        this.#overlay.style.display = 'none';
        this.#clearModifiers();
        this.#releaseAll();
      }, 0);
    });
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

  /** Send key-down, applying chord if overlay modifiers are latched. */
  #pressKey(sc, key) {
    if (this.#activeModifiers.size > 0) {
      // Overlay modifiers are latched — send as chord and consume them.
      // Ignore any CHAR_MAP shift requirement; overlay takes precedence.
      this.#sendChord(sc);
      return;
    }
    const entry = CHAR_MAP.get(key);
    if (entry?.shift) this.#sendKey(SHIFT_SC, 1);
    this.#sendKey(sc, 1);
    this.#heldKeys.add(sc);
  }

  /** Send key-up, releasing shift if needed (only when not using chord path). */
  #releaseKey(sc, key) {
    if (!this.#heldKeys.has(sc)) return; // chord path already sent release
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
