use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use lumen_audio::OpusPacket;
use lumen_compositor::InputEvent;
use lumen_encode::EncodedFrame;
use str0m::{
    change::SdpOffer,
    format::{Codec, PayloadParams},
    media::{Frequency, MediaKind, MediaTime, Mid},
    net::{Protocol, Receive},
    Candidate, Event, Input, IceConnectionState, Output, Rtc,
};

use crate::types::SessionConfig;

/// A single WebRTC peer session backed by `str0m`.
pub struct WebRtcSession {
    rtc: Rtc,
    socket: Arc<UdpSocket>,
    /// Video track mid; populated on first `Event::MediaAdded` for video.
    video_mid: Option<Mid>,
    /// Audio track mid; populated on first `Event::MediaAdded` for audio.
    audio_mid: Option<Mid>,
    input_events: Vec<InputEvent>,
    connected: bool,
    /// Whether a keyframe was requested by the peer since last drain.
    pub keyframe_requested: bool,
}

impl WebRtcSession {
    /// Create a new session from a browser SDP offer.
    /// Returns `(session, answer_sdp_string)`.
    pub async fn new(config: SessionConfig, offer_sdp: &str) -> Result<(Self, String)> {
        let socket = UdpSocket::bind(config.bind_addr)
            .with_context(|| format!("Failed to bind UDP on {}", config.bind_addr))?;
        socket.set_nonblocking(true)?;
        let local_addr = socket.local_addr()?;
        let socket = Arc::new(socket);

        let mut rtc = Rtc::new(Instant::now());

        let candidate = Candidate::host(local_addr, "udp")
            .map_err(|e| anyhow!("ICE candidate error: {:?}", e))?;
        rtc.add_local_candidate(candidate);

        let offer = SdpOffer::from_sdp_string(offer_sdp)
            .map_err(|e| anyhow!("SDP parse error: {:?}", e))?;

        let answer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| anyhow!("accept_offer error: {:?}", e))?;

        let answer_str = answer.to_sdp_string();

        Ok((
            Self {
                rtc,
                socket,
                video_mid: None,
                audio_mid: None,
                input_events: Vec::new(),
                connected: false,
                keyframe_requested: false,
            },
            answer_str,
        ))
    }

    /// Push an encoded H.264 frame to the video RTP track.
    pub fn push_video(&mut self, frame: &EncodedFrame) -> Result<()> {
        let mid = match self.video_mid {
            Some(m) if self.connected => m,
            _ => return Ok(()),
        };
        let pts_90k = frame.pts_ms * 90;
        let rtp_time = MediaTime::new(pts_90k, Frequency::NINETY_KHZ);
        let writer = self.rtc.writer(mid)
            .ok_or_else(|| anyhow!("No video writer for {:?}", mid))?;
        let pt = writer.payload_params()
            .find(|p| matches!(p.spec().codec, Codec::H264))
            .map(PayloadParams::pt)
            .ok_or_else(|| anyhow!("No H264 PT negotiated"))?;
        writer.write(pt, Instant::now(), rtp_time, frame.data.to_vec())
            .map_err(|e| anyhow!("Video write error: {:?}", e))
    }

    /// Push an Opus packet to the audio RTP track.
    pub fn push_audio(&mut self, packet: &OpusPacket) -> Result<()> {
        let mid = match self.audio_mid {
            Some(m) if self.connected => m,
            _ => return Ok(()),
        };
        let pts_48k = packet.pts_samples;
        let rtp_time = MediaTime::new(pts_48k, Frequency::FORTY_EIGHT_KHZ);
        let writer = self.rtc.writer(mid)
            .ok_or_else(|| anyhow!("No audio writer for {:?}", mid))?;
        let pt = writer.payload_params()
            .find(|p| matches!(p.spec().codec, Codec::Opus))
            .map(PayloadParams::pt)
            .ok_or_else(|| anyhow!("No Opus PT negotiated"))?;
        writer.write(pt, Instant::now(), rtp_time, packet.data.to_vec())
            .map_err(|e| anyhow!("Audio write error: {:?}", e))
    }

    /// Drain any [`InputEvent`]s received from the browser via the data channel.
    pub fn drain_input_events(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.input_events)
    }

    /// Add a remote ICE candidate received over the signaling channel.
    pub fn add_remote_candidate(&mut self, candidate_str: &str) -> Result<()> {
        let c = Candidate::from_sdp_string(candidate_str)
            .map_err(|e| anyhow!("Candidate parse error: {:?}", e))?;
        self.rtc.add_remote_candidate(c);
        Ok(())
    }

    /// Drive the str0m I/O state machine once. Must be called in a tight loop.
    pub async fn drive(&mut self) -> Result<SessionState> {
        let now = Instant::now();
        let local_addr = self.socket.local_addr()?;

        // Drain incoming UDP packets (non-blocking).
        let mut buf = [0u8; 2048];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((n, remote_addr)) => {
                    let recv = Receive {
                        proto: Protocol::Udp,
                        source: remote_addr,
                        destination: local_addr,
                        contents: buf[..n].try_into()
                            .map_err(|_| anyhow!("Datagram too small"))?,
                    };
                    let _ = self.rtc.handle_input(Input::Receive(now, recv));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        let _ = self.rtc.handle_input(Input::Timeout(now));

        // Drain str0m output events and transmissions.
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    let _ = self.socket.send_to(&t.contents, t.destination);
                }
                Ok(Output::Event(Event::Connected)) => {
                    tracing::info!("WebRTC peer connected");
                    self.connected = true;
                }
                Ok(Output::Event(Event::MediaAdded(added))) => {
                    match added.kind {
                        MediaKind::Video if self.video_mid.is_none() => {
                            tracing::info!("Video mid: {:?}", added.mid);
                            self.video_mid = Some(added.mid);
                        }
                        MediaKind::Audio if self.audio_mid.is_none() => {
                            tracing::info!("Audio mid: {:?}", added.mid);
                            self.audio_mid = Some(added.mid);
                        }
                        _ => {}
                    }
                }
                Ok(Output::Event(Event::IceConnectionStateChange(IceConnectionState::Disconnected))) => {
                    return Ok(SessionState::Closed);
                }
                Ok(Output::Event(Event::ChannelData(data))) => {
                    if let Ok(ev) = serde_json::from_slice::<InputEvent>(&data.data) {
                        self.input_events.push(ev);
                    }
                }
                Ok(Output::Event(Event::KeyframeRequest(_))) => {
                    self.keyframe_requested = true;
                }
                Ok(Output::Event(_)) => {}
                Ok(Output::Timeout(_)) => break,
                Err(e) => {
                    tracing::debug!("str0m error: {:?}", e);
                    break;
                }
            }
        }

        Ok(SessionState::Active)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Closed,
}
