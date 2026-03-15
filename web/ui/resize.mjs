/**
 * resize.mjs — ResizeManager for Lumen.
 *
 * Watches the video element for size changes and synchronises the compositor
 * output dimensions, debouncing rapid resize events.
 */

export class ResizeManager {
  #videoEl;
  #client;
  #cursor;
  #observer      = null;
  #debounceTimer = null;

  /**
   * @param {HTMLVideoElement} videoEl
   * @param {import('../lumen-client.mjs').LumenClient} client
   * @param {import('./cursor.mjs').CursorManager} cursor
   */
  constructor(videoEl, client, cursor) {
    this.#videoEl = videoEl;
    this.#client  = client;
    this.#cursor  = cursor;
  }

  /** Start observing the video element for resize changes. */
  bind() {
    this.#observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const rect = entry.contentRect;
        clearTimeout(this.#debounceTimer);
        this.#debounceTimer = setTimeout(() => {
          const w = Math.round(rect.width  * devicePixelRatio) & ~1;
          const h = Math.round(rect.height * devicePixelRatio) & ~1;
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

  /** Send the current video element size to the compositor immediately. */
  sendCurrentSize() {
    const rect = this.#videoEl.getBoundingClientRect();
    const w = Math.round(rect.width  * devicePixelRatio) & ~1;
    const h = Math.round(rect.height * devicePixelRatio) & ~1;
    if (w > 0 && h > 0) {
      this.#client.sendResize(w, h);
    }
  }
}
