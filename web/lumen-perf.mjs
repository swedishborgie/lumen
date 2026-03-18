/**
 * lumen-perf.mjs — Real-time performance monitor
 *
 * Renders a stack of scrolling time-series graphs onto a <canvas> element
 * showing WebRTC video metrics: bitrate, frame rate, dropped frames, RTT,
 * jitter, packet loss, and average decode time.
 *
 * Usage:
 *   const perf = new PerformanceMonitor(canvas, client);
 *   perf.start();   // on connect
 *   perf.stop();    // on disconnect
 */

// ── Layout constants ──────────────────────────────────────────────────────────

/** Number of 1-second samples to retain (2 minutes). */
const HISTORY = 120;

/** Fixed CSS width of the overlay canvas (matches the CSS width: 220px). */
const OVERLAY_W = 220;

/** Height of the text-header panel in CSS pixels. */
const HEADER_H = 44;

/** Height of each graph panel in CSS pixels. */
const GRAPH_H = 50;

/** Vertical gap between panels in CSS pixels. */
const GAP = 4;

/** Horizontal padding inside the canvas on each side. */
const PAD_X = 6;

/** Vertical padding above and below the graph area within each panel. */
const PAD_Y = 4;

/** Label area height within each graph panel. */
const LABEL_H = 14;

// ── Color palette ─────────────────────────────────────────────────────────────

const C = {
  bg:        '#0a0a0a',
  panelBg:   '#111',
  grid:      'rgba(255,255,255,0.06)',
  label:     '#888',
  value:     '#ccc',
  bitrate:   '#22c55e',   // green
  fps:       '#22d3ee',   // cyan
  fpsGhost:  'rgba(34,211,238,0.35)',
  dropped:   '#ef4444',   // red
  rtt:       '#fbbf24',   // amber
  jitter:    '#fb923c',   // orange
  loss:      '#f87171',   // light red
  decode:    '#a78bfa',   // violet
  zero:      'rgba(255,255,255,0.15)',
  noData:    '#444',
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Circular-buffer push: appends value, drops oldest when full. */
function push(buf, value) {
  buf.push(value);
  if (buf.length > HISTORY) buf.shift();
}

/** Safe delta between two nullable cumulative counters. Returns null if either is null. */
function delta(curr, prev, key) {
  if (curr == null || prev == null) return null;
  const c = curr[key], p = prev[key];
  if (c == null || p == null) return null;
  return Math.max(0, c - p);
}

/** Format a number for display, with unit suffix. */
function fmt(val, unit, decimals = 1) {
  if (val == null || !isFinite(val)) return '—';
  return val.toFixed(decimals) + unit;
}

// ── PerformanceMonitor ────────────────────────────────────────────────────────

export class PerformanceMonitor {
  #canvas;
  #client;
  #ctx;
  #dpr = 1;

  // Data history buffers (one value per second)
  #bitrate    = [];  // KB/s
  #fpsDecoded = [];  // frames/s decoded
  #fpsRecv    = [];  // frames/s received (ghost)
  #dropped    = [];  // dropped frames/s
  #rtt        = [];  // ms (nullable)
  #jitter     = [];  // ms (nullable)
  #pktLoss    = [];  // packets lost/s
  #decodeMs   = [];  // avg ms per decoded frame

  // Latest snapshot for text header
  #latestSnap = null;
  #prevSnap   = null;
  #videoEl    = null;

  #sampleInterval = null;
  #rafId          = null;
  #running        = false;

  /**
   * @param {HTMLCanvasElement} canvas
   * @param {import('./lumen-client.mjs').LumenClient} client
   * @param {HTMLVideoElement} [videoEl]  Optional, used to read resolution.
   */
  constructor(canvas, client, videoEl = null) {
    this.#canvas  = canvas;
    this.#client  = client;
    this.#videoEl = videoEl;
    this.#ctx     = canvas.getContext('2d');
    this.#resize();

    // Re-size when DPR changes (e.g. window moved to different-DPI display).
    window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`)
      .addEventListener('change', () => this.#resize(), { once: true });
  }

  /** Begin sampling and rendering. Call on connection established. */
  start() {
    if (this.#running) return;
    this.#running = true;
    this.#sampleInterval = setInterval(() => this.#sample(), 1000);
    this.#scheduleRender();
  }

  /** Stop sampling and rendering. Call on disconnect. Clears the canvas. */
  stop() {
    this.#running = false;
    clearInterval(this.#sampleInterval);
    this.#sampleInterval = null;
    if (this.#rafId != null) {
      cancelAnimationFrame(this.#rafId);
      this.#rafId = null;
    }
    this.#clearBuffers();
    this.#prevSnap   = null;
    this.#latestSnap = null;
    this.#drawNoData();
  }

  // ── Private ────────────────────────────────────────────────────────────────

  #clearBuffers() {
    this.#bitrate    = [];
    this.#fpsDecoded = [];
    this.#fpsRecv    = [];
    this.#dropped    = [];
    this.#rtt        = [];
    this.#jitter     = [];
    this.#pktLoss    = [];
    this.#decodeMs   = [];
  }

  async #sample() {
    const snap = await this.#client.getStats();
    if (!snap) return;
    const prev = this.#prevSnap;
    this.#prevSnap   = snap;
    this.#latestSnap = snap;
    if (!prev) return;  // Need two samples for deltas.

    // Bitrate (KB/s)
    push(this.#bitrate, (snap.videoBytes - prev.videoBytes) / 1024);

    // Frame rates
    push(this.#fpsDecoded, Math.max(0, snap.framesDecoded  - prev.framesDecoded));
    push(this.#fpsRecv,    Math.max(0, snap.framesReceived - prev.framesReceived));

    // Dropped frames per second
    push(this.#dropped, Math.max(0, snap.framesDropped - prev.framesDropped));

    // RTT & jitter (already in seconds from WebRTC; convert to ms)
    push(this.#rtt,    snap.rtt    != null ? snap.rtt    * 1000 : null);
    push(this.#jitter, snap.jitter != null ? snap.jitter * 1000 : null);

    // Packet loss per second
    push(this.#pktLoss, Math.max(0, snap.videoLost - prev.videoLost));

    // Average decode time per frame (ms)
    const dtDelta = delta(snap, prev, 'totalDecodeTime');
    const fdDelta = delta(snap, prev, 'framesDecoded');
    if (dtDelta != null && fdDelta != null && fdDelta > 0) {
      push(this.#decodeMs, (dtDelta / fdDelta) * 1000);
    } else {
      push(this.#decodeMs, null);
    }
  }

  #scheduleRender() {
    this.#rafId = requestAnimationFrame(() => {
      if (!this.#running) return;
      this.#render();
      this.#scheduleRender();
    });
  }

  #resize() {
    this.#dpr = window.devicePixelRatio || 1;
    const cssW = OVERLAY_W;
    const cssH = HEADER_H + GAP + (GRAPH_H + GAP) * 7;

    this.#canvas.width  = Math.round(cssW * this.#dpr);
    this.#canvas.height = Math.round(cssH * this.#dpr);
    this.#canvas.style.height = cssH + 'px';

    if (!this.#running) this.#drawNoData();
  }

  // ── Rendering ──────────────────────────────────────────────────────────────

  #render() {
    const { canvas: cv, ctx, dpr } = { canvas: this.#canvas, ctx: this.#ctx, dpr: this.#dpr };
    const cssW = OVERLAY_W;

    ctx.save();
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, cssW, cv.height / dpr);

    // Background
    ctx.fillStyle = C.bg;
    ctx.fillRect(0, 0, cssW, cv.height / dpr);

    const snap = this.#latestSnap;

    let y = 0;

    // ── Header panel ──────────────────────────────────────────────────────────
    this.#drawHeader(ctx, cssW, y, snap);
    y += HEADER_H + GAP;

    // ── Graph panels ─────────────────────────────────────────────────────────
    this.#drawGraph(ctx, cssW, y, {
      label: 'Bitrate',
      data:  this.#bitrate,
      color: C.bitrate,
      unit:  ' KB/s',
      floor: 10,
      decimals: 1,
    });
    y += GRAPH_H + GAP;

    this.#drawGraph(ctx, cssW, y, {
      label:     'Frame Rate',
      data:      this.#fpsDecoded,
      color:     C.fps,
      unit:      ' fps',
      floor:     30,
      decimals:  0,
      ghostData: this.#fpsRecv,
      ghostColor: C.fpsGhost,
    });
    y += GRAPH_H + GAP;

    this.#drawGraph(ctx, cssW, y, {
      label:    'Dropped Frames',
      data:     this.#dropped,
      color:    C.dropped,
      unit:     '/s',
      floor:    1,
      decimals: 0,
      barMode:  true,
    });
    y += GRAPH_H + GAP;

    this.#drawGraph(ctx, cssW, y, {
      label:    'RTT',
      data:     this.#rtt,
      color:    C.rtt,
      unit:     ' ms',
      floor:    50,
      decimals: 1,
    });
    y += GRAPH_H + GAP;

    this.#drawGraph(ctx, cssW, y, {
      label:    'Jitter',
      data:     this.#jitter,
      color:    C.jitter,
      unit:     ' ms',
      floor:    10,
      decimals: 2,
    });
    y += GRAPH_H + GAP;

    this.#drawGraph(ctx, cssW, y, {
      label:    'Packet Loss',
      data:     this.#pktLoss,
      color:    C.loss,
      unit:     ' pkts/s',
      floor:    1,
      decimals: 0,
      barMode:  true,
    });
    y += GRAPH_H + GAP;

    this.#drawGraph(ctx, cssW, y, {
      label:    'Decode Time',
      data:     this.#decodeMs,
      color:    C.decode,
      unit:     ' ms/f',
      floor:    5,
      decimals: 2,
    });

    ctx.restore();
  }

  /**
   * Draw the text-only header panel: decoder type, resolution, connection state.
   * @param {CanvasRenderingContext2D} ctx
   * @param {number} w   CSS width
   * @param {number} y   CSS top offset
   * @param {object|null} snap
   */
  #drawHeader(ctx, w, y, snap) {
    ctx.fillStyle = C.panelBg;
    ctx.beginPath();
    ctx.roundRect(PAD_X, y, w - PAD_X * 2, HEADER_H, 4);
    ctx.fill();

    const cx = PAD_X + 8;

    ctx.font      = '10px monospace';
    ctx.fillStyle = C.label;
    ctx.fillText('decoder', cx, y + 14);
    ctx.fillText('resolution', cx, y + 30);
    ctx.fillText('pkts total', cx, y + 46);

    ctx.font      = '11px monospace';
    ctx.fillStyle = C.value;
    ctx.textAlign = 'right';

    const rx = w - PAD_X - 8;

    const decoder = snap?.decoderImpl ?? '—';
    ctx.fillText(decoder, rx, y + 14);

    const vw = this.#videoEl?.videoWidth;
    const vh = this.#videoEl?.videoHeight;
    const res = (vw && vh) ? `${vw}×${vh}` : '—';
    ctx.fillText(res, rx, y + 30);

    // Show total cumulative packets received — direct from snapshot, no delta needed.
    const pktsTotal = snap?.videoPackets != null ? snap.videoPackets.toLocaleString() : '—';
    ctx.fillText(pktsTotal, rx, y + 46);

    ctx.textAlign = 'left';
  }

  /**
   * Draw a single metric graph panel.
   * @param {CanvasRenderingContext2D} ctx
   * @param {number} w          CSS width
   * @param {number} y          CSS top offset
   * @param {object} opts
   * @param {string}   opts.label
   * @param {number[]} opts.data        Primary data buffer (may contain nulls)
   * @param {string}   opts.color       Line/bar color
   * @param {string}   opts.unit        Unit suffix for display value
   * @param {number}   opts.floor       Minimum Y-axis maximum
   * @param {number}   opts.decimals    Decimal places for current-value display
   * @param {boolean}  [opts.barMode]   Draw bars instead of a line
   * @param {number[]} [opts.ghostData] Secondary ghost line data
   * @param {string}   [opts.ghostColor]
   */
  #drawGraph(ctx, w, y, opts) {
    const { label, data, color, unit, floor, decimals, barMode, ghostData, ghostColor } = opts;

    const panelW = w - PAD_X * 2;
    const plotX  = PAD_X;
    const plotY  = y + LABEL_H + PAD_Y;
    const plotW  = panelW;
    const plotH  = GRAPH_H - LABEL_H - PAD_Y * 2;

    // Panel background
    ctx.fillStyle = C.panelBg;
    ctx.beginPath();
    ctx.roundRect(PAD_X, y, panelW, GRAPH_H, 4);
    ctx.fill();

    // Label
    ctx.font      = '10px monospace';
    ctx.fillStyle = C.label;
    ctx.textAlign = 'left';
    ctx.fillText(label, plotX + 6, y + LABEL_H - 2);

    // Current value (last non-null entry)
    const lastVal = [...data].reverse().find(v => v != null) ?? null;
    ctx.font      = '10px monospace';
    ctx.fillStyle = color;
    ctx.textAlign = 'right';
    ctx.fillText(fmt(lastVal, unit, decimals), plotX + plotW - 6, y + LABEL_H - 2);
    ctx.textAlign = 'left';

    // Compute Y-axis max from visible window
    const nonNull = data.filter(v => v != null);
    const yMax = nonNull.length > 0
      ? Math.max(floor, Math.max(...nonNull) * 1.2)
      : floor;

    // Clip to plot area
    ctx.save();
    ctx.beginPath();
    ctx.rect(plotX, plotY, plotW, plotH);
    ctx.clip();

    // Grid lines at 25%, 50%, 75%
    ctx.strokeStyle = C.grid;
    ctx.lineWidth   = 1;
    for (const frac of [0.25, 0.5, 0.75]) {
      const gy = plotY + plotH * (1 - frac);
      ctx.beginPath();
      ctx.moveTo(plotX, gy);
      ctx.lineTo(plotX + plotW, gy);
      ctx.stroke();
    }

    // Zero line
    ctx.strokeStyle = C.zero;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(plotX, plotY + plotH);
    ctx.lineTo(plotX + plotW, plotY + plotH);
    ctx.stroke();
    ctx.setLineDash([]);

    if (data.length > 1) {
      // Ghost line (e.g. framesReceived behind framesDecoded)
      if (ghostData && ghostData.length > 1) {
        this.#plotLine(ctx, ghostData, plotX, plotY, plotW, plotH, yMax, ghostColor, 1);
      }

      if (barMode) {
        this.#plotBars(ctx, data, plotX, plotY, plotW, plotH, yMax, color);
      } else {
        // Filled area under line
        this.#plotArea(ctx, data, plotX, plotY, plotW, plotH, yMax, color);
        this.#plotLine(ctx, data, plotX, plotY, plotW, plotH, yMax, color, 1.5);
      }
    } else {
      // Not enough data yet
      ctx.fillStyle = C.noData;
      ctx.font      = '9px monospace';
      ctx.textAlign = 'center';
      ctx.fillText('collecting…', plotX + plotW / 2, plotY + plotH / 2 + 4);
      ctx.textAlign = 'left';
    }

    ctx.restore();
  }

  /**
   * Plot a filled translucent area under a line.
   */
  #plotArea(ctx, data, x, y, w, h, yMax, color) {
    const n = data.length;
    const step = w / Math.max(HISTORY - 1, 1);
    // Offset so newest sample is at right edge regardless of buffer fill.
    const startX = x + w - (n - 1) * step;

    ctx.beginPath();
    let started = false;
    for (let i = 0; i < n; i++) {
      const v = data[i];
      if (v == null) { started = false; continue; }
      const px = startX + i * step;
      const py = y + h * (1 - Math.min(v / yMax, 1));
      if (!started) { ctx.moveTo(px, py); started = true; }
      else ctx.lineTo(px, py);
    }
    // Close the area down to the baseline.
    if (started) {
      const lastIdx  = data.reduceRight((acc, v, i) => acc === -1 && v != null ? i : acc, -1);
      const firstIdx = data.findIndex(v => v != null);
      if (lastIdx >= 0 && firstIdx >= 0) {
        ctx.lineTo(startX + lastIdx  * step, y + h);
        ctx.lineTo(startX + firstIdx * step, y + h);
        ctx.closePath();
      }
    }
    ctx.fillStyle = color.replace(')', ', 0.15)').replace('rgb', 'rgba');
    // Fallback: if not rgb format, use low-opacity hex approach.
    ctx.globalAlpha = 0.18;
    ctx.fill();
    ctx.globalAlpha = 1;
  }

  /**
   * Plot a polyline through the data, skipping null gaps.
   */
  #plotLine(ctx, data, x, y, w, h, yMax, color, lineWidth) {
    const n = data.length;
    const step   = w / Math.max(HISTORY - 1, 1);
    const startX = x + w - (n - 1) * step;

    ctx.strokeStyle = color;
    ctx.lineWidth   = lineWidth;
    ctx.lineJoin    = 'round';

    let pen = false;
    ctx.beginPath();
    for (let i = 0; i < n; i++) {
      const v = data[i];
      if (v == null) { pen = false; continue; }
      const px = startX + i * step;
      const py = y + h * (1 - Math.min(v / yMax, 1));
      if (!pen) { ctx.moveTo(px, py); pen = true; }
      else       ctx.lineTo(px, py);
    }
    ctx.stroke();
  }

  /**
   * Plot bars (for dropped frames, packet loss).
   */
  #plotBars(ctx, data, x, y, w, h, yMax, color) {
    const n     = data.length;
    const step  = w / Math.max(HISTORY - 1, 1);
    const barW  = Math.max(1, step - 1);
    const startX = x + w - (n - 1) * step;

    ctx.fillStyle = color;
    for (let i = 0; i < n; i++) {
      const v = data[i];
      if (v == null || v === 0) continue;
      const barH = Math.max(1, h * Math.min(v / yMax, 1));
      const px   = startX + i * step - barW / 2;
      ctx.fillRect(px, y + h - barH, barW, barH);
    }
  }

  /** Draw the "no data / disconnected" placeholder. */
  #drawNoData() {
    if (!this.#ctx) return;
    const dpr  = this.#dpr;
    const cssW = OVERLAY_W;
    const cssH = this.#canvas.height / dpr;

    this.#ctx.save();
    this.#ctx.scale(dpr, dpr);
    this.#ctx.clearRect(0, 0, cssW, cssH);
    this.#ctx.fillStyle = C.bg;
    this.#ctx.fillRect(0, 0, cssW, cssH);

    this.#ctx.font      = '12px monospace';
    this.#ctx.fillStyle = C.noData;
    this.#ctx.textAlign = 'center';
    this.#ctx.fillText('Not connected', cssW / 2, cssH / 2);
    this.#ctx.textAlign = 'left';
    this.#ctx.restore();
  }
}
