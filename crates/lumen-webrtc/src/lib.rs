//! lumen-webrtc — WebRTC session management via str0m.
//!
//! Handles ICE negotiation, DTLS handshake, and SRTP media delivery.
//! Packetizes H.264 frames (RFC 6184) and Opus packets (RFC 7587) into RTP.
//! Decodes inbound WebRTC data-channel messages into `InputEvent`s
//! for forwarding back to the compositor.

pub mod session;
pub mod manager;
pub mod packetize;
pub mod types;

pub use manager::SessionManager;
pub use session::{SessionState, WebRtcSession};
pub use types::{IceServer, SessionConfig, SessionId};
