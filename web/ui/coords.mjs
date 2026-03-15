/**
 * coords.mjs — Coordinate mapping utilities for Lumen.
 *
 * Pure functions that convert between browser CSS pixel space and compositor
 * pixel space, accounting for object-fit:contain letterboxing/pillarboxing.
 */

/**
 * Compute the display geometry for a video element.
 *
 * Returns everything needed for coordinate mapping: scale factors, virtual
 * dimensions, draw dimensions, letterbox/pillarbox offsets, and the bounding
 * rect.
 *
 * @param {HTMLVideoElement} videoEl
 * @returns {{ scaleX: number, scaleY: number,
 *             vw: number, vh: number,
 *             drawW: number, drawH: number,
 *             offX: number, offY: number,
 *             rect: DOMRect }}
 */
export function getDisplayScale(videoEl) {
  const rect      = videoEl.getBoundingClientRect();
  const vw        = videoEl.videoWidth  || 1920;
  const vh        = videoEl.videoHeight || 1080;
  const elAspect  = rect.width / rect.height;
  const vidAspect = vw / vh;
  let drawW = rect.width, drawH = rect.height;
  if (elAspect > vidAspect) {
    drawW = rect.height * vidAspect;   // pillarbox
  } else {
    drawH = rect.width / vidAspect;    // letterbox
  }
  const scaleX = vw / drawW;
  const scaleY = vh / drawH;
  const offX   = (rect.width  - drawW) / 2;
  const offY   = (rect.height - drawH) / 2;
  return { scaleX, scaleY, vw, vh, drawW, drawH, offX, offY, rect };
}

/**
 * Map browser client coordinates → compositor pixel coordinates.
 * Clamps the result to [0, vw-1] × [0, vh-1].
 *
 * @param {HTMLVideoElement} videoEl
 * @param {number} clientX
 * @param {number} clientY
 * @returns {{ x: number, y: number }}
 */
export function toCompositorCoords(videoEl, clientX, clientY) {
  const { vw, vh, drawW, drawH, offX, offY, rect } = getDisplayScale(videoEl);
  return {
    x: Math.max(0, Math.min(vw - 1, ((clientX - rect.left - offX) / drawW) * vw)),
    y: Math.max(0, Math.min(vh - 1, ((clientY - rect.top  - offY) / drawH) * vh)),
  };
}

/**
 * Back-project compositor pixel coordinates → canvas CSS pixel coordinates.
 * Inverse of toCompositorCoords.
 *
 * @param {HTMLVideoElement} videoEl
 * @param {number} cx  Compositor X in pixels.
 * @param {number} cy  Compositor Y in pixels.
 * @returns {{ x: number, y: number }}
 */
export function compositorToDisplayCoords(videoEl, cx, cy) {
  const { vw, vh, drawW, drawH, offX, offY } = getDisplayScale(videoEl);
  return {
    x: (cx / vw) * drawW + offX,
    y: (cy / vh) * drawH + offY,
  };
}
