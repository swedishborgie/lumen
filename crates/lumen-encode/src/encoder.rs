use std::path::PathBuf;

use anyhow::Result;
use bytes::Bytes;

use lumen_compositor::CapturedFrame;

/// Configuration for the video encoder.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Target bitrate in kbps (CBR mode).
    pub bitrate_kbps: u32,
    /// CRF quality value for software encoder (0–51; lower = better).
    pub crf: i32,
    /// Use constant bitrate mode (recommended for WebRTC).
    pub cbr: bool,
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
            crf: 23,
            cbr: true,
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
}

/// Abstraction over hardware and software H.264 encoders.
pub trait VideoEncoder: Send {
    /// Encode a captured frame. Returns `None` when the encoder is buffering.
    fn encode(&mut self, frame: CapturedFrame) -> Result<Option<EncodedFrame>>;
    /// Request that the next output frame be an IDR (keyframe).
    fn request_keyframe(&mut self);
    /// Update the target bitrate at runtime (CBR mode).
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
        config.crf,
        config.cbr,
    )?;
    Ok(Box::new(enc))
}
