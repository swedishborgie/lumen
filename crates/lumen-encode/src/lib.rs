//! lumen-encode — H.264 video encoder abstraction.
//!
//! Provides a pluggable `VideoEncoder` trait with three backends:
//! - `NvencEncoder`: hardware-accelerated via NVIDIA NVENC, zero-copy DMA-BUF (feature `nvenc`)
//! - `VaapiEncoder`: hardware-accelerated via VA-API (Intel/AMD), zero-copy DMA-BUF
//! - `SoftwareEncoder`: software H.264 via x264
//!
//! Use `create_encoder()` to auto-select the best available backend.

pub mod encoder;
pub mod software;
pub mod vaapi;
pub mod yuv;
#[cfg(feature = "nvenc")]
pub mod nvenc;

#[cfg(feature = "nvenc")]
pub use encoder::{create_encoder, probe_nvenc, probe_vaapi, EncoderConfig, EncodedFrame, VideoEncoder};
#[cfg(not(feature = "nvenc"))]
pub use encoder::{create_encoder, probe_vaapi, EncoderConfig, EncodedFrame, VideoEncoder};
