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
import { TouchHandler }      from './ui/touch.mjs';
import { FloatingKeyboard }  from './ui/keyboard-button.mjs';
import { PerformanceMonitor } from './lumen-perf.mjs';
import { logger, Level }     from './lumen-debug.mjs';
import { compositorToDisplayCoords } from './ui/coords.mjs';

export class LumenUI {
  #client;
  #els;         // { video, videoContainer, cursorCanvas, perfCanvas, perfToggle,
                //   debugToggle, debugLevel, debugLevelRow,
                //   btnConnect, btnDisconnect,
                //   btnFullscreen, statusEl, fullscreenHint, clipboardInput,
                //   splash, splashStatus,
                //   displayAuto, displayFixed, displayFixedControls,
                //   displayPreset720p, displayPreset1080p,
                //   displayCustomW, displayCustomH, displayApply,
                //   gamepadList }
  #cursor;      // CursorManager
  #input;       // InputHandler
  #gamepad;     // GamepadController
  #resize;      // ResizeManager
  #perf;        // PerformanceMonitor
  #touch;       // TouchHandler | null (only on touch-capable devices)
  #keyboard;    // FloatingKeyboard | null (only on touch-capable devices)

  #audioUnlocked       = false;
  #clipboardDebounceTimer = null;
  #connectedGamepads   = new Map(); // gamepad index → name

  // Auto-reconnect state.
  #intentionalDisconnect = false; // true when the user clicked Disconnect
  #reconnectAttempt      = 0;     // resets to 0 on successful connect
  #reconnectTimer        = null;  // setTimeout handle for next reconnect
  #reconnectCountdown    = null;  // setInterval handle for countdown display

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
   *           splashStatus: HTMLElement,
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

    // Touch and keyboard support — only on devices that support touch input.
    if ('ontouchstart' in window) {
      this.#input.setTouchActive(true);
      this.#touch = new TouchHandler(
        videoContainer,
        video,
        client,
        () => this.#input.getMousePos(),
        (x, y) => {
          this.#input.setMousePos(x, y);
          const dp = compositorToDisplayCoords(video, x, y);
          this.#cursor.moveTo(dp.x, dp.y);
        },
      );
      this.#touch.bind();
      this.#keyboard = new FloatingKeyboard(client);
    } else {
      this.#touch    = null;
      this.#keyboard = null;
    }

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
      if (this.#els.splashStatus) this.#els.splashStatus.textContent = e.detail;
    });

    this.#client.addEventListener('statechange', (e) => {
      const state = e.detail;
      btnConnect.disabled              = state !== 'idle';
      btnDisconnect.disabled           = state === 'idle';
      this.#els.btnFullscreen.disabled = state !== 'connected';

      if (state === 'connected') {
        this.#cancelReconnect();
        this.#reconnectAttempt = 0;
        video.focus();
        if (this.#els.perfToggle?.checked) this.#perf.start();
        // Send the current size immediately so the compositor matches the viewport.
        this.#resize.sendCurrentSize();
        // Re-sync any gamepads that connected before the data channel was open,
        // or that were active during a previous session (reconnect case).
        this.#gamepad.resync();
        this.#keyboard?.show();
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
        if (this.#intentionalDisconnect) {
          this.#intentionalDisconnect = false;
          this.#reconnectAttempt = 0;
        } else {
          this.#scheduleReconnect();
        }
        this.#keyboard?.hide();
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
    btnConnect.addEventListener('click', () => {
      this.#cancelReconnect();
      this.#reconnectAttempt = 0;
      this.#client.connect();
    });
    btnDisconnect.addEventListener('click', () => {
      this.#intentionalDisconnect = true;
      this.#cancelReconnect();
      this.#client.disconnect();
    });
  }

  // ── auto-reconnect ────────────────────────────────────────────────────────────

  /** Schedule the next reconnect attempt with exponential backoff (capped at 30s). */
  #scheduleReconnect() {
    const attempt = ++this.#reconnectAttempt;
    const delay   = Math.min(1000 * (2 ** (attempt - 1)), 30_000);
    let remaining = Math.ceil(delay / 1000);

    const updateCountdown = () => {
      this.#setStatusAll(`Reconnecting in ${remaining}s\u2026`);
    };
    updateCountdown();

    this.#reconnectCountdown = setInterval(() => {
      remaining--;
      if (remaining > 0) {
        updateCountdown();
      } else {
        clearInterval(this.#reconnectCountdown);
        this.#reconnectCountdown = null;
      }
    }, 1000);

    this.#reconnectTimer = setTimeout(async () => {
      this.#reconnectTimer = null;
      clearInterval(this.#reconnectCountdown);
      this.#reconnectCountdown = null;
      // Re-probe immediately before connecting.  The initial probe at the top
      // of this method may have silently swallowed a network error (server was
      // unreachable), so by the time the timer fires the server may be back up
      // but the session is expired.  This second probe catches that case.
      if (await this.#probeAuth()) {
        this.#client.connect();
      }
    }, delay);

    // Probe immediately to surface auth failures early — avoids waiting the
    // full countdown before detecting an expired session.  If the server is
    // unreachable right now the probe is silently ignored and the pre-connect
    // probe above handles it once the server comes back.
    this.#probeAuth();
  }

  /**
   * Fetch /api/config to check whether the session is still authenticated.
   * Returns false and triggers a page reload when the session has expired so
   * the OAuth2 flow can re-authenticate; returns true otherwise (including
   * when the server is unreachable, so the normal reconnect loop handles it).
   *
   * @returns {Promise<boolean>} true → proceed with connect, false → reloading
   */
  async #probeAuth() {
    try {
      // Use redirect:'manual' so the browser does not follow the OAuth2 redirect
      // to the auth server (which would be cross-origin and trigger a CORS error).
      // A redirect response comes back as type='opaqueredirect' with status 0.
      // cache:'no-store' prevents a stale 200 from masking an expired session.
      const res = await fetch('/api/config', {
        credentials: 'include',
        redirect: 'manual',
        cache: 'no-store',
      });
      if (res.status === 401 || res.status === 403 || res.type === 'opaqueredirect') {
        this.#cancelReconnect();
        this.#setStatusAll('Session expired \u2014 reloading\u2026');
        location.reload();
        return false;
      }
    } catch {
      // Network is down or server unreachable — normal reconnect will handle it.
    }
    return true;
  }

  /** Cancel any pending reconnect timer and countdown. */
  #cancelReconnect() {
    clearTimeout(this.#reconnectTimer);
    clearInterval(this.#reconnectCountdown);
    this.#reconnectTimer     = null;
    this.#reconnectCountdown = null;
  }

  /** Update both the tray status element and the splash status element. */
  #setStatusAll(msg) {
    if (this.#els.statusEl)    this.#els.statusEl.textContent    = msg;
    if (this.#els.splashStatus) this.#els.splashStatus.textContent = msg;
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
