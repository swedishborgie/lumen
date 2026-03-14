/**
 * lumen-ui.js — DOM UI layer for Lumen.
 *
 * Binds a LumenClient instance to DOM elements.  Manages:
 *   - video element (srcObject, focus, cursor)
 *   - keyboard / mouse / wheel input forwarding
 *   - connect / disconnect buttons
 *   - status text
 *   - periodic stats display
 */

import { LumenClient, KEY_MAP, BTN_CODES } from './lumen-client.js';

export class LumenUI {
  #client;
  #els;       // { video, cursorCanvas, stats, btnConnect, btnDisconnect, btnFullscreen, statusEl, fullscreenHint }
  #statsTimer = null;
  #resizeObserver = null;
  #resizeDebounceTimer = null;
  #audioUnlocked = false;
  // Fullscreen / pointer-lock state
  #pointerLocked = false;
  #vMouseX = 0;   // virtual cursor position in compositor pixel space
  #vMouseY = 0;
  // Canvas cursor state
  #cursorCtx    = null;
  #cursorKind   = 'default';   // 'default' | 'hidden' | 'image'
  #cursorImg    = null;        // ImageBitmap for 'image' kind
  #cursorHotX   = 0;
  #cursorHotY   = 0;
  #displayX     = 0;           // cursor position in canvas CSS pixels
  #displayY     = 0;

  /**
   * @param {LumenClient} client
   * @param {{ video: HTMLVideoElement,
   *           cursorCanvas: HTMLCanvasElement,
   *           stats: HTMLElement,
   *           btnConnect: HTMLButtonElement,
   *           btnDisconnect: HTMLButtonElement,
   *           btnFullscreen: HTMLButtonElement,
   *           statusEl: HTMLElement,
   *           fullscreenHint: HTMLElement,
   *           clipboardInput: HTMLTextAreaElement }} elements
   */
  constructor(client, elements) {
    this.#client = client;
    this.#els    = elements;

    this.#bindClientEvents();
    this.#bindInputEvents();
    this.#bindControlEvents();
    this.#bindFullscreenEvents();
    this.#bindClipboardPanel();
    this.#bindResizeObserver();
    this.#initCursorCanvas();
  }

  // ── client event bindings ────────────────────────────────────────────────────

  #bindClientEvents() {
    const { video, statusEl, stats, btnConnect, btnDisconnect } = this.#els;

    this.#client.addEventListener('statuschange', (e) => {
      statusEl.textContent = e.detail;
    });

    this.#client.addEventListener('statechange', (e) => {
      const state = e.detail;
      btnConnect.disabled    = state !== 'idle';
      btnDisconnect.disabled = state === 'idle';
      this.#els.btnFullscreen.disabled = state !== 'connected';

      if (state === 'connected') {
        video.focus();
        this.#statsTimer = setInterval(() => this.#updateStats(), 1000);
        // Send the current size immediately so the compositor matches the viewport.
        this.#sendCurrentSize();
      } else if (state === 'idle') {
        if (this.#statsTimer) { clearInterval(this.#statsTimer); this.#statsTimer = null; }
        this.#prevSnap = null;
        this.#audioUnlocked = false;
        stats.textContent  = 'No stats yet';
        video.srcObject    = null;
        video.muted        = true;
        // Clear the canvas cursor and reset state.
        this.#cursorKind = 'default';
        this.#cursorImg  = null;
        this.#clearCursorCanvas();
        // Exit fullscreen/pointer-lock if the session ends.
        if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
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
      if (msg.type === 'cursor_update') this.#applyCursor(msg).catch(console.warn);
      else if (msg.type === 'clipboard_update') this.#applyClipboard(msg);
      else console.log('[lumen] dcmessage unknown type:', msg.type);
    });
  }

  // ── input event bindings ─────────────────────────────────────────────────────

  #bindInputEvents() {
    const { video } = this.#els;
    // Pointer events are attached to the container div (parent of video and canvas)
    // rather than the video element itself.  The canvas overlay (even with
    // pointer-events:none) can intercept pointerdown in some browsers; the
    // container always receives events regardless of internal stacking order.
    const pointerTarget = video.parentElement ?? video;

    // Keyboard events stay on the video element — it holds focus.
    video.addEventListener('keydown', (e) => {
      e.preventDefault();
      this.#tryUnlockAudio();
      const sc = KEY_MAP[e.code];
      if (sc === undefined) return;
      this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 1 });
    });

    video.addEventListener('keyup', (e) => {
      e.preventDefault();
      const sc = KEY_MAP[e.code];
      if (sc === undefined) return;
      this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 0 });
    });

    pointerTarget.addEventListener('pointermove', (e) => {
      if (this.#pointerLocked) {
        // Relative motion: accumulate into virtual cursor position.
        const { scaleX, scaleY, vw, vh } = this.#getDisplayScale();
        this.#vMouseX = Math.max(0, Math.min(vw - 1, this.#vMouseX + e.movementX * scaleX));
        this.#vMouseY = Math.max(0, Math.min(vh - 1, this.#vMouseY + e.movementY * scaleY));
        this.#client.sendInput({ type: 'pointer_motion', x: this.#vMouseX, y: this.#vMouseY });
        const dp = this.#compositorToDisplayCoords(this.#vMouseX, this.#vMouseY);
        this.#displayX = dp.x;
        this.#displayY = dp.y;
      } else {
        const { x, y } = this.#toCompositorCoords(e.clientX, e.clientY);
        this.#client.sendInput({ type: 'pointer_motion', x, y });
        const rect = this.#els.video.getBoundingClientRect();
        this.#displayX = e.clientX - rect.left;
        this.#displayY = e.clientY - rect.top;
      }
      this.#drawCursor();
    });

    pointerTarget.addEventListener('pointerdown', (e) => {
      console.log('[lumen] pointerdown', { button: e.button, pointerId: e.pointerId, target: e.target?.tagName, dcState: this.#client.dcReadyState });
      e.preventDefault();
      video.focus();
      try { video.setPointerCapture(e.pointerId); } catch (err) { console.warn('[lumen] setPointerCapture failed:', err.message); }
      this.#tryUnlockAudio();
      const btn = BTN_CODES[e.button];
      console.log('[lumen] btn lookup:', { eButton: e.button, btn });
      if (btn === undefined) { console.warn('[lumen] dropping unknown button', e.button); return; }
      if (!this.#pointerLocked) {
        const { x, y } = this.#toCompositorCoords(e.clientX, e.clientY);
        console.log('[lumen] sending pointer_motion', { x, y });
        this.#client.sendInput({ type: 'pointer_motion', x, y });
      }
      console.log('[lumen] sending pointer_button', { btn, state: 1 });
      this.#client.sendInput({ type: 'pointer_button', btn, state: 1 });
    });

    pointerTarget.addEventListener('pointerup', (e) => {
      const btn = BTN_CODES[e.button];
      if (btn === undefined) return;   // drop unknown buttons
      console.log('[lumen] sending pointer_button', { btn, state: 0 });
      this.#client.sendInput({ type: 'pointer_button', btn, state: 0 });
    });

    pointerTarget.addEventListener('contextmenu', (e) => e.preventDefault());

    pointerTarget.addEventListener('wheel', (e) => {
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

      this.#client.sendInput({
        type: 'pointer_axis',
        x: deltaX, y: deltaY,
        source, v120_x, v120_y,
      });
    }, { passive: false });
  }

  // ── control button bindings ──────────────────────────────────────────────────

  #bindControlEvents() {
    const { btnConnect, btnDisconnect } = this.#els;
    btnConnect.addEventListener('click',    () => this.#client.connect());
    btnDisconnect.addEventListener('click', () => this.#client.disconnect());
  }

  // ── fullscreen + pointer lock + keyboard lock ────────────────────────────────

  // Keys to capture via the Keyboard Lock API when in fullscreen.
  // Supported only in Chromium-based browsers when fullscreen is active.
  static #LOCKABLE_KEYS = [
    'Escape', 'Tab',
    'MetaLeft', 'MetaRight',
    'AltLeft', 'AltRight',
    'F1','F2','F3','F4','F5','F6','F7','F8','F9','F10','F11','F12',
    'F13','F14','F15','F16','F17','F18','F19','F20','F21','F22','F23','F24',
  ];

  #bindFullscreenEvents() {
    const { btnFullscreen, video } = this.#els;

    btnFullscreen.addEventListener('click', () => this.#enterFullscreen());

    document.addEventListener('fullscreenchange', () => this.#handleFullscreenChange());

    document.addEventListener('pointerlockchange', () => this.#handlePointerLockChange());
    document.addEventListener('pointerlockerror', () => {
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
      this.#pointerLocked = false;
      video.focus();
    }
  }

  #handlePointerLockChange() {
    const { video } = this.#els;
    if (document.pointerLockElement === video) {
      this.#pointerLocked = true;
      // Initialise virtual cursor at the centre of the compositor output.
      const vw = video.videoWidth  || 1920;
      const vh = video.videoHeight || 1080;
      this.#vMouseX = vw / 2;
      this.#vMouseY = vh / 2;
      const dp = this.#compositorToDisplayCoords(this.#vMouseX, this.#vMouseY);
      this.#displayX = dp.x;
      this.#displayY = dp.y;
      this.#drawCursor();
    } else {
      this.#pointerLocked = false;
    }
  }

  // ── clipboard panel (browser → compositor) ───────────────────────────────────

  #clipboardDebounceTimer = null;

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

  // ── stats display ────────────────────────────────────────────────────────────

  #prevSnap = null;

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
    const fRecv   = df('framesReceived');
    const fDec    = df('framesDecoded');
    const fDrop   = df('framesDropped');
    const pktLost = prev != null ? snap.videoLost - prev.videoLost : '—';
    this.#els.stats.textContent = [
      `Video       : ${kb}  |  pkts ${df('videoPackets')}  lost ${pktLost}`,
      `Frames/s    : recv ${fRecv}  decoded ${fDec}  dropped ${fDrop}`,
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

  // ── cursor canvas ─────────────────────────────────────────────────────────────

  /** Set up the cursor canvas and do an initial size sync. */
  #initCursorCanvas() {
    this.#resizeCursorCanvas();
  }

  /** Resize the canvas pixel buffer to match the video element's CSS display size. */
  #resizeCursorCanvas() {
    const { cursorCanvas, video } = this.#els;
    const rect = video.getBoundingClientRect();
    const dpr  = devicePixelRatio || 1;
    cursorCanvas.width  = Math.round(rect.width  * dpr);
    cursorCanvas.height = Math.round(rect.height * dpr);
    const ctx = cursorCanvas.getContext('2d');
    ctx.scale(dpr, dpr);
    this.#cursorCtx = ctx;
    this.#drawCursor();
  }

  /** Clear the canvas entirely (used when disconnected). */
  #clearCursorCanvas() {
    const { cursorCanvas } = this.#els;
    this.#cursorCtx?.clearRect(0, 0, cursorCanvas.width, cursorCanvas.height);
  }

  /**
   * Redraw the cursor on the canvas at the current (#displayX, #displayY) position.
   * Called after every pointer move and cursor update.
   */
  #drawCursor() {
    const { cursorCanvas } = this.#els;
    const ctx = this.#cursorCtx;
    if (!ctx) return;
    ctx.clearRect(0, 0, cursorCanvas.width, cursorCanvas.height);
    if (this.#cursorKind === 'hidden') return;
    if (this.#cursorKind === 'image' && this.#cursorImg) {
      ctx.drawImage(
        this.#cursorImg,
        this.#displayX - this.#cursorHotX,
        this.#displayY - this.#cursorHotY,
      );
    } else {
      this.#drawDefaultArrow(ctx, this.#displayX, this.#displayY);
    }
  }

  /** Draw a classic arrow cursor with white fill and black outline. */
  #drawDefaultArrow(ctx, x, y) {
    ctx.save();
    ctx.translate(x, y);
    ctx.beginPath();
    // Arrow outline (clockwise, tip at origin pointing up-left)
    ctx.moveTo(0,    0);
    ctx.lineTo(0,    14);
    ctx.lineTo(3.5,  10.5);
    ctx.lineTo(6,    16);
    ctx.lineTo(8,    15);
    ctx.lineTo(5.5,  9.5);
    ctx.lineTo(10,   9.5);
    ctx.closePath();
    ctx.fillStyle   = 'white';
    ctx.strokeStyle = 'black';
    ctx.lineWidth   = 1.2;
    ctx.lineJoin    = 'round';
    ctx.stroke();
    ctx.fill();
    ctx.restore();
  }

  /**
   * Apply a cursor_update message from the compositor.
   * Decodes the cursor image (if any) and redraws the canvas.
   */
  async #applyCursor(msg) {
    switch (msg.kind) {
      case 'default':
        this.#cursorKind = 'default';
        this.#cursorImg  = null;
        break;
      case 'hidden':
        this.#cursorKind = 'hidden';
        this.#cursorImg  = null;
        break;
      case 'image': {
        const { w, h, hotspot_x, hotspot_y, data } = msg;
        this.#cursorHotX = hotspot_x;
        this.#cursorHotY = hotspot_y;
        // Decode base64 RGBA → ImageBitmap for efficient repeated drawing.
        const raw    = atob(data);
        const pixels = new Uint8ClampedArray(raw.length);
        for (let i = 0; i < raw.length; i++) pixels[i] = raw.charCodeAt(i);
        this.#cursorImg  = await createImageBitmap(new ImageData(pixels, w, h));
        this.#cursorKind = 'image';
        break;
      }
    }
    this.#drawCursor();
  }

  // ── clipboard handling ───────────────────────────────────────────────────────

  /**
   * Apply a clipboard_update message from the compositor.
   * Populates the clipboard panel textarea so the user can see and copy the text.
   * Note: setting .value programmatically does not fire 'input', so this will
   * not echo back to the compositor.
   */
  #applyClipboard(msg) {
    if (typeof msg.text !== 'string') return;
    console.log('[lumen] clipboard_update received, length=%d, preview=%s',
      msg.text.length, JSON.stringify(msg.text.slice(0, 80)));
    this.#els.clipboardInput.value = msg.text;
  }

  // ── coordinate mapping ────────────────────────────────────────────────────────

  /**
   * Compute the scale factors from CSS pixels → compositor pixels, accounting
   * for object-fit:contain letterboxing/pillarboxing.  Used both by
   * #toCompositorCoords (absolute) and the pointer-lock motion handler (relative).
   */
  #getDisplayScale() {
    const { video } = this.#els;
    const rect = video.getBoundingClientRect();
    const vw = video.videoWidth  || 1920;
    const vh = video.videoHeight || 1080;
    const elAspect  = rect.width / rect.height;
    const vidAspect = vw / vh;
    let drawW = rect.width, drawH = rect.height;
    if (elAspect > vidAspect) {
      drawW = rect.height * vidAspect;   // pillarbox
    } else {
      drawH = rect.width / vidAspect;    // letterbox
    }
    return { scaleX: vw / drawW, scaleY: vh / drawH, vw, vh };
  }

  /**
   * Back-project compositor pixel coords → canvas CSS pixel coords.
   * Inverse of #toCompositorCoords.
   */
  #compositorToDisplayCoords(cx, cy) {
    const { video } = this.#els;
    const rect = video.getBoundingClientRect();
    const { scaleX, scaleY, vw, vh } = this.#getDisplayScale();
    const drawW = vw / scaleX;
    const drawH = vh / scaleY;
    const offX  = (rect.width  - drawW) / 2;
    const offY  = (rect.height - drawH) / 2;
    return {
      x: (cx / vw) * drawW + offX,
      y: (cy / vh) * drawH + offY,
    };
  }

  /**
   * Map video-element client coords → compositor pixel coords,
   * accounting for object-fit:contain letterboxing/pillarboxing.
   */
  #toCompositorCoords(clientX, clientY) {
    const { video } = this.#els;
    const rect = video.getBoundingClientRect();
    const { scaleX, scaleY, vw, vh } = this.#getDisplayScale();
    const drawW = vw / scaleX;
    const drawH = vh / scaleY;
    const offX  = (rect.width  - drawW) / 2;
    const offY  = (rect.height - drawH) / 2;
    return {
      x: Math.max(0, Math.min(vw - 1, ((clientX - rect.left - offX) / drawW) * vw)),
      y: Math.max(0, Math.min(vh - 1, ((clientY - rect.top  - offY) / drawH) * vh)),
    };
  }

  // ── resize observer ──────────────────────────────────────────────────────────

  #sendCurrentSize() {
    const { video } = this.#els;
    const rect = video.getBoundingClientRect();
    const w = Math.round(rect.width  * devicePixelRatio) & ~1;
    const h = Math.round(rect.height * devicePixelRatio) & ~1;
    if (w > 0 && h > 0) {
      this.#client.sendResize(w, h);
    }
  }

  #bindResizeObserver() {
    const { video } = this.#els;
    this.#resizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const rect = entry.contentRect;
        clearTimeout(this.#resizeDebounceTimer);
        this.#resizeDebounceTimer = setTimeout(() => {
          const w = Math.round(rect.width  * devicePixelRatio) & ~1;
          const h = Math.round(rect.height * devicePixelRatio) & ~1;
          if (w > 0 && h > 0) {
            this.#client.sendResize(w, h);
          }
          this.#resizeCursorCanvas();
        }, 150);
      }
    });
    this.#resizeObserver.observe(video);
  }
}
