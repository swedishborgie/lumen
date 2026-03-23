use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use opus::{Application, Channels, Encoder};
use tokio::sync::mpsc;

use crate::pw_sink::PipeWireSink;
use crate::types::{AudioConfig, OpusPacket};

/// PipeWire audio capture + Opus encoder.
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
        let (packet_tx, packet_rx) = mpsc::channel(4);
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
    /// Creates a PipeWire virtual sink stream, initialises an Opus encoder,
    /// then loops reading PCM frames, optionally gating silence, and encoding
    /// to Opus packets delivered over the channel.
    pub fn run(&mut self) -> Result<()> {
        let sample_rate = self.config.sample_rate;
        let channels = self.config.channels;
        let frame_ms = self.config.frame_duration_ms;

        tracing::info!(
            sample_rate,
            channels,
            bitrate_bps = self.config.bitrate_bps,
            frame_ms,
            "AudioCapture starting (PipeWire virtual sink)"
        );

        // ── PipeWire virtual sink ─────────────────────────────────────────
        let sink = PipeWireSink::create(sample_rate, channels)
            .context("Failed to create PipeWire virtual sink")?;

        // ── Opus encoder ──────────────────────────────────────────────────
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

        // Number of samples per channel per Opus frame.
        let frame_size = (sample_rate as u64 * frame_ms as u64 / 1000) as usize;
        let frame_samples = frame_size * channels as usize; // interleaved

        // Accumulation buffer: PipeWire delivers audio at its own quantum
        // size; we collect samples here until we have a full Opus frame.
        let mut pcm_buf: Vec<f32> = Vec::with_capacity(frame_samples * 2);
        let mut output_buf = vec![0u8; 4000]; // max Opus packet size
        let mut pts: u64 = 0;
        let mut last_bitrate = self.bitrate.load(Ordering::Relaxed);
        let mut audio_idle = false;
        let mut prev_peer_count: usize = 0;

        tracing::info!(frame_size, "AudioCapture loop started");

        loop {
            if self.stop_flag.load(Ordering::Relaxed) {
                tracing::info!("AudioCapture stop requested");
                break;
            }

            // Dynamic bitrate update.
            let current_bitrate = self.bitrate.load(Ordering::Relaxed);
            if current_bitrate != last_bitrate {
                if encoder.set_bitrate(opus::Bitrate::Bits(current_bitrate)).is_ok() {
                    tracing::debug!(
                        "Opus bitrate updated: {} → {} bps",
                        last_bitrate,
                        current_bitrate
                    );
                    last_bitrate = current_bitrate;
                }
            }

            // Receive the next batch of samples from PipeWire.
            // Use a short timeout so the stop_flag is checked regularly.
            let frame = match sink.pcm_rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(f) => f,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::warn!("PipeWire PCM channel disconnected");
                    break;
                }
            };

            pcm_buf.extend_from_slice(&frame.samples);

            // Peer-count transition: activate / deactivate the default sink override.
            let current_peers = self
                .peer_count
                .as_ref()
                .map_or(1, |c| c.load(Ordering::Relaxed));
            if prev_peer_count == 0 && current_peers > 0 {
                sink.activate();
            } else if prev_peer_count > 0 && current_peers == 0 {
                sink.deactivate();
            }
            prev_peer_count = current_peers;

            // Encode all complete Opus frames available in the accumulation buffer.
            while pcm_buf.len() >= frame_samples {
                let this_frame: Vec<f32> = pcm_buf.drain(..frame_samples).collect();

                // Silence gate: skip encoding all-zero frames.
                if self.config.use_silence_gate && this_frame.iter().all(|&s| s == 0.0) {
                    pts += frame_size as u64;
                    continue;
                }

                // Skip Opus encoding when no peers are connected; keep consuming
                // PCM to stay current so the first frame after reconnect isn't stale.
                if current_peers == 0 {
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

                // Encode to Opus (F32LE → Opus).
                let encoded_len = encoder
                    .encode_float(&this_frame, &mut output_buf)
                    .context("Opus encode failed")?;

                let packet = OpusPacket {
                    data: Bytes::copy_from_slice(&output_buf[..encoded_len]),
                    pts_samples: pts,
                };
                pts += frame_size as u64;

                if self.packet_tx.try_send(packet).is_err() {
                    tracing::trace!("AudioCapture: packet dropped (receiver lagging)");
                }
            }
        }

        // `sink` is dropped here → PipeWireSink::drop() restores default sink
        // and stops the PipeWire thread.
        tracing::info!("AudioCapture stopped");
        Ok(())
    }
}

