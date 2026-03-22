use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::time::Instant;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use lumen_compositor::InputEvent;
use lumen_webrtc::{SessionId, SessionManager};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Shared state passed to every signaling handler.
#[derive(Clone)]
pub struct SignalingState {
    pub sessions: Arc<SessionManager>,
    /// Forwards browser input events to the compositor seat.
    pub input_tx: mpsc::Sender<InputEvent>,
    /// Notifies the encoder task that a keyframe is needed.
    pub keyframe_flag: Arc<AtomicBool>,
    /// The most recent cursor state JSON, replayed to new sessions on DC open.
    pub last_cursor_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    /// The most recent clipboard JSON, replayed to new sessions on DC open.
    pub last_clipboard_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    /// Forwards resize requests to the resize coordinator task.
    pub resize_tx: mpsc::Sender<(u32, u32)>,
    /// ICE server configuration served to the browser via `/api/config`.
    pub ice_servers: Vec<crate::types::IceServerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Offer { sdp: String },
    #[allow(dead_code)] // fields populated by serde from browser ICE candidates
    Candidate { candidate: String, sdp_mid: Option<String>, sdp_m_line_index: Option<u32> },
    Resize { width: u32, height: u32 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Answer { sdp: String, session_id: String },
    Error { message: String },
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SignalingState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Returns the ICE server list (and any future client-side configuration)
/// as a JSON object: `{ "iceServers": [...] }`.
pub async fn config_handler(
    State(state): State<SignalingState>,
) -> axum::response::Json<serde_json::Value> {
    let ice = serde_json::to_value(&state.ice_servers).unwrap_or(serde_json::json!([]));
    axum::response::Json(serde_json::json!({ "iceServers": ice }))
}

async fn handle_socket(mut socket: WebSocket, state: SignalingState) {
    tracing::info!("New signaling connection");
    let mut session_id: Option<SessionId> = None;

    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                send_error(&mut socket, &e.to_string()).await;
                continue;
            }
        };

        match client_msg {
            ClientMessage::Offer { sdp } => {
                tracing::debug!("SDP offer:\n{}", sdp);
                match state.sessions.create_session(&sdp).await {
                    Ok((id, answer_sdp)) => {
                        session_id = Some(id.clone());
                        tracing::debug!("Answer SDP:\n{}", answer_sdp);
                        // Spawn the drive task, forwarding input and keyframe signals.
                        spawn_drive_task(
                            state.sessions.clone(),
                            id.clone(),
                            state.input_tx.clone(),
                            state.keyframe_flag.clone(),
                            state.last_cursor_json.clone(),
                            state.last_clipboard_json.clone(),
                        );
                        let resp = ServerMessage::Answer { sdp: answer_sdp, session_id: id.0 };
                        let _ = socket
                            .send(Message::Text(serde_json::to_string(&resp).unwrap().into()))
                            .await;
                    }
                    Err(e) => send_error(&mut socket, &e.to_string()).await,
                }
            }
            ClientMessage::Candidate { candidate, .. } => {
                if let Some(ref id) = session_id {
                    if let Some(session) = state.sessions.get_session(id).await {
                        let mut s = session.lock().await;
                        if let Err(e) = s.add_remote_candidate(&candidate) {
                            tracing::debug!("Candidate error: {e:#}");
                        }
                    }
                }
            }
            ClientMessage::Resize { width, height } => {
                // Validate: must be positive, even, and within a sane limit.
                if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0
                    || width > 4096 || height > 4096
                {
                    tracing::warn!("Rejected invalid resize {}x{}", width, height);
                } else {
                    let _ = state.resize_tx.try_send((width, height));
                }
            }
        }
    }

    tracing::info!("Signaling connection closed");
    // Remove the session immediately so peer_count drops to zero without waiting
    // for ICE to time out (~20-30s). The drive task will exit on its next iteration
    // when get_session returns None.
    if let Some(ref id) = session_id {
        state.sessions.remove_session(id).await;
    }
}

fn spawn_drive_task(
    sessions: Arc<SessionManager>,
    id: SessionId,
    input_tx: mpsc::Sender<InputEvent>,
    keyframe_flag: Arc<AtomicBool>,
    last_cursor_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    last_clipboard_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
) {
    tokio::spawn(async move {
        // Grab the video notifier once — push_video() fires it every time a new
        // frame is written to str0m, waking this task immediately instead of
        // waiting for the pacer-timeout sleep.  This prevents the fan-out task
        // from piling up multiple frames in str0m before we flush them.
        let video_notify = match sessions.get_session(&id).await {
            Some(s) => s.lock().await.video_notify(),
            None => return,
        };

        let mut cursor_sent = false;
        let mut clipboard_sent = false;
        loop {
            let session = match sessions.get_session(&id).await {
                Some(s) => s,
                None => break,
            };
            let (state, input_events, kf_requested, dc_open, next_wakeup) = {
                let mut s = session.lock().await;
                let (drive_state, next_wakeup) = match s.drive().await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::debug!("Session drive error: {e:#}");
                        break;
                    }
                };
                let events = s.drain_input_events();
                let kf = s.keyframe_requested;
                if kf { s.keyframe_requested = false; }
                let dc_open = s.is_dc_open();
                (drive_state, events, kf, dc_open, next_wakeup)
            };

            // Forward input events to compositor (best-effort, don't block).
            for ev in input_events {
                let _ = input_tx.try_send(ev);
            }

            // Signal encoder to produce a keyframe.
            if kf_requested {
                keyframe_flag.store(true, Ordering::Relaxed);
            }

            // Replay last cursor and clipboard state once when the data channel first opens.
            if dc_open && !cursor_sent {
                cursor_sent = true;
                if let Some(cursor_json) = last_cursor_json.lock().await.clone() {
                    if let Some(session) = sessions.get_session(&id).await {
                        session.lock().await.push_dc_message(cursor_json);
                    }
                }
            }

            if dc_open && !clipboard_sent {
                clipboard_sent = true;
                if let Some(clipboard_json) = last_clipboard_json.lock().await.clone() {
                    if let Some(session) = sessions.get_session(&id).await {
                        session.lock().await.push_dc_message(clipboard_json);
                    }
                }
            }

            if state == lumen_webrtc::SessionState::Closed {
                tracing::info!("WebRTC peer disconnected");
                break;
            }

            // Sleep until str0m's next deadline OR until push_video signals that
            // new data is queued — whichever comes first.  The notify path means
            // we flush the pacer immediately on every new frame instead of waiting
            // for the timeout, which eliminates the burst that confuses Firefox.
            let sleep_dur = next_wakeup
                .saturating_duration_since(Instant::now())
                .min(std::time::Duration::from_millis(5));
            tokio::select! {
                _ = tokio::time::sleep(sleep_dur) => {}
                _ = video_notify.notified() => {}
            }
        }
        // Always clean up — whether the peer disconnected cleanly, the drive
        // task errored, or the session was already removed externally.
        sessions.remove_session(&id).await;
    });
}

async fn send_error(socket: &mut WebSocket, message: &str) {
    let msg = ServerMessage::Error { message: message.to_string() };
    let _ = socket.send(Message::Text(serde_json::to_string(&msg).unwrap().into())).await;
}
