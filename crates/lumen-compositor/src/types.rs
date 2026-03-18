use std::path::PathBuf;
use std::sync::{atomic::AtomicUsize, Arc};
use std::time::Instant;
use bytes::Bytes;
use smithay::backend::allocator::dmabuf::Dmabuf;

/// Configuration for the Wayland compositor.
#[derive(Debug)]
pub struct CompositorConfig {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Fractional scaling factor (1.0 = no scaling).
    pub scale: f64,
    /// Target capture frame rate.
    pub target_fps: f64,
    /// DRM render node path (e.g. `/dev/dri/renderD128`).
    /// `None` falls back to the software (Pixman) renderer.
    pub render_node: Option<PathBuf>,
    /// Wayland socket name of a nested inner compositor (e.g. `"wayland-inner"`).
    /// When set, a clipboard bridge task connects to that socket using
    /// `zwlr_data_control_manager_v1` to sync clipboard bidirectionally.
    pub inner_display: Option<String>,
    /// Active peer count. When `Some` and the count is zero, frame rendering is
    /// skipped so the compositor idles instead of rendering into the void.
    /// `None` means always render (default, backward-compatible).
    pub peer_count: Option<Arc<AtomicUsize>>,
    /// Optional channel to receive the Wayland socket name once the compositor
    /// has created it. Sent exactly once, immediately after the socket is bound.
    /// Use this to trigger actions (e.g. launching a client) that require the
    /// socket to exist before they start.
    pub socket_name_tx: Option<std::sync::mpsc::SyncSender<String>>,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale: 1.0,
            target_fps: 30.0,
            render_node: None,
            inner_display: None,
            peer_count: None,
            socket_name_tx: None,
        }
    }
}

/// A single captured frame from the compositor.
///
/// Either `rgba_buffer` (CPU path) or `dmabuf` (GPU zero-copy) will be set.
#[derive(Clone)]
pub struct CapturedFrame {
    /// Raw RGBA8888 pixel data (software/CPU path).
    pub rgba_buffer: Option<Bytes>,
    /// DMA-BUF handle for zero-copy GPU→VA-API path.
    pub dmabuf: Option<Dmabuf>,
    /// DRM format modifier for the DMA-BUF (valid when `dmabuf` is `Some`).
    pub drm_modifier: u64,
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp in milliseconds (from compositor monotonic clock).
    pub pts_ms: u64,
    /// Wall-clock instant at which this frame was captured.
    ///
    /// Carried through the encoding pipeline so that `push_video` can pass the
    /// true capture time — not `Instant::now()` — to `writer.write()`. str0m
    /// embeds this in RTCP Sender Reports to establish the NTP↔RTP mapping used
    /// by the browser for A/V synchronisation. If the encode pipeline introduces
    /// latency (e.g. VA-API async buffering), using `Instant::now()` at send time
    /// instead of capture time shifts the video RTCP SR forward by the encode
    /// latency, causing the browser to delay audio by that amount.
    pub captured_at: Instant,
}

/// A clipboard update event emitted by the compositor when the Wayland selection changes.
#[derive(Debug, Clone)]
pub enum ClipboardEvent {
    /// The clipboard now contains the given text.
    Text(String),
    /// The clipboard was cleared (no active selection).
    Cleared,
}

/// A cursor update event emitted by the compositor whenever the pointer image changes.
#[derive(Debug, Clone)]
pub enum CursorEvent {
    /// The compositor default cursor should be shown (CSS "default").
    Default,
    /// A named cursor shape; the inner string is a standard CSS cursor value
    /// (e.g. `"pointer"`, `"text"`, `"crosshair"`).
    Named(String),
    /// The cursor should be hidden.
    Hidden,
    /// A custom cursor image from a Wayland client surface.
    Image {
        width: u32,
        height: u32,
        hotspot_x: i32,
        hotspot_y: i32,
        /// Raw RGBA8888 pixel data, row-major.
        rgba: Bytes,
    },
}
