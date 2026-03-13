//! lumen-audio — PulseAudio capture and Opus encoding.
//!
//! Rewrite of the pcmflux C++ core in pure Rust.
//! Captures system audio from a PulseAudio monitor source,
//! encodes it to Opus packets, and delivers them via a Tokio channel.

pub mod capture;
pub mod types;

pub use capture::{AudioCapture, BitrateHandle};
pub use types::{AudioConfig, OpusPacket};
