use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use lumen_compositor::InputEvent;
use lumen_webrtc::SessionManager;

/// Configuration for the HTTP + WebSocket server.
#[derive(Clone)]
pub struct WebServerConfig {
    pub bind_addr: SocketAddr,
    /// Directory from which static files (HTML, JS, CSS) are served.
    pub static_dir: PathBuf,
    pub session_manager: Arc<SessionManager>,
    /// Channel for forwarding browser input events to the compositor.
    pub input_tx: tokio::sync::mpsc::Sender<InputEvent>,
    /// Flag set when any peer has requested a keyframe.
    pub keyframe_flag: Arc<AtomicBool>,
}
