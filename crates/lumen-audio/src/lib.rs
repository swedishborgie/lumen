//! lumen-audio — PipeWire audio capture and Opus encoding.
//!
//! Creates a native PipeWire virtual sink that appears as an audio output
//! device in the system.  Audio routed to this sink is captured directly
//! (no monitor), encoded to Opus packets, and delivered via a Tokio channel.

pub mod capture;
pub mod types;
mod pw_sink;

pub use capture::{AudioCapture, BitrateHandle};
pub use types::{AudioConfig, OpusPacket};
