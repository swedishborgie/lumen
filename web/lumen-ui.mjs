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

export class LumenUI {
  #client;
  #els;         // { video, videoContainer, cursorCanvas, stats, btnConnect, btnDisconnect,
                //   btnFullscreen, statusEl, fullscreenHint, clipboardInput, splash,
                //   displayAuto, displayFixed, displayFixedControls,
                //   displayPreset720p, displayPreset1080p,
                //   displayCustomW, displayCustomH, displayApply }
  #cursor;      // CursorManager
  #input;       // InputHandler
  #gamepad;     // GamepadController
  #resize;      // ResizeManager

  #statsTimer          = null;
  #audioUnlocked       = false;
  #clipboardDebounceTimer = null;
  #prevSnap            = null;

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
   *           stats: HTMLElement,
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

    const { video, videoContainer, cursorCanvas } = elements;

    this.#cursor  = new CursorManager(cursorCanvas, video);
    this.#input   = new InputHandler(video, client, this.#cursor, () => this.#tryUnlockAudio());
    this.#gamepad = new GamepadController(client);
    this.#resize  = new ResizeManager(video, videoContainer, client, this.#cursor);

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
    const { video, statusEl, stats, btnConnect, btnDisconnect } = this.#els;

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
        this.#statsTimer = setInterval(() => this.#updateStats(), 1000);
        // Send the current size immediately so the compositor matches the viewport.
        this.#resize.sendCurrentSize();
      } else if (state === 'idle') {
        if (this.#statsTimer) { clearInterval(this.#statsTimer); this.#statsTimer = null; }
        this.#gamepad.stop();
        this.#prevSnap       = null;
        this.#audioUnlocked  = false;
        stats.textContent    = 'No stats yet';
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
      else console.log('[lumen] dcmessage unknown type:', msg.type);
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
    });

    displayFixed.addEventListener('click', () => {
      setActiveToggle('fixed');
      // Apply the currently-active preset (or 1280×720 as default).
      const [w, h] = LumenUI.#PRESETS['720p'];
      setActivePreset('720p');
      displayCustomW.value = '';
      displayCustomH.value = '';
      applyFixed(w, h);
    });

    displayPreset720p.addEventListener('click', () => {
      const [w, h] = LumenUI.#PRESETS['720p'];
      setActivePreset('720p');
      displayCustomW.value = '';
      displayCustomH.value = '';
      applyFixed(w, h);
    });

    displayPreset1080p.addEventListener('click', () => {
      const [w, h] = LumenUI.#PRESETS['1080p'];
      setActivePreset('1080p');
      displayCustomW.value = '';
      displayCustomH.value = '';
      applyFixed(w, h);
    });

    displayApply.addEventListener('click', () => {
      const w = parseInt(displayCustomW.value, 10);
      const h = parseInt(displayCustomH.value, 10);
      if (!w || !h || w < 2 || h < 2) return;
      setActivePreset(null);
      applyFixed(w, h);
    });

    // Allow pressing Enter in either custom input to apply.
    [displayCustomW, displayCustomH].forEach(el => {
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') displayApply.click();
      });
    });
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
        console.log('[lumen] sending clipboard to compositor, length=%d, preview=%s',
          text.length, JSON.stringify(text.slice(0, 80)));
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
    console.log('[lumen] clipboard_update received, length=%d, preview=%s',
      msg.text.length, JSON.stringify(msg.text.slice(0, 80)));
    this.#els.clipboardInput.value = msg.text;
  }

  // ── stats display ────────────────────────────────────────────────────────────

  async #updateStats() {
    const snap = await this.#client.getStats();
    if (!snap) return;
    const prev = this.#prevSnap;
    this.#prevSnap = snap;

    // All WebRTC stats are cumulative totals; compute per-second deltas vs prev sample.
    const df = (key) => prev != null ? snap[key] - prev[key] : '—';

    const kb      = prev != null ? ((snap.videoBytes - prev.videoBytes) / 1024).toFixed(1) + ' KB/s' : '—';
    const jitter  = snap.jitter != null ? (snap.jitter * 1000).toFixed(1) + ' ms' : '—';
    const rtt     = snap.rtt    != null ? '   RTT: ' + (snap.rtt * 1000).toFixed(1) + ' ms' : '';
    const pktLost = prev != null ? snap.videoLost - prev.videoLost : '—';
    this.#els.stats.textContent = [
      `Video       : ${kb}  |  pkts ${df('videoPackets')}  lost ${pktLost}`,
      `Frames/s    : recv ${df('framesReceived')}  decoded ${df('framesDecoded')}  dropped ${df('framesDropped')}`,
      `Decoder     : ${snap.decoderImpl ?? '—'}`,
      `Jitter      : ${jitter}${rtt}`,
    ].join('\n');
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
