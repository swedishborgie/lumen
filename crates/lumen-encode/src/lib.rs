//! lumen-encode — H.264 video encoder abstraction.
//!
//! Provides a pluggable `VideoEncoder` trait with two backends:
//! - `VaapiEncoder`: hardware-accelerated via VA-API (Intel/AMD), zero-copy DMA-BUF
//! - `SoftwareEncoder`: software H.264 via x264
//!
//! Use `create_encoder()` to auto-select the best available backend.

pub mod encoder;
pub mod software;
pub mod vaapi;
pub mod yuv;

pub use encoder::{create_encoder, EncoderConfig, EncodedFrame, VideoEncoder};
