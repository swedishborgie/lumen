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
  #els;       // { video, stats, btnConnect, btnDisconnect, statusEl }
  #statsTimer = null;
  #resizeObserver = null;
  #resizeDebounceTimer = null;
  #audioUnlocked = false;

  /**
   * @param {LumenClient} client
   * @param {{ video: HTMLVideoElement,
   *           stats: HTMLElement,
   *           btnConnect: HTMLButtonElement,
   *           btnDisconnect: HTMLButtonElement,
   *           statusEl: HTMLElement,
   *           clipboardInput: HTMLTextAreaElement }} elements
   */
  constructor(client, elements) {
    this.#client = client;
    this.#els    = elements;

    this.#bindClientEvents();
    this.#bindInputEvents();
    this.#bindControlEvents();
    this.#bindClipboardPanel();
    this.#bindResizeObserver();
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

      if (state === 'connected') {
        video.focus();
        video.style.cursor = 'default';
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
        video.style.cursor = 'default';
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
      if (msg.type === 'cursor_update') this.#applyCursor(msg);
      else if (msg.type === 'clipboard_update') this.#applyClipboard(msg);
      else console.log('[lumen] dcmessage unknown type:', msg.type);
    });
  }

  // ── input event bindings ─────────────────────────────────────────────────────

  #bindInputEvents() {
    const { video } = this.#els;

    video.addEventListener('keydown', (e) => {
      this.#tryUnlockAudio();
      const sc = KEY_MAP[e.code];
      if (sc === undefined) return;
      e.preventDefault();
      this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 1 });
    });

    video.addEventListener('keyup', (e) => {
      const sc = KEY_MAP[e.code];
      if (sc === undefined) return;
      e.preventDefault();
      this.#client.sendInput({ type: 'keyboard_key', scancode: sc, state: 0 });
    });

    video.addEventListener('pointermove', (e) => {
      const { x, y } = this.#toCompositorCoords(e.clientX, e.clientY);
      this.#client.sendInput({ type: 'pointer_motion', x, y });
    });

    video.addEventListener('pointerdown', (e) => {
      video.focus();
      video.setPointerCapture(e.pointerId);
      this.#tryUnlockAudio();
      const { x, y } = this.#toCompositorCoords(e.clientX, e.clientY);
      this.#client.sendInput({ type: 'pointer_motion', x, y });
      this.#client.sendInput({ type: 'pointer_button', btn: BTN_CODES[e.button] ?? 272, state: 1 });
      e.preventDefault();
    });

    video.addEventListener('pointerup', (e) => {
      this.#client.sendInput({ type: 'pointer_button', btn: BTN_CODES[e.button] ?? 272, state: 0 });
    });

    video.addEventListener('contextmenu', (e) => e.preventDefault());

    video.addEventListener('wheel', (e) => {
      e.preventDefault();
      this.#client.sendInput({ type: 'pointer_axis', x: e.deltaX / 20, y: e.deltaY / 20 });
    }, { passive: false });
  }

  // ── control button bindings ──────────────────────────────────────────────────

  #bindControlEvents() {
    const { btnConnect, btnDisconnect } = this.#els;
    btnConnect.addEventListener('click',    () => this.#client.connect());
    btnDisconnect.addEventListener('click', () => this.#client.disconnect());
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

  // ── cursor handling ──────────────────────────────────────────────────────────

  /**
   * Apply a cursor_update message from the compositor.
   * Converts raw RGBA to a data-URL via an offscreen canvas and sets
   * the CSS `cursor` property on the video element.
   */
  #applyCursor(msg) {
    const { video } = this.#els;
    switch (msg.kind) {
      case 'default':
        video.style.cursor = 'default';
        break;
      case 'hidden':
        video.style.cursor = 'none';
        break;
      case 'image': {
        const { w, h, hotspot_x, hotspot_y, data } = msg;
        const canvas = document.createElement('canvas');
        canvas.width  = w;
        canvas.height = h;
        const ctx = canvas.getContext('2d');
        // Decode base64 RGBA into Uint8ClampedArray.
        const raw    = atob(data);
        const pixels = new Uint8ClampedArray(raw.length);
        for (let i = 0; i < raw.length; i++) pixels[i] = raw.charCodeAt(i);
        ctx.putImageData(new ImageData(pixels, w, h), 0, 0);
        const url = canvas.toDataURL();
        video.style.cursor = `url(${url}) ${hotspot_x} ${hotspot_y}, auto`;
        break;
      }
    }
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
   * Map video-element client coords → compositor pixel coords,
   * accounting for object-fit:contain letterboxing/pillarboxing.
   */
  #toCompositorCoords(clientX, clientY) {
    const { video } = this.#els;
    const rect = video.getBoundingClientRect();
    const vw = video.videoWidth  || 1920;
    const vh = video.videoHeight || 1080;
    const elAspect  = rect.width / rect.height;
    const vidAspect = vw / vh;
    let offX = 0, offY = 0, drawW = rect.width, drawH = rect.height;
    if (elAspect > vidAspect) {   // pillarbox
      drawW = rect.height * vidAspect;
      offX  = (rect.width - drawW) / 2;
    } else {                       // letterbox
      drawH = rect.width / vidAspect;
      offY  = (rect.height - drawH) / 2;
    }
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
        }, 150);
      }
    });
    this.#resizeObserver.observe(video);
  }
}
