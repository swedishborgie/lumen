use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;

use lumen_compositor::CapturedFrame;

/// Configuration for the video encoder.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Average/target bitrate in kbps (VBR mode).
    pub bitrate_kbps: u32,
    /// Peak bitrate cap in kbps (VBR mode). Must be ≥ `bitrate_kbps`.
    /// Defaults to `bitrate_kbps * 2`.
    pub max_bitrate_kbps: u32,
    /// VA-API DRM render node. `None` triggers auto-detection.
    /// Use `Some(PathBuf::from(""))` to force software encoding.
    pub render_node: Option<PathBuf>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            bitrate_kbps: 4000,
            max_bitrate_kbps: 8000,
            render_node: None,
        }
    }
}

/// An encoded H.264 frame (one or more NAL units in Annex-B format).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Raw H.264 Annex-B byte stream (NAL units with 0x00 0x00 0x00 0x01 start codes).
    pub data: Bytes,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: u64,
    pub is_keyframe: bool,
    /// Wall-clock instant at which the source frame was captured by the compositor.
    ///
    /// Must be passed as the `instant` argument to `writer.write()` in `push_video`
    /// so that str0m's RTCP Sender Reports reflect the true capture time rather than
    /// the (later) encode-output time. Using `Instant::now()` at send time instead
    /// would shift the video NTP↔RTP mapping by the encoder's pipeline latency,
    /// causing the browser to desync audio behind video by that amount.
    pub captured_at: Instant,
}

/// Abstraction over hardware and software H.264 encoders.
pub trait VideoEncoder: Send {
    /// Encode a captured frame. Returns `None` when the encoder is buffering.
    fn encode(&mut self, frame: CapturedFrame) -> Result<Option<EncodedFrame>>;
    /// Request that the next output frame be an IDR (keyframe).
    fn request_keyframe(&mut self);
    /// Update the average target bitrate at runtime (VBR mode).
    /// The peak cap scales proportionally with the average.
    fn update_bitrate(&mut self, kbps: u32);
    /// Reinitialize the encoder for a new frame size.
    /// Forces a keyframe on the next encode call.
    fn resize(&mut self, width: u32, height: u32) -> Result<()>;
}

/// Probe whether VA-API hardware encoding is available for the given config.
///
/// Returns `true` if a `VaapiEncoder` can be successfully initialised on the
/// render node specified in `config`.  Use this before starting the compositor
/// so that the rendering path (GPU DMA-BUF vs CPU RGBA) can be chosen to match
/// the encoder that will actually be used.
pub fn probe_vaapi(config: &EncoderConfig) -> bool {
    if config
        .render_node
        .as_deref()
        .map(|p| !p.as_os_str().is_empty())
        .unwrap_or(false)
    {
        crate::vaapi::VaapiEncoder::new(config).is_ok()
    } else {
        false
    }
}

/// Auto-select the best available encoder backend.
///
/// Probes VA-API on `config.render_node` when a path is set; otherwise uses
/// the software x264 encoder.  The VA-API path is currently a compile-time
/// feature stub — set `render_node` to `None` or `Some("")` to force software.
pub fn create_encoder(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>> {
    // VA-API path: only attempt if a non-empty render node path is specified.
    if config.render_node.as_deref().map(|p| !p.as_os_str().is_empty()).unwrap_or(false) {
        match crate::vaapi::VaapiEncoder::new(config) {
            Ok(enc) => {
                tracing::info!("Using VA-API hardware encoder");
                return Ok(Box::new(enc));
            }
            Err(e) => {
                tracing::warn!("VA-API encoder unavailable ({}), falling back to x264", e);
            }
        }
    }

    tracing::info!("Using software x264 encoder");
    let enc = crate::software::SoftwareEncoder::new(
        config.width,
        config.height,
        config.fps,
        config.bitrate_kbps,
        config.max_bitrate_kbps,
    )?;
    Ok(Box::new(enc))
}
