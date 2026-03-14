use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use opus::{Application, Channels, Encoder};
use tokio::sync::mpsc;

use crate::types::{AudioConfig, OpusPacket};

/// PulseAudio capture + Opus encoder.
///
/// Call [`AudioCapture::run`] from a dedicated OS thread or
/// `tokio::task::spawn_blocking`. Encoded packets are delivered via the
/// channel returned from [`AudioCapture::new`].
pub struct AudioCapture {
    config: AudioConfig,
    packet_tx: mpsc::Sender<OpusPacket>,
    stop_flag: Arc<AtomicBool>,
    bitrate: Arc<AtomicI32>,
    peer_count: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

/// A cloneable handle allowing dynamic bitrate updates from any thread.
#[derive(Clone)]
pub struct BitrateHandle(Arc<AtomicI32>);

impl BitrateHandle {
    pub fn set(&self, bps: i32) {
        self.0.store(bps, Ordering::Relaxed);
    }
}

impl AudioCapture {
    /// Create a new `AudioCapture`.
    ///
    /// Returns `(capture, packet_rx)`. Feed `packet_rx` into the WebRTC
    /// session. Call [`AudioCapture::run`] on a blocking thread.
    pub fn new(config: AudioConfig) -> Result<(Self, mpsc::Receiver<OpusPacket>)> {
        let (packet_tx, packet_rx) = mpsc::channel(64);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let bitrate = Arc::new(AtomicI32::new(config.bitrate_bps));
        let peer_count = config.peer_count.clone();
        Ok((Self { config, packet_tx, stop_flag, bitrate, peer_count }, packet_rx))
    }

    /// Returns a handle for updating the encoder bitrate at runtime.
    pub fn bitrate_handle(&self) -> BitrateHandle {
        BitrateHandle(self.bitrate.clone())
    }

    /// Signal the capture loop to stop cleanly.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    /// Blocking capture + encode loop.
    ///
    /// Opens a PulseAudio `RECORD` stream, initialises an Opus encoder,
    /// then loops reading PCM frames, optionally gating silence, and
    /// encoding to Opus packets delivered over the channel.
    pub fn run(&mut self) -> Result<()> {
        let sample_rate = self.config.sample_rate;
        let channels = self.config.channels;
        let frame_ms = self.config.frame_duration_ms;

        tracing::info!(
            device = ?self.config.device_name,
            sample_rate,
            channels,
            bitrate_bps = self.config.bitrate_bps,
            frame_ms,
            "AudioCapture starting",
        );

        // --- PulseAudio simple stream ---
        let pa_spec = Spec {
            format: Format::S16le,
            rate: sample_rate,
            channels,
        };
        if !pa_spec.is_valid() {
            bail!("Invalid PulseAudio sample spec: rate={sample_rate} channels={channels}");
        }

        // Resolve device: use the configured name, or auto-detect the monitor
        // of the default output sink so desktop application audio is captured
        // rather than the microphone (the PA default input source).
        let resolved_device: Option<String> = self.config.device_name.clone()
            .or_else(default_monitor_source);
        if let Some(ref name) = resolved_device {
            tracing::info!("AudioCapture using source: {}", name);
        } else {
            tracing::warn!("AudioCapture: could not detect default monitor source; \
                falling back to PulseAudio default input (likely microphone). \
                Set LUMEN_AUDIO_DEVICE to a .monitor source for desktop audio.");
        }
        let device_cstr = resolved_device.as_deref();
        let pa = Simple::new(
            None,                    // server  — None = local
            "lumen",                 // application name
            Direction::Record,
            device_cstr,             // source  — None = default
            "capture",               // stream description
            &pa_spec,
            None,                    // channel map
            None,                    // buffering attributes
        )
        .context("Failed to open PulseAudio stream")?;

        // --- Opus encoder ---
        let opus_channels = match channels {
            1 => Channels::Mono,
            _ => Channels::Stereo,
        };
        let mut encoder = Encoder::new(sample_rate, opus_channels, Application::LowDelay)
            .context("Failed to create Opus encoder")?;

        encoder
            .set_bitrate(opus::Bitrate::Bits(self.config.bitrate_bps))
            .context("Failed to set Opus bitrate")?;

        encoder
            .set_vbr(self.config.use_vbr)
            .context("Failed to configure Opus VBR")?;

        // Frame size: number of samples per channel per frame.
        // Opus supports 2.5, 5, 10, 20, 40, 60 ms frames.
        let frame_size = (sample_rate as u64 * frame_ms as u64 / 1000) as usize;
        let bytes_per_sample = 2usize; // S16LE
        let frame_bytes = frame_size * channels as usize * bytes_per_sample;

        let mut pcm_buf = vec![0i16; frame_size * channels as usize];
        let mut raw_buf = vec![0u8; frame_bytes];
        let mut output_buf = vec![0u8; 4000]; // max Opus packet size
        let mut pts: u64 = 0;
        let mut last_bitrate = self.bitrate.load(Ordering::Relaxed);
        let mut audio_idle = false;

        tracing::info!("AudioCapture loop started (frame_size={frame_size} samples)");

        loop {
            if self.stop_flag.load(Ordering::Relaxed) {
                tracing::info!("AudioCapture stop requested");
                break;
            }

            // Dynamic bitrate update
            let current_bitrate = self.bitrate.load(Ordering::Relaxed);
            if current_bitrate != last_bitrate {
                if let Ok(()) = encoder.set_bitrate(opus::Bitrate::Bits(current_bitrate)) {
                    tracing::debug!("Opus bitrate updated: {} → {} bps", last_bitrate, current_bitrate);
                    last_bitrate = current_bitrate;
                }
            }

            // Read PCM frame from PulseAudio
            pa.read(&mut raw_buf).context("PulseAudio read failed")?;

            // Convert raw bytes to i16 samples (little-endian)
            for (i, chunk) in raw_buf.chunks_exact(2).enumerate() {
                pcm_buf[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
            }

            // Silence gate: skip encoding if all samples are zero
            if self.config.use_silence_gate && pcm_buf.iter().all(|&s| s == 0) {
                pts += frame_size as u64;
                continue;
            }

            // Skip Opus encoding when no peers are connected; keep draining PA to
            // stay current so the first frame after reconnect isn't stale.
            if self.peer_count.as_ref()
                .map_or(false, |c| c.load(std::sync::atomic::Ordering::Relaxed) == 0)
            {
                if !audio_idle {
                    tracing::debug!("AudioCapture idle: no peers, skipping Opus encode");
                    audio_idle = true;
                }
                pts += frame_size as u64;
                continue;
            }
            if audio_idle {
                tracing::debug!("AudioCapture resuming: peer connected");
                audio_idle = false;
            }

            // Encode to Opus
            let encoded_len = encoder
                .encode(&pcm_buf, &mut output_buf)
                .context("Opus encode failed")?;

            let packet = OpusPacket {
                data: Bytes::copy_from_slice(&output_buf[..encoded_len]),
                pts_samples: pts,
            };

            pts += frame_size as u64;

            // Deliver packet; drop if consumer is lagging (non-blocking try_send)
            if self.packet_tx.try_send(packet).is_err() {
                tracing::trace!("AudioCapture: packet dropped (receiver lagging)");
            }
        }

        tracing::info!("AudioCapture stopped");
        Ok(())
    }
}

/// Query PulseAudio/PipeWire for the default output sink and return its
/// monitor source name (e.g. `"alsa_output.pci-0000_1f_04.analog-stereo.monitor"`).
///
/// This is the correct source for capturing desktop application audio rather
/// than the microphone. Returns `None` if `pactl` is unavailable or fails.
fn default_monitor_source() -> Option<String> {
    let output = std::process::Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sink = String::from_utf8(output.stdout).ok()?;
    let sink = sink.trim();
    if sink.is_empty() {
        return None;
    }
    Some(format!("{sink}.monitor"))
}
