/**
 * resize.mjs — ResizeManager for Lumen.
 *
 * Watches the video element for size changes and synchronises the compositor
 * output dimensions, debouncing rapid resize events.
 *
 * Supports two display modes:
 *   - auto  (default): compositor tracks the browser viewport size.
 *   - fixed: compositor is locked to an explicit CSS-pixel resolution; the
 *            container is sized to exactly that resolution (1:1 CSS pixel
 *            scale, no DPR multiplication) and centred in the viewport.
 */

export class ResizeManager {
  #videoEl;
  #containerEl;
  #client;
  #cursor;
  #observer       = null;
  #debounceTimer  = null;
  #mode           = 'auto';  // 'auto' | 'fixed'
  #fixedW         = 0;
  #fixedH         = 0;
  #uiScaleAware   = false;

  /**
   * @param {HTMLVideoElement}   videoEl
   * @param {HTMLElement}        containerEl  The #video-container element.
   * @param {import('../lumen-client.mjs').LumenClient} client
   * @param {import('./cursor.mjs').CursorManager} cursor
   */
  constructor(videoEl, containerEl, client, cursor) {
    this.#videoEl     = videoEl;
    this.#containerEl = containerEl;
    this.#client      = client;
    this.#cursor      = cursor;
  }

  /** Current mode: 'auto' | 'fixed'. */
  get mode() { return this.#mode; }

  /**
   * Convert a CSS-pixel dimension to the compositor pixel count, respecting
   * the current UI-scale-aware setting.
   * @param {number} cssPx
   * @returns {number}
   */
  #toCompositorPx(cssPx) {
    return this.#uiScaleAware
      ? Math.round(cssPx)
      : Math.round(cssPx * devicePixelRatio);
  }

  /** Start observing the video element for resize changes. */
  bind() {
    this.#observer = new ResizeObserver((entries) => {
      if (this.#mode !== 'auto') return;
      for (const entry of entries) {
        const rect = entry.contentRect;
        clearTimeout(this.#debounceTimer);
        this.#debounceTimer = setTimeout(() => {
          const w = this.#toCompositorPx(rect.width)  & ~1;
          const h = this.#toCompositorPx(rect.height) & ~1;
          if (w > 0 && h > 0) {
            this.#client.sendResize(w, h);
          }
          this.#cursor.resize();
        }, 150);
      }
    });
    this.#observer.observe(this.#videoEl);
  }

  /** Stop observing. */
  unbind() {
    clearTimeout(this.#debounceTimer);
    this.#observer?.disconnect();
    this.#observer = null;
  }

  /**
   * Switch to auto mode: compositor tracks the browser viewport.
   * Clears any fixed inline size from the container.
   */
  setAutoMode() {
    this.#mode = 'auto';
    this.#containerEl.style.width  = '';
    this.#containerEl.style.height = '';
    this.sendCurrentSize();
    this.#cursor.resize();
  }

  /**
   * Switch to fixed mode: compositor is locked to w×h CSS pixels (1:1 scale).
   * The container is sized explicitly; DPR is intentionally NOT applied so that
   * each compositor pixel maps to exactly one CSS pixel.
   *
   * @param {number} w  Width in CSS pixels (must be positive and even).
   * @param {number} h  Height in CSS pixels (must be positive and even).
   */
  setFixedMode(w, h) {
    this.#mode   = 'fixed';
    this.#fixedW = w & ~1;
    this.#fixedH = h & ~1;
    this.#containerEl.style.width  = `${this.#fixedW}px`;
    this.#containerEl.style.height = `${this.#fixedH}px`;
    this.#client.sendResize(this.#fixedW, this.#fixedH);
    this.#cursor.resize();
  }

  /**
   * Enable or disable UI-scale-aware resizing in auto mode.
   * When enabled, the compositor is resized to logical (CSS-pixel) dimensions
   * rather than physical pixel dimensions, proportionally matching the
   * browser's UI scale (devicePixelRatio). Has no effect in fixed mode.
   *
   * @param {boolean} enabled
   */
  setUiScaleAware(enabled) {
    this.#uiScaleAware = enabled;
    if (this.#mode === 'auto') {
      this.sendCurrentSize();
    }
  }

  /** Send the current size to the compositor, respecting the active mode. */
  sendCurrentSize() {
    if (this.#mode === 'fixed') {
      if (this.#fixedW > 0 && this.#fixedH > 0) {
        this.#client.sendResize(this.#fixedW, this.#fixedH);
      }
      return;
    }
    const rect = this.#videoEl.getBoundingClientRect();
    const w = this.#toCompositorPx(rect.width)  & ~1;
    const h = this.#toCompositorPx(rect.height) & ~1;
    if (w > 0 && h > 0) {
      this.#client.sendResize(w, h);
    }
  }
}
