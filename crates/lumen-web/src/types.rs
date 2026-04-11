use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use lumen_compositor::InputEvent;
use lumen_webrtc::SessionManager;

/// ICE server descriptor sent to the browser via `/api/config`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IceServerConfig {
    pub urls: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// Authentication mode for the web server.
#[derive(Clone)]
pub enum AuthConfig {
    /// No authentication — open access (default).
    None,
    /// HTTP Basic authentication validated against the system PAM.
    ///
    /// The browser presents a native username/password dialog.  The submitted
    /// username must match the user running the lumen process and the password
    /// must pass PAM validation via the `login` service.
    Basic,
    /// Bearer token (preshared key) authentication.
    ///
    /// Every request must include an `Authorization: Bearer <token>` header
    /// whose value matches the configured secret.  Intended for use behind a
    /// reverse proxy that injects the header on behalf of clients.
    Bearer {
        /// The expected token value. Compared in constant time.
        token: String,
    },
    /// OpenID Connect OAuth2 authorization code flow with PKCE.
    ///
    /// On first access the user is redirected to the configured OIDC provider.
    /// After authentication the provider redirects to `/auth/callback`.  The
    /// `sub` claim in the ID token must equal `expected_subject`.
    OAuth2 {
        /// OIDC issuer URL.  The discovery document is fetched from
        /// `{issuer_url}/.well-known/openid-configuration`.
        issuer_url: String,
        client_id: String,
        client_secret: String,
        /// Full redirect URI registered with the provider,
        /// e.g. `http://localhost:8080/auth/callback`.
        redirect_uri: String,
        /// Expected `sub` claim in the validated ID token; access is denied if
        /// it does not match.
        expected_subject: String,
    },
}

/// Server capabilities reported to the client via `/api/config`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerCapabilities {
    /// List of supported video codec names (e.g. `["h264", "vp9"]`).
    pub supported_codecs: Vec<String>,
    /// Currently active video codec name.
    pub current_codec: String,
    /// Currently active target frame rate.
    pub fps: f64,
}

impl Default for ServerCapabilities {
    fn default() -> Self {
        Self {
            supported_codecs: vec!["h264".to_string()],
            current_codec: "h264".to_string(),
            fps: 30.0,
        }
    }
}


/// Configuration for the HTTP + WebSocket server.
pub struct WebServerConfig {
    pub bind_addr: SocketAddr,
    pub session_manager: Arc<SessionManager>,
    /// Channel for forwarding browser input events to the compositor.
    pub input_tx: tokio::sync::mpsc::Sender<InputEvent>,
    /// Flag set when any peer has requested a keyframe.
    pub keyframe_flag: Arc<AtomicBool>,
    /// The most recent cursor state JSON, replayed to new sessions on DC open.
    pub last_cursor_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    /// The most recent clipboard JSON, replayed to new sessions on DC open.
    pub last_clipboard_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    /// Channel for forwarding resize requests (width, height) to the resize coordinator.
    pub resize_tx: tokio::sync::mpsc::Sender<(u32, u32)>,
    /// Authentication configuration.
    pub auth: AuthConfig,
    /// ICE server list sent to the browser via `/api/config`.
    pub ice_servers: Vec<IceServerConfig>,
    /// Hostname shown in the browser tab title and PWA app name.
    pub hostname: String,
    /// Optional graceful-shutdown signal. When the sender is dropped or sends,
    /// the web server stops accepting new connections and drains existing ones.
    pub shutdown_signal: Option<tokio::sync::oneshot::Receiver<()>>,
    /// Receiver for encoder metrics. When `Some`, subscribed browser clients
    /// receive a `metrics` WebSocket message at ~1 Hz while the overlay is open.
    pub encoder_metrics_rx: Option<tokio::sync::watch::Receiver<crate::metrics::EncoderMetrics>>,
    /// Receiver for system metrics (CPU, RAM). Merged with encoder metrics before sending.
    pub system_metrics_rx: Option<tokio::sync::watch::Receiver<crate::metrics::SystemMetrics>>,
    /// Path to the PEM-encoded TLS certificate chain. When both `tls_cert` and
    /// `tls_key` are set the server binds an HTTPS endpoint; otherwise plain HTTP.
    pub tls_cert: Option<PathBuf>,
    /// Path to the PEM-encoded TLS private key. Must be provided together with
    /// `tls_cert`.
    pub tls_key: Option<PathBuf>,
    /// Watch channel carrying current server capabilities (codecs, fps).
    /// Updated by `main.rs` when codec or FPS changes.
    pub capabilities_rx: tokio::sync::watch::Receiver<ServerCapabilities>,
    /// Sender for codec change requests (from browser → encoder task).
    /// Sends the codec name as a lowercase string (e.g. `"h264"`, `"vp9"`).
    pub codec_tx: Arc<tokio::sync::watch::Sender<String>>,
    /// Sender for FPS change requests (from browser → encoder task + compositor).
    pub fps_tx: Arc<tokio::sync::watch::Sender<f64>>,
}
