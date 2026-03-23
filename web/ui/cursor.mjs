/**
 * cursor.mjs — CursorManager for Lumen.
 *
 * Owns the cursor canvas, cursor image state, and all drawing logic.
 * Receives compositor cursor_update messages via apply() and position
 * updates via moveTo().
 */

import { compositorToDisplayCoords } from './coords.mjs';

export class CursorManager {
  #canvas;
  #videoEl;
  #ctx      = null;
  #kind     = 'default';   // 'default' | 'hidden' | 'image'
  #img      = null;        // ImageBitmap for 'image' kind
  #hotX     = 0;
  #hotY     = 0;
  #displayX = 0;           // cursor position in canvas CSS pixels
  #displayY = 0;

  /**
   * @param {HTMLCanvasElement} canvas
   * @param {HTMLVideoElement}  videoEl
   */
  constructor(canvas, videoEl) {
    this.#canvas  = canvas;
    this.#videoEl = videoEl;
  }

  /** Size the canvas to match the video element's CSS display size. */
  init() {
    this.#resizeCanvas();
  }

  /** Resize the canvas pixel buffer to the video element's current CSS display size. */
  resize() {
    this.#resizeCanvas();
  }

  /** Clear the canvas entirely (called when disconnected). */
  clear() {
    this.#ctx?.clearRect(0, 0, this.#canvas.width, this.#canvas.height);
  }

  /**
   * Move the cursor to the given CSS pixel position within the canvas and redraw.
   * @param {number} x  CSS pixels from canvas left edge.
   * @param {number} y  CSS pixels from canvas top edge.
   */
  moveTo(x, y) {
    this.#displayX = x;
    this.#displayY = y;
    this.#draw();
  }

  /**
   * Apply a cursor_update message from the compositor.
   * Decodes the cursor image (if any) and redraws the canvas.
   *
   * @param {{ kind: string, css?: string, w?: number, h?: number,
   *           hotspot_x?: number, hotspot_y?: number, data?: string }} msg
   */
  async apply(msg) {
    console.debug('[cursor] apply:', JSON.stringify(msg).slice(0, 120));
    switch (msg.kind) {
      case 'default':
        this.#kind = 'default';
        this.#img  = null;
        this.#canvas.style.cursor = '';
        this.#setContainerCursor('none'); // canvas draws arrow; hide native cursor
        console.debug('[cursor] -> default arrow (canvas draw)');
        break;
      case 'named':
        this.#kind = 'named';
        this.#img  = null;
        this.#canvas.style.cursor = msg.css || 'default';
        this.#setContainerCursor(msg.css || 'default');
        console.debug('[cursor] -> named css:', msg.css);
        break;
      case 'hidden':
        this.#kind = 'hidden';
        this.#img  = null;
        this.#canvas.style.cursor = 'none';
        this.#setContainerCursor('none');
        console.debug('[cursor] -> hidden');
        break;
      case 'image': {
        const { w, h, hotspot_x, hotspot_y, data } = msg;
        this.#hotX = hotspot_x;
        this.#hotY = hotspot_y;
        // Decode base64 RGBA → ImageBitmap for efficient repeated drawing.
        const raw    = atob(data);
        const pixels = new Uint8ClampedArray(raw.length);
        for (let i = 0; i < raw.length; i++) pixels[i] = raw.charCodeAt(i);
        this.#img  = await createImageBitmap(new ImageData(pixels, w, h));
        this.#kind = 'image';
        this.#canvas.style.cursor = 'none';
        this.#setContainerCursor('none'); // canvas draws custom image; hide native cursor
        console.debug(`[cursor] -> image ${w}x${h} hotspot=(${hotspot_x},${hotspot_y})`);
        break;
      }
      default:
        console.warn('[cursor] unknown kind:', msg.kind);
    }
    this.#draw();
  }

  // ── private helpers ──────────────────────────────────────────────────────────

  /** Apply a CSS cursor value to the video container element.
   *  The cursor canvas has pointer-events:none so the browser resolves cursor
   *  style from the container div.  Chrome on macOS also ignores cursor:none
   *  on <video> elements, making this the reliable place to set it. */
  #setContainerCursor(css) {
    const el = this.#videoEl.parentElement ?? this.#videoEl;
    el.style.cursor = css;
  }

  #resizeCanvas() {
    const rect = this.#videoEl.getBoundingClientRect();
    const dpr  = devicePixelRatio || 1;
    this.#canvas.width  = Math.round(rect.width  * dpr);
    this.#canvas.height = Math.round(rect.height * dpr);
    const ctx = this.#canvas.getContext('2d');
    ctx.scale(dpr, dpr);
    this.#ctx = ctx;
    this.#draw();
  }

  #draw() {
    const ctx = this.#ctx;
    if (!ctx) return;
    ctx.clearRect(0, 0, this.#canvas.width, this.#canvas.height);
    if (this.#kind === 'hidden' || this.#kind === 'named') return;
    if (this.#kind === 'image' && this.#img) {
      ctx.drawImage(this.#img, this.#displayX - this.#hotX, this.#displayY - this.#hotY);
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
}
