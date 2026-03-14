use std::net::{IpAddr, SocketAddr};

/// ICE server configuration (STUN or TURN).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IceServer {
    /// URL of the form `stun:<host>:<port>` or `turn:<host>:<port>`.
    pub url: String,
    /// Username and credential for TURN servers.
    pub credential: Option<(String, String)>,
}

/// Configuration for the embedded TURN client used by each WebRTC session.
///
/// When present, the session allocates a relay address on the TURN server and
/// adds it as an ICE relay candidate so browsers behind NAT can connect.
#[derive(Debug, Clone)]
pub struct TurnClientConfig {
    /// Address of the TURN server (typically `127.0.0.1:3478` when the server
    /// is embedded in the same process).
    pub server_addr: SocketAddr,
    pub username: String,
    pub password: String,
    /// The external IP advertised by the TURN server for relay addresses.
    /// Used to pre-create a TURN permission so the browser can send to us.
    pub relay_ip: IpAddr,
}

/// Top-level configuration for a WebRTC session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Local socket address for ICE candidate gathering.
    pub bind_addr: SocketAddr,
    /// Optional TURN client configuration.  When `Some`, each session
    /// allocates a TURN relay candidate in addition to host candidates.
    pub turn: Option<TurnClientConfig>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            turn: None,
        }
    }
}

/// Opaque session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}
