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
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: u64,
}
