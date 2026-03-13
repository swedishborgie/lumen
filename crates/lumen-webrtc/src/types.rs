use std::net::SocketAddr;

/// ICE server configuration (STUN or TURN).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IceServer {
    /// URL of the form `stun:<host>:<port>` or `turn:<host>:<port>`.
    pub url: String,
    /// Username and credential for TURN servers.
    pub credential: Option<(String, String)>,
}

/// Top-level configuration for a WebRTC session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub ice_servers: Vec<IceServer>,
    /// Local socket address for ICE candidate gathering.
    pub bind_addr: SocketAddr,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ice_servers: vec![IceServer {
                url: "stun:stun.l.google.com:19302".into(),
                credential: None,
            }],
            bind_addr: "0.0.0.0:0".parse().unwrap(),
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
