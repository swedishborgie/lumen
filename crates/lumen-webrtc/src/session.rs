use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use lumen_audio::OpusPacket;
use lumen_compositor::InputEvent;
use lumen_encode::EncodedFrame;
use str0m::{
    change::SdpOffer,
    channel::ChannelId,
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
    /// The real IP:port addresses of our local ICE candidates (no 0.0.0.0).
    local_candidates: Vec<std::net::SocketAddr>,
    /// Video track mid; populated on first `Event::MediaAdded` for video.
    video_mid: Option<Mid>,
    /// Audio track mid; populated on first `Event::MediaAdded` for audio.
    audio_mid: Option<Mid>,
    input_events: Vec<InputEvent>,
    /// Outbound data channel messages queued for sending.
    pending_dc_out: VecDeque<Vec<u8>>,
    /// The data channel ID, populated when the browser opens the channel.
    dc_channel_id: Option<ChannelId>,
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
        let port = socket.local_addr()?.port();
        let socket = Arc::new(socket);

        // Discover the real outbound IP by connecting a probe socket — this
        // never sends any data but causes the OS to select a source address.
        let outbound_ip = {
            let probe = UdpSocket::bind("0.0.0.0:0")?;
            probe.connect("8.8.8.8:80")?;
            probe.local_addr()?.ip()
        };

        let mut rtc = Rtc::new(Instant::now());

        let loopback: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let loopback_addr = std::net::SocketAddr::new(loopback, port);
        let outbound_addr = std::net::SocketAddr::new(outbound_ip, port);

        let mut local_candidates = Vec::new();

        // Add loopback candidate for same-machine (localhost) connections.
        if let Ok(c) = Candidate::host(loopback_addr, "udp") {
            rtc.add_local_candidate(c);
            local_candidates.push(loopback_addr);
        }
        // Add LAN candidate for remote connections.
        if outbound_ip != loopback {
            if let Ok(c) = Candidate::host(outbound_addr, "udp") {
                rtc.add_local_candidate(c);
                local_candidates.push(outbound_addr);
            }
        }

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
                local_candidates,
                video_mid: None,
                audio_mid: None,
                input_events: Vec::new(),
                pending_dc_out: VecDeque::new(),
                dc_channel_id: None,
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
            Some(_) => {
                tracing::debug!("push_video: not yet connected, dropping frame");
                return Ok(());
            }
            None => {
                tracing::debug!("push_video: no video_mid yet, dropping frame");
                return Ok(());
            }
        };
        let pts_90k = frame.pts_ms * 90;
        let rtp_time = MediaTime::new(pts_90k, Frequency::NINETY_KHZ);
        let writer = self.rtc.writer(mid)
            .ok_or_else(|| anyhow!("No video writer for {:?}", mid))?;
        let pt = writer.payload_params()
            .find(|p| matches!(p.spec().codec, Codec::H264))
            .map(PayloadParams::pt)
            .ok_or_else(|| anyhow!("No H264 PT negotiated"))?;
        tracing::debug!(pts_ms = frame.pts_ms, keyframe = frame.is_keyframe, "Sending video RTP");
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

    /// Queue a message to be sent to the browser via the data channel.
    pub fn push_dc_message(&mut self, data: Vec<u8>) {
        self.pending_dc_out.push_back(data);
    }

    /// Returns true once the browser data channel is open and ready.
    pub fn is_dc_open(&self) -> bool {
        self.dc_channel_id.is_some()
    }

    /// Add a remote ICE candidate received over the signaling channel.
    pub fn add_remote_candidate(&mut self, candidate_str: &str) -> Result<()> {
        let c = Candidate::from_sdp_string(candidate_str)
            .map_err(|e| anyhow!("Candidate parse error: {:?}", e))?;
        self.rtc.add_remote_candidate(c);
        Ok(())
    }

    /// Resolve which local candidate address a packet from `source_ip` arrived on.
    ///
    /// Since the socket is bound to `0.0.0.0`, `local_addr()` returns `0.0.0.0:port`
    /// which str0m doesn't recognise. We pick the registered candidate whose IP
    /// is in the same address family and scope (loopback ↔ loopback, else LAN).
    fn resolve_local_addr(&self, source_ip: std::net::IpAddr) -> std::net::SocketAddr {
        let prefer_loopback = source_ip.is_loopback();
        self.local_candidates
            .iter()
            .find(|a| a.ip().is_loopback() == prefer_loopback)
            .or_else(|| self.local_candidates.first())
            .copied()
            .unwrap_or_else(|| self.socket.local_addr().unwrap())
    }

    /// Drive the str0m I/O state machine once. Must be called in a tight loop.
    pub async fn drive(&mut self) -> Result<SessionState> {
        let now = Instant::now();

        // Drain incoming UDP packets (non-blocking).
        let mut buf = [0u8; 2048];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((n, remote_addr)) => {
                    // Map the wildcard-bound socket address to the matching real
                    // local candidate IP so str0m can find the right candidate pair.
                    let destination = self.resolve_local_addr(remote_addr.ip());
                    let recv = Receive {
                        proto: Protocol::Udp,
                        source: remote_addr,
                        destination,
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
                Ok(Output::Event(Event::ChannelOpen(cid, label))) => {
                    tracing::info!("Data channel opened: id={:?} label={:?}", cid, label);
                    self.dc_channel_id = Some(cid);
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
                    match serde_json::from_slice::<InputEvent>(&data.data) {
                        Ok(ev) => {
                            tracing::debug!("Data channel input: {:?}", ev);
                            self.input_events.push(ev);
                        }
                        Err(e) => {
                            tracing::warn!("Data channel parse error: {} — raw: {:?}",
                                e, String::from_utf8_lossy(&data.data));
                        }
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

        // Send any queued outbound data channel messages.
        if let Some(cid) = self.dc_channel_id {
            while let Some(data) = self.pending_dc_out.pop_front() {
                if let Some(mut ch) = self.rtc.channel(cid) {
                    if ch.write(false, &data).is_err() {
                        // Channel not ready yet; re-queue and stop for this drive cycle.
                        self.pending_dc_out.push_front(data);
                        break;
                    }
                } else {
                    self.pending_dc_out.push_front(data);
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
