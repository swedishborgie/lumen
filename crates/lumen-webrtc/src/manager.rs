use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::session::WebRtcSession;
use crate::types::{SessionConfig, SessionId};
/// Manages multiple concurrent WebRTC peer sessions.
pub struct SessionManager {
    config: SessionConfig,
    sessions: Arc<Mutex<HashMap<SessionId, Arc<Mutex<WebRtcSession>>>>>,
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Accept a browser SDP offer, create a new session, and return the
    /// answer SDP alongside a session ID for subsequent signaling messages.
    pub async fn create_session(&self, offer_sdp: &str) -> Result<(SessionId, String)> {
        let (session, answer_sdp) = WebRtcSession::new(self.config.clone(), offer_sdp).await?;
        let id = SessionId::new();
        let session = Arc::new(Mutex::new(session));
        self.sessions.lock().await.insert(id.clone(), session);
        Ok((id, answer_sdp))
    }

    pub async fn get_session(&self, id: &SessionId) -> Option<Arc<Mutex<WebRtcSession>>> {
        self.sessions.lock().await.get(id).cloned()
    }

    pub async fn remove_session(&self, id: &SessionId) {
        self.sessions.lock().await.remove(id);
    }

    /// Return all active sessions (for media fan-out).
    pub async fn all_sessions(&self) -> Vec<Arc<Mutex<WebRtcSession>>> {
        self.sessions.lock().await.values().cloned().collect()
    }
}
