use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;

use lumen_compositor::CapturedFrame;
use crate::codec::VideoCodec;

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
    /// Requested video codec. Hardware encoders support all variants;
    /// the software (x264) encoder only supports `VideoCodec::H264`.
    pub codec: VideoCodec,
    /// CUDA device index for NVENC encoding (e.g. `"0"` for the first GPU).
    ///
    /// Only meaningful when the `nvenc` feature is enabled.  `None` disables
    /// the NVENC probe and falls through to VA-API / x264.  Defaults to `"0"`
    /// so that the first NVIDIA GPU is tried automatically when the feature is
    /// active.
    #[cfg(feature = "nvenc")]
    pub cuda_device: Option<String>,
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
            codec: VideoCodec::H264,
            #[cfg(feature = "nvenc")]
            cuda_device: Some("0".to_string()),
        }
    }
}

/// An encoded video frame (one or more NAL/OBU units in the codec's bitstream format).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Raw compressed bitstream bytes.
    /// - H.264/H.265: Annex-B byte stream (NAL units with 0x00 0x00 0x00 0x01 start codes).
    /// - VP9/AV1: raw codec bitstream bytes.
    pub data: Bytes,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: u64,
    pub is_keyframe: bool,
    /// The codec used to produce this frame.
    pub codec: VideoCodec,
    /// Wall-clock instant at which the source frame was captured by the compositor.
    ///
    /// Must be passed as the `instant` argument to `writer.write()` in `push_video`
    /// so that str0m's RTCP Sender Reports reflect the true capture time rather than
    /// the (later) encode-output time. Using `Instant::now()` at send time instead
    /// would shift the video NTP↔RTP mapping by the encoder's pipeline latency,
    /// causing the browser to desync audio behind video by that amount.
    pub captured_at: Instant,
}

/// Abstraction over hardware and software video encoders.
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
/// render node specified in `config` for the codec in `config.codec`.  Use
/// this before starting the compositor so that the rendering path (GPU DMA-BUF
/// vs CPU RGBA) can be chosen to match the encoder that will actually be used.
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

/// Probe all VA-API codecs that can be successfully initialised on the render
/// node specified in `config`.  Returns a list of supported [`VideoCodec`]
/// variants, always starting with H264 (if available).
///
/// The returned list is used to populate the `/api/config` capabilities.
pub fn probe_supported_vaapi_codecs(config: &EncoderConfig) -> Vec<VideoCodec> {
    if !config
        .render_node
        .as_deref()
        .map(|p| !p.as_os_str().is_empty())
        .unwrap_or(false)
    {
        return vec![];
    }

    let candidates = [VideoCodec::H264, VideoCodec::H265, VideoCodec::Vp9, VideoCodec::Av1];
    candidates
        .iter()
        .filter(|&&codec| {
            let mut probe_cfg = config.clone();
            probe_cfg.codec = codec;
            crate::vaapi::VaapiEncoder::new(&probe_cfg).is_ok()
        })
        .copied()
        .collect()
}

/// Probe whether NVENC hardware encoding is available for the given config.
///
/// Returns `true` if an `NvencEncoder` can be successfully initialised with
/// the CUDA device specified in `config`.  Requires the `nvenc` Cargo feature.
#[cfg(feature = "nvenc")]
pub fn probe_nvenc(config: &EncoderConfig) -> bool {
    config
        .cuda_device
        .as_deref()
        .map(|d| !d.is_empty())
        .unwrap_or(false)
        && crate::nvenc::NvencEncoder::new(config).is_ok()
}

/// Auto-select the best available encoder backend.
///
/// For hardware paths the codec in `config.codec` is attempted first; if
/// unavailable it falls back to H264 (hardware) then x264 (software).
///
/// Probe order (when features are active):
///   1. NVENC  — when `nvenc` feature is enabled and `config.cuda_device` is set.
///   2. VA-API — when `config.render_node` is a non-empty path.
///   3. x264   — always available software fallback (H264 only).
pub fn create_encoder(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>> {
    // NVENC path: only when feature is enabled and a CUDA device is configured.
    #[cfg(feature = "nvenc")]
    if config.cuda_device.as_deref().map(|d| !d.is_empty()).unwrap_or(false) {
        match crate::nvenc::NvencEncoder::new(config) {
            Ok(enc) => {
                tracing::info!("Using NVENC hardware encoder");
                return Ok(Box::new(enc));
            }
            Err(e) => {
                tracing::warn!("NVENC encoder unavailable ({}), trying VA-API / x264", e);
            }
        }
    }

    // VA-API path: only attempt if a non-empty render node path is specified.
    if config.render_node.as_deref().map(|p| !p.as_os_str().is_empty()).unwrap_or(false) {
        match crate::vaapi::VaapiEncoder::new(config) {
            Ok(enc) => {
                tracing::info!("Using VA-API hardware encoder ({})", config.codec);
                return Ok(Box::new(enc));
            }
            Err(e) => {
                tracing::warn!("VA-API encoder unavailable for {} ({})", config.codec, e);
                // If the preferred codec failed and it wasn't H264, retry with H264.
                if config.codec != VideoCodec::H264 {
                    let mut fallback = config.clone();
                    fallback.codec = VideoCodec::H264;
                    match crate::vaapi::VaapiEncoder::new(&fallback) {
                        Ok(enc) => {
                            tracing::info!("Falling back to VA-API H264 encoder");
                            return Ok(Box::new(enc));
                        }
                        Err(e2) => {
                            tracing::warn!("VA-API H264 fallback also unavailable ({}), falling back to x264", e2);
                        }
                    }
                } else {
                    tracing::warn!("Falling back to x264 software encoder");
                }
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
