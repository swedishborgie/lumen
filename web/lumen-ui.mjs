/**
 * lumen-ui.mjs — DOM UI layer for Lumen.
 *
 * Binds a LumenClient instance to DOM elements. Orchestrates the sub-modules
 * in ./ui/ and manages the remaining UI concerns: state-driven button/video
 * updates, fullscreen/pointer-lock lifecycle, clipboard sync, stats display,
 * and audio autoplay unlock.
 */

import { CursorManager }     from './ui/cursor.mjs';
import { InputHandler }      from './ui/input.mjs';
import { GamepadController } from './ui/gamepad.mjs';
import { ResizeManager }     from './ui/resize.mjs';
import { PerformanceMonitor } from './lumen-perf.mjs';
import { logger, Level }     from './lumen-debug.mjs';

export class LumenUI {
  #client;
  #els;         // { video, videoContainer, cursorCanvas, perfCanvas, perfToggle,
                //   debugToggle, debugLevel, debugLevelRow,
                //   btnConnect, btnDisconnect,
                //   btnFullscreen, statusEl, fullscreenHint, clipboardInput, splash,
                //   displayAuto, displayFixed, displayFixedControls,
                //   displayPreset720p, displayPreset1080p,
                //   displayCustomW, displayCustomH, displayApply,
                //   gamepadList }
  #cursor;      // CursorManager
  #input;       // InputHandler
  #gamepad;     // GamepadController
  #resize;      // ResizeManager
  #perf;        // PerformanceMonitor

  #audioUnlocked       = false;
  #clipboardDebounceTimer = null;
  #connectedGamepads   = new Map(); // gamepad index → name

  // Keys to capture via the Keyboard Lock API when in fullscreen.
  // Supported only in Chromium-based browsers when fullscreen is active.
  static #LOCKABLE_KEYS = [
    'Escape', 'Tab',
    'MetaLeft', 'MetaRight',
    'AltLeft', 'AltRight',
    'F1','F2','F3','F4','F5','F6','F7','F8','F9','F10','F11','F12',
    'F13','F14','F15','F16','F17','F18','F19','F20','F21','F22','F23','F24',
  ];

  /**
   * @param {import('./lumen-client.mjs').LumenClient} client
   * @param {{ video: HTMLVideoElement,
   *           videoContainer: HTMLElement,
   *           cursorCanvas: HTMLCanvasElement,
   *           perfCanvas: HTMLCanvasElement,
   *           perfToggle: HTMLInputElement,
   *           debugToggle: HTMLInputElement,
   *           debugLevel: HTMLSelectElement,
   *           debugLevelRow: HTMLElement,
   *           btnConnect: HTMLButtonElement,
   *           btnDisconnect: HTMLButtonElement,
   *           btnFullscreen: HTMLButtonElement,
   *           statusEl: HTMLElement,
   *           fullscreenHint: HTMLElement,
   *           clipboardInput: HTMLTextAreaElement,
   *           splash: HTMLElement,
   *           displayAuto: HTMLButtonElement,
   *           displayFixed: HTMLButtonElement,
   *           displayFixedControls: HTMLElement,
   *           displayPreset720p: HTMLButtonElement,
   *           displayPreset1080p: HTMLButtonElement,
   *           displayCustomW: HTMLInputElement,
   *           displayCustomH: HTMLInputElement,
   *           displayApply: HTMLButtonElement }} elements
   */
  constructor(client, elements) {
    this.#client = client;
    this.#els    = elements;

    const { video, videoContainer, cursorCanvas, perfCanvas } = elements;

    this.#cursor  = new CursorManager(cursorCanvas, video);
    this.#input   = new InputHandler(video, client, this.#cursor, () => this.#tryUnlockAudio());
    this.#gamepad = new GamepadController(client, {
      onConnect:    (index, name) => this.#onGamepadConnect(index, name),
      onDisconnect: (index)       => this.#onGamepadDisconnect(index),
    });
    this.#resize  = new ResizeManager(video, videoContainer, client, this.#cursor);
    this.#perf    = new PerformanceMonitor(perfCanvas, client, video);

    this.#cursor.init();
    this.#input.bind();
    this.#gamepad.bind();
    this.#resize.bind();

    this.#bindClientEvents();
    this.#bindControlEvents();
    this.#bindFullscreenEvents();
    this.#bindClipboardPanel();
    this.#bindDisplayMode();
    this.#bindSplashEvents();
    this.#bindPerfToggle();
    this.#bindDebugToggle();
  }

  // ── client event bindings ────────────────────────────────────────────────────

  #bindSplashEvents() {
    const { video, splash } = this.#els;
    if (!splash) return;
    video.addEventListener('playing', () => {
      splash.classList.add('hidden');
    });
  }

  #bindClientEvents() {
    const { video, statusEl, btnConnect, btnDisconnect } = this.#els;

    this.#client.addEventListener('statuschange', (e) => {
      statusEl.textContent = e.detail;
    });

    this.#client.addEventListener('statechange', (e) => {
      const state = e.detail;
      btnConnect.disabled              = state !== 'idle';
      btnDisconnect.disabled           = state === 'idle';
      this.#els.btnFullscreen.disabled = state !== 'connected';

      if (state === 'connected') {
        video.focus();
        if (this.#els.perfToggle?.checked) this.#perf.start();
        // Send the current size immediately so the compositor matches the viewport.
        this.#resize.sendCurrentSize();
        // Re-sync any gamepads that connected before the data channel was open,
        // or that were active during a previous session (reconnect case).
        this.#gamepad.resync();
      } else if (state === 'idle') {
        this.#perf.stop();
        this.#gamepad.stop();
        this.#connectedGamepads.clear();
        this.#updateGamepadList();
        this.#audioUnlocked  = false;
        video.srcObject      = null;
        video.muted          = true;
        // Clear the canvas cursor and reset state.
        this.#cursor.clear();
        // Exit fullscreen/pointer-lock if the session ends.
        if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
        // Show splash screen again.
        if (this.#els.splash) {
          this.#els.splash.classList.remove('hidden');
        }
      }
    });

    this.#client.addEventListener('track', (e) => {
      // Keep video element's srcObject in sync with the client's stream.
      if (video.srcObject !== this.#client.stream) {
        video.srcObject = this.#client.stream;
      }
      // Unmute when the audio track arrives. The video element starts muted to
      // satisfy browser autoplay policy; we defer unmuting until the first user
      // interaction (click or keypress) which provides the required gesture.
      if (e.detail.kind === 'audio') {
        this.#tryUnlockAudio();
      }
    });

    this.#client.addEventListener('dcmessage', (e) => {
      const msg = e.detail;
      if (msg.type === 'cursor_update') this.#cursor.apply(msg).catch(console.warn);
      else if (msg.type === 'clipboard_update') this.#applyClipboard(msg);
      else console.warn('[lumen] dcmessage unknown type:', msg.type);
    });
  }

  // ── control button bindings ──────────────────────────────────────────────────

  #bindControlEvents() {
    const { btnConnect, btnDisconnect } = this.#els;
    btnConnect.addEventListener('click',    () => this.#client.connect());
    btnDisconnect.addEventListener('click', () => this.#client.disconnect());
  }

  // ── fullscreen + pointer lock + keyboard lock ────────────────────────────────

  #bindFullscreenEvents() {
    const { btnFullscreen } = this.#els;
    btnFullscreen.addEventListener('click', () => this.#enterFullscreen());
    document.addEventListener('fullscreenchange',  () => this.#handleFullscreenChange());
    document.addEventListener('pointerlockchange', () => this.#handlePointerLockChange());
    document.addEventListener('pointerlockerror',  () => {
      console.warn('[lumen] pointer lock request failed');
    });
  }

  async #enterFullscreen() {
    const container = this.#els.video.closest('#video-container') ?? this.#els.video;
    try {
      await container.requestFullscreen({ navigationUI: 'hide' });
    } catch (err) {
      console.warn('[lumen] requestFullscreen failed:', err);
    }
  }

  async #handleFullscreenChange() {
    const { video, btnFullscreen, fullscreenHint } = this.#els;
    if (document.fullscreenElement) {
      // Entered fullscreen — request pointer lock then keyboard lock.
      try {
        await video.requestPointerLock({ unadjustedMovement: true });
      } catch {
        // unadjustedMovement not supported in all browsers; fall back.
        video.requestPointerLock();
      }
      // Keyboard Lock: capture OS-level keys (Chromium only, no-op elsewhere).
      await navigator.keyboard?.lock(LumenUI.#LOCKABLE_KEYS).catch(() => {});
      btnFullscreen.textContent = '✕ Exit Fullscreen';
      fullscreenHint.classList.add('visible');
    } else {
      // Exited fullscreen — pointer lock and keyboard lock are released automatically.
      navigator.keyboard?.unlock();
      btnFullscreen.textContent = '⛶ Fullscreen';
      fullscreenHint.classList.remove('visible');
      this.#input.onPointerLockReleased();
      video.focus();
    }
  }

  #handlePointerLockChange() {
    const { video } = this.#els;
    if (document.pointerLockElement === video) {
      this.#input.onPointerLockAcquired(
        video.videoWidth  || 1920,
        video.videoHeight || 1080,
      );
    } else {
      this.#input.onPointerLockReleased();
    }
  }

  // ── display mode controls ────────────────────────────────────────────────────

  /** Supported fixed-size presets as [width, height] CSS-pixel pairs. */
  static #PRESETS = {
    '720p':  [1280,  720],
    '1080p': [1920, 1080],
  };

  #bindDisplayMode() {
    const {
      displayAuto, displayFixed, displayFixedControls,
      displayPreset720p, displayPreset1080p,
      displayCustomW, displayCustomH, displayApply,
    } = this.#els;

    if (!displayAuto) return; // elements not present (graceful degradation)

    const setActiveToggle = (mode) => {
      displayAuto.classList.toggle('active',  mode === 'auto');
      displayFixed.classList.toggle('active', mode === 'fixed');
      displayFixedControls.style.display = mode === 'fixed' ? '' : 'none';
      if (mode === 'auto') {
        document.body.classList.remove('fixed-mode');
      } else {
        document.body.classList.add('fixed-mode');
      }
    };

    const setActivePreset = (key) => {
      displayPreset720p.classList.toggle('active',  key === '720p');
      displayPreset1080p.classList.toggle('active', key === '1080p');
    };

    const applyFixed = (w, h) => {
      const cw = Math.max(2, (Math.round(w) & ~1));
      const ch = Math.max(2, (Math.round(h) & ~1));
      this.#resize.setFixedMode(cw, ch);
    };

    displayAuto.addEventListener('click', () => {
      setActiveToggle('auto');
      setActivePreset(null);
      this.#resize.setAutoMode();
      localStorage.setItem('lumen.displayMode', 'auto');
    });

    displayFixed.addEventListener('click', () => {
      setActiveToggle('fixed');
      // Apply the currently-active preset (or 1280×720 as default).
      const [w, h] = LumenUI.#PRESETS['720p'];
      setActivePreset('720p');
      displayCustomW.value = '';
      displayCustomH.value = '';
      applyFixed(w, h);
      localStorage.setItem('lumen.displayMode',   'fixed');
      localStorage.setItem('lumen.displayPreset', '720p');
      localStorage.removeItem('lumen.displayW');
      localStorage.removeItem('lumen.displayH');
    });

    displayPreset720p.addEventListener('click', () => {
      const [w, h] = LumenUI.#PRESETS['720p'];
      setActivePreset('720p');
      displayCustomW.value = '';
      displayCustomH.value = '';
      applyFixed(w, h);
      localStorage.setItem('lumen.displayMode',   'fixed');
      localStorage.setItem('lumen.displayPreset', '720p');
      localStorage.removeItem('lumen.displayW');
      localStorage.removeItem('lumen.displayH');
    });

    displayPreset1080p.addEventListener('click', () => {
      const [w, h] = LumenUI.#PRESETS['1080p'];
      setActivePreset('1080p');
      displayCustomW.value = '';
      displayCustomH.value = '';
      applyFixed(w, h);
      localStorage.setItem('lumen.displayMode',   'fixed');
      localStorage.setItem('lumen.displayPreset', '1080p');
      localStorage.removeItem('lumen.displayW');
      localStorage.removeItem('lumen.displayH');
    });

    displayApply.addEventListener('click', () => {
      const w = parseInt(displayCustomW.value, 10);
      const h = parseInt(displayCustomH.value, 10);
      if (!w || !h || w < 2 || h < 2) return;
      setActivePreset(null);
      applyFixed(w, h);
      localStorage.setItem('lumen.displayMode',   'fixed');
      localStorage.setItem('lumen.displayPreset', '');
      localStorage.setItem('lumen.displayW', String(w));
      localStorage.setItem('lumen.displayH', String(h));
    });

    // Allow pressing Enter in either custom input to apply.
    [displayCustomW, displayCustomH].forEach(el => {
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') displayApply.click();
      });
    });

    // Restore saved display mode.
    const savedMode   = localStorage.getItem('lumen.displayMode');
    const savedPreset = localStorage.getItem('lumen.displayPreset');
    const savedW      = localStorage.getItem('lumen.displayW');
    const savedH      = localStorage.getItem('lumen.displayH');
    if (savedMode === 'fixed') {
      setActiveToggle('fixed');
      if (savedPreset && LumenUI.#PRESETS[savedPreset]) {
        const [w, h] = LumenUI.#PRESETS[savedPreset];
        setActivePreset(savedPreset);
        applyFixed(w, h);
      } else if (savedW && savedH) {
        const w = parseInt(savedW, 10);
        const h = parseInt(savedH, 10);
        if (w >= 2 && h >= 2) {
          displayCustomW.value = savedW;
          displayCustomH.value = savedH;
          applyFixed(w, h);
        }
      }
    }
  }

  // ── gamepad detection ────────────────────────────────────────────────────────

  #onGamepadConnect(index, name) {
    this.#connectedGamepads.set(index, name);
    this.#updateGamepadList();
  }

  #onGamepadDisconnect(index) {
    this.#connectedGamepads.delete(index);
    this.#updateGamepadList();
  }

  #updateGamepadList() {
    const el = this.#els.gamepadList;
    if (!el) return;
    if (this.#connectedGamepads.size === 0) {
      el.innerHTML = '<span class="gamepad-none">No controllers detected</span>';
      return;
    }
    el.innerHTML = '';
    for (const [, name] of this.#connectedGamepads) {
      const item = document.createElement('div');
      item.className = 'gamepad-item';
      item.textContent = name;
      el.appendChild(item);
    }
  }

  // ── clipboard panel (browser → compositor) ───────────────────────────────────

  #bindClipboardPanel() {
    const { clipboardInput } = this.#els;
    // Auto-send 300 ms after the user stops typing or immediately after paste.
    // Programmatic updates to .value (from #applyClipboard) do not fire 'input',
    // so there is no echo risk.
    clipboardInput.addEventListener('input', () => {
      clearTimeout(this.#clipboardDebounceTimer);
      this.#clipboardDebounceTimer = setTimeout(() => {
        const text = clipboardInput.value;
        if (!text) return;
        this.#client.sendClipboardWrite(text);
      }, 300);
    });
  }

  /**
   * Apply a clipboard_update message from the compositor.
   * Populates the textarea so the user can see and copy the text.
   * Setting .value programmatically does not fire 'input', so there is no
   * echo risk back to the compositor.
   */
  #applyClipboard(msg) {
    if (typeof msg.text !== 'string') return;
    this.#els.clipboardInput.value = msg.text;
  }

  // ── perf toggle ──────────────────────────────────────────────────────────────

  #bindPerfToggle() {
    const { perfToggle, perfCanvas } = this.#els;
    if (!perfToggle || !perfCanvas) return;
    perfToggle.addEventListener('change', () => {
      const on = perfToggle.checked;
      perfCanvas.classList.toggle('visible', on);
      localStorage.setItem('lumen.perfOverlay', on ? '1' : '0');
      const connected = this.#client.state === 'connected';
      if (on && connected) {
        this.#perf.start();
      } else {
        this.#perf.stop();
      }
    });

    // Restore saved state.
    if (localStorage.getItem('lumen.perfOverlay') === '1') {
      perfToggle.checked = true;
      perfCanvas.classList.add('visible');
      // The monitor itself starts on connect (see #bindClientEvents).
    }
  }

  // ── debug logging toggle ──────────────────────────────────────────────────────

  #bindDebugToggle() {
    const { debugToggle, debugLevel, debugLevelRow } = this.#els;
    if (!debugToggle) return;

    const applyLevel = () => {
      if (debugToggle.checked) {
        logger.setLevel(Number(debugLevel?.value ?? Level.INFO));
      } else {
        logger.setLevel(Level.NONE);
      }
    };

    debugToggle.addEventListener('change', () => {
      const on = debugToggle.checked;
      if (debugLevelRow) debugLevelRow.style.display = on ? '' : 'none';
      localStorage.setItem('lumen.debugLogging', on ? '1' : '0');
      applyLevel();
    });

    debugLevel?.addEventListener('change', () => {
      localStorage.setItem('lumen.debugLevel', debugLevel.value);
      applyLevel();
    });

    // Restore saved state.
    const savedLevel = localStorage.getItem('lumen.debugLevel');
    if (savedLevel && debugLevel) debugLevel.value = savedLevel;
    if (localStorage.getItem('lumen.debugLogging') === '1') {
      debugToggle.checked = true;
      if (debugLevelRow) debugLevelRow.style.display = '';
    }
    applyLevel();
  }

  // ── audio unlock ─────────────────────────────────────────────────────────────

  /** Unmute and play on first user gesture; no-op after first call. */
  #tryUnlockAudio() {
    if (this.#audioUnlocked) return;
    const { video } = this.#els;
    video.muted = false;
    video.play().catch(() => {
      // If play() fails (e.g. srcObject not yet set), leave muted — the next
      // gesture will try again.
      video.muted = true;
      this.#audioUnlocked = false;
    });
    this.#audioUnlocked = true;
  }
}
