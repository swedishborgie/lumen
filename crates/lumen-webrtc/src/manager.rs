use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::session::WebRtcSession;
use crate::types::{SessionConfig, SessionId};
/// Manages multiple concurrent WebRTC peer sessions.
pub struct SessionManager {
    config: SessionConfig,
    sessions: Arc<Mutex<HashMap<SessionId, Arc<Mutex<WebRtcSession>>>>>,
    /// Number of currently active sessions. Updated atomically so the encoder
    /// task can check cheaply without blocking on the async mutex.
    peer_count: Arc<AtomicUsize>,
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            peer_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// A cheaply-cloneable handle to the live peer count.
    /// The encoder task uses this to skip encoding when no one is watching.
    pub fn peer_count(&self) -> Arc<AtomicUsize> {
        self.peer_count.clone()
    }

    /// Accept a browser SDP offer, create a new session, and return the
    /// answer SDP alongside a session ID for subsequent signaling messages.
    pub async fn create_session(&self, offer_sdp: &str) -> Result<(SessionId, String)> {
        let (session, answer_sdp) = WebRtcSession::new(self.config.clone(), offer_sdp).await?;
        let id = SessionId::new();
        let session = Arc::new(Mutex::new(session));
        self.sessions.lock().await.insert(id.clone(), session);
        self.peer_count.fetch_add(1, Ordering::Relaxed);
        Ok((id, answer_sdp))
    }

    pub async fn get_session(&self, id: &SessionId) -> Option<Arc<Mutex<WebRtcSession>>> {
        self.sessions.lock().await.get(id).cloned()
    }

    pub async fn remove_session(&self, id: &SessionId) {
        let removed = self.sessions.lock().await.remove(id).is_some();
        if removed {
            self.peer_count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Return all active sessions (for media fan-out).
    pub async fn all_sessions(&self) -> Vec<Arc<Mutex<WebRtcSession>>> {
        self.sessions.lock().await.values().cloned().collect()
    }

    /// Send a data channel message to every connected session.
    pub async fn broadcast_dc_message(&self, data: Vec<u8>) {
        let sessions = self.sessions.lock().await;
        for session in sessions.values() {
            session.lock().await.push_dc_message(data.clone());
        }
    }
}
