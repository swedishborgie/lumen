//! lumen-web — HTTP server and WebSocket signaling.
//!
//! Serves the browser client over HTTP and handles WebRTC signaling
//! (SDP offer/answer, trickle ICE) over a WebSocket endpoint at `/ws`.
//! Static assets are served from a configurable directory.

pub mod server;
pub mod signaling;
pub mod types;

pub use server::WebServer;
pub use types::WebServerConfig;
