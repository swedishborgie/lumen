//! lumen-web — HTTP server and WebSocket signaling.
//!
//! Serves the browser client over HTTP and handles WebRTC signaling
//! (SDP offer/answer, trickle ICE) over a WebSocket endpoint at `/ws`.
//! Static assets are embedded into the binary at compile time.

pub mod assets;
pub mod auth;
pub mod metrics;
pub mod server;
pub mod signaling;
pub mod types;

pub use server::WebServer;
pub use types::{AuthConfig, IceServerConfig, WebServerConfig};
