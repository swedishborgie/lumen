//! lumen-encode — video encoder abstraction.
//!
//! Provides a pluggable `VideoEncoder` trait with three backends:
//! - `NvencEncoder`: hardware-accelerated via NVIDIA NVENC, zero-copy DMA-BUF (feature `nvenc`)
//! - `VaapiEncoder`: hardware-accelerated via VA-API (Intel/AMD), zero-copy DMA-BUF
//! - `SoftwareEncoder`: software H.264 via x264 (H264 only)
//!
//! Use `create_encoder()` to auto-select the best available backend.

pub mod codec;
pub mod encoder;
pub mod software;
pub mod vaapi;
pub mod yuv;
#[cfg(feature = "nvenc")]
pub mod nvenc;

pub use codec::VideoCodec;

#[cfg(feature = "nvenc")]
pub use encoder::{create_encoder, probe_nvenc, probe_vaapi, probe_supported_vaapi_codecs, EncoderConfig, EncodedFrame, VideoEncoder};
#[cfg(not(feature = "nvenc"))]
pub use encoder::{create_encoder, probe_vaapi, probe_supported_vaapi_codecs, EncoderConfig, EncodedFrame, VideoEncoder};
