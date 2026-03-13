use std::path::PathBuf;
use bytes::Bytes;
use smithay::backend::allocator::dmabuf::Dmabuf;

/// Configuration for the Wayland compositor.
#[derive(Debug, Clone)]
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
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale: 1.0,
            target_fps: 30.0,
            render_node: None,
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
    /// Presentation timestamp in milliseconds.
    pub pts_ms: u64,
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
    /// The compositor default cursor should be shown.
    Default,
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
