use std::sync::{atomic::AtomicUsize, Arc};
use bytes::Bytes;

/// Configuration for the PulseAudio capture and Opus encoder.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// PulseAudio source name. `None` auto-detects the monitor of the default
    /// output sink (i.e. captures desktop application audio). Override with an
    /// explicit source such as `"alsa_output.pci-0000_1f_04.analog-stereo.monitor"`
    /// or `"alsa_input.pci-0000_1f_04.analog-stereo"` for microphone capture.
    pub device_name: Option<String>,
    /// Sample rate in Hz (default: 48000).
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u8,
    /// Opus target bitrate in bits/second (default: 128 000).
    pub bitrate_bps: i32,
    /// Opus frame duration in milliseconds (default: 20).
    pub frame_duration_ms: u32,
    /// Use Opus variable bitrate mode.
    pub use_vbr: bool,
    /// Skip encoding silent frames (all-zero PCM detection).
    pub use_silence_gate: bool,
    /// Active peer count. When `Some` and the count is zero, Opus encoding is
    /// skipped (PCM is still drained from PulseAudio to keep the buffer current).
    /// `None` means always encode (default, backward-compatible).
    pub peer_count: Option<Arc<AtomicUsize>>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: 48_000,
            channels: 2,
            bitrate_bps: 128_000,
            frame_duration_ms: 20,
            use_vbr: false,
            use_silence_gate: false,
            peer_count: None,
        }
    }
}

/// A single Opus-encoded audio packet ready for RTP packetization.
#[derive(Debug, Clone)]
pub struct OpusPacket {
    /// Raw Opus packet bytes.
    pub data: Bytes,
    /// Presentation timestamp in samples (at the configured sample rate).
    pub pts_samples: u64,
}
