//! Server-side metrics streamed to the browser performance overlay.
//!
//! `ServerMetrics` is serialized as JSON and sent over the signaling WebSocket
//! to any connected browser that has the performance overlay open.  The data
//! is published into `tokio::sync::watch` channels so the encoder hot-path
//! never blocks or allocates on the write side.

use serde::Serialize;

/// Per-frame metrics collected by the encoder loop.
///
/// All timing values are in microseconds so they can be represented as
/// integers without precision loss.  Cumulative counters (`frames_encoded`,
/// `dropped_frames`) are reset to zero when the encoder restarts.
#[derive(Clone, Default, Serialize, Debug)]
pub struct EncoderMetrics {
    /// Unix timestamp in milliseconds when this snapshot was taken.
    pub timestamp_ms: u64,
    /// Cumulative number of frames successfully encoded since startup.
    pub frames_encoded: u64,
    /// Time taken by the last `encoder.encode()` call, in microseconds.
    pub encode_time_us: u64,
    /// Time between compositor frame capture and encoder receipt, in microseconds.
    /// Elevated values indicate GPU fence stalls or compositor backpressure.
    pub capture_latency_us: u64,
    /// Compressed size of the last encoded frame in bytes.
    pub frame_size_bytes: usize,
    /// Whether the last encoded frame was a keyframe (IDR).
    pub keyframe: bool,
    /// Cumulative number of frames dropped due to broadcast channel lag.
    pub dropped_frames: u64,
    /// Current encoder width in pixels.
    pub width: u32,
    /// Current encoder height in pixels.
    pub height: u32,
}

/// System-level metrics sampled by a background task every second.
#[derive(Clone, Default, Serialize, Debug)]
pub struct SystemMetrics {
    /// All-core average CPU utilization, 0–100.  `None` if unavailable.
    pub cpu_usage_pct: Option<f32>,
    /// RSS memory currently in use by the process, in mebibytes.  `None` if unavailable.
    pub mem_used_mb: Option<u32>,
    /// Total system memory in mebibytes.  `None` if unavailable.
    pub mem_total_mb: Option<u32>,
}

/// Combined snapshot sent to the browser as a single `metrics` WebSocket message.
#[derive(Clone, Default, Serialize, Debug)]
pub struct ServerMetrics {
    pub encoder: EncoderMetrics,
    pub system: SystemMetrics,
}
