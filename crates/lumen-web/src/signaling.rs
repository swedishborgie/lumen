use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

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
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Offer { sdp: String },
    #[allow(dead_code)] // fields populated by serde from browser ICE candidates
    Candidate { candidate: String, sdp_mid: Option<String>, sdp_m_line_index: Option<u32> },
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
        }
    }

    tracing::info!("Signaling connection closed");
}

fn spawn_drive_task(
    sessions: Arc<SessionManager>,
    id: SessionId,
    input_tx: mpsc::Sender<InputEvent>,
    keyframe_flag: Arc<AtomicBool>,
    last_cursor_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
) {
    tokio::spawn(async move {
        let mut cursor_sent = false;
        loop {
            let session = match sessions.get_session(&id).await {
                Some(s) => s,
                None => break,
            };
            let (state, input_events, kf_requested, dc_open) = {
                let mut s = session.lock().await;
                let drive_state = match s.drive().await {
                    Ok(st) => st,
                    Err(e) => {
                        tracing::debug!("Session drive error: {e:#}");
                        break;
                    }
                };
                let events = s.drain_input_events();
                let kf = s.keyframe_requested;
                if kf { s.keyframe_requested = false; }
                let dc_open = s.is_dc_open();
                (drive_state, events, kf, dc_open)
            };

            // Forward input events to compositor (best-effort, don't block).
            for ev in input_events {
                let _ = input_tx.try_send(ev);
            }

            // Signal encoder to produce a keyframe.
            if kf_requested {
                keyframe_flag.store(true, Ordering::Relaxed);
            }

            // Replay the last cursor state once when the data channel first opens,
            // so reconnecting clients immediately see the correct cursor shape.
            if dc_open && !cursor_sent {
                cursor_sent = true;
                if let Some(cursor_json) = last_cursor_json.lock().await.clone() {
                    if let Some(session) = sessions.get_session(&id).await {
                        session.lock().await.push_dc_message(cursor_json);
                    }
                }
            }

            if state == lumen_webrtc::SessionState::Closed {
                tracing::info!("WebRTC peer disconnected");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
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
