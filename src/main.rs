use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

use anyhow::Result;
use base64::Engine as _;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::broadcast;

#[derive(Parser, Debug)]
#[command(name = "lumen", about = "Wayland WebRTC streaming compositor")]
struct Args {
    #[arg(long, env = "LUMEN_BIND", default_value = "0.0.0.0:8080")]
    bind_addr: SocketAddr,
    #[arg(long, env = "LUMEN_WIDTH", default_value_t = 1920)]
    width: u32,
    #[arg(long, env = "LUMEN_HEIGHT", default_value_t = 1080)]
    height: u32,
    #[arg(long, env = "LUMEN_FPS", default_value_t = 30.0)]
    fps: f64,
    #[arg(long, env = "LUMEN_VIDEO_BITRATE_KBPS", default_value_t = 4000)]
    video_bitrate_kbps: u32,
    #[arg(long, env = "LUMEN_AUDIO_BITRATE_BPS", default_value_t = 128_000)]
    audio_bitrate_bps: i32,
    #[arg(long, env = "LUMEN_AUDIO_DEVICE")]
    audio_device: Option<String>,
    #[arg(long, env = "LUMEN_DRI_NODE")]
    dri_node: Option<PathBuf>,
    #[arg(long, env = "LUMEN_ICE_SERVERS", default_value = "stun:stun.l.google.com:19302")]
    ice_servers: String,
    #[arg(long, env = "LUMEN_STATIC_DIR", default_value = "./web")]
    static_dir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lumen=info".parse()?)
                .add_directive("lumen_compositor=info".parse()?)
                .add_directive("lumen_audio=info".parse()?)
                .add_directive("lumen_encode=info".parse()?)
                .add_directive("lumen_webrtc=info".parse()?)
                .add_directive("lumen_web=info".parse()?),
        )
        .init();

    let args = Args::parse();
    tracing::info!(?args.bind_addr, "Starting lumen");

    // ── Compositor ────────────────────────────────────────────────────────────
    let mut compositor = lumen_compositor::Compositor::new(lumen_compositor::CompositorConfig {
        width: args.width,
        height: args.height,
        target_fps: args.fps,
        render_node: args.dri_node.clone(),
        ..Default::default()
    })?;
    let frame_rx = compositor.frame_receiver();
    let cursor_rx = compositor.cursor_receiver();
    let compositor_input_tx = compositor.input_sender();

    // ── Audio ─────────────────────────────────────────────────────────────────
    let (mut audio_capture, audio_rx) = lumen_audio::AudioCapture::new(lumen_audio::AudioConfig {
        device_name: args.audio_device.clone(),
        bitrate_bps: args.audio_bitrate_bps,
        ..Default::default()
    })?;

    // ── Encoder ───────────────────────────────────────────────────────────────
    let encoder_config = lumen_encode::EncoderConfig {
        width: args.width,
        height: args.height,
        fps: args.fps,
        bitrate_kbps: args.video_bitrate_kbps,
        render_node: args.dri_node,
        ..Default::default()
    };

    // Broadcast channel distributes encoded frames to all active WebRTC sessions.
    let (encoded_tx, _) = broadcast::channel::<Arc<lumen_encode::EncodedFrame>>(8);

    // Shared keyframe request flag: set by a drive task, polled by the encoder.
    let keyframe_flag = Arc::new(AtomicBool::new(false));

    // ── Session manager ───────────────────────────────────────────────────────
    let ice_servers = args.ice_servers.split(',')
        .map(|s| lumen_webrtc::types::IceServer { url: s.trim().to_string(), credential: None })
        .collect();
    let session_manager = lumen_webrtc::SessionManager::new(lumen_webrtc::SessionConfig {
        ice_servers,
        bind_addr: "0.0.0.0:0".parse()?,
    });

    // ── Input forwarding channel ──────────────────────────────────────────────
    // Drive tasks drop events into this channel; a dedicated task drains them
    // into the compositor's input sender.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<lumen_compositor::InputEvent>(256);

    // ── Spawn: compositor ─────────────────────────────────────────────────────
    std::thread::spawn(move || {
        if let Err(e) = compositor.run() {
            tracing::error!("Compositor: {e:#}");
        }
    });

    // ── Spawn: audio capture ──────────────────────────────────────────────────
    tokio::task::spawn_blocking(move || {
        if let Err(e) = audio_capture.run() {
            tracing::error!("Audio capture: {e:#}");
        }
    });

    // ── Spawn: encoder task ───────────────────────────────────────────────────
    {
        let encoded_tx = encoded_tx.clone();
        let keyframe_flag = keyframe_flag.clone();
        let peer_count = session_manager.peer_count();
        tokio::task::spawn_blocking(move || {
            let mut encoder = match lumen_encode::create_encoder(&encoder_config) {
                Ok(e) => e,
                Err(e) => { tracing::error!("Encoder init: {e:#}"); return; }
            };
            let mut frame_rx = frame_rx;
            let mut encoded_count: u64 = 0;
            loop {
                let frame = match frame_rx.blocking_recv() {
                    Ok(f) => f,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!("Encoder dropped {n} frames (channel full)");
                        continue;
                    }
                    Err(_) => break,
                };
                // Skip encoding when nobody is watching.
                if peer_count.load(Ordering::Relaxed) == 0 {
                    continue;
                }
                // Service any pending keyframe request before encoding.
                if keyframe_flag.swap(false, Ordering::Relaxed) {
                    encoder.request_keyframe();
                }
                match encoder.encode(frame) {
                    Ok(Some(ef)) => {
                        encoded_count += 1;
                        if encoded_count == 1 || encoded_count % 150 == 0 {
                            tracing::info!(encoded_count, keyframe = ef.is_keyframe,
                                bytes = ef.data.len(), "Encoded frame");
                        }
                        let _ = encoded_tx.send(Arc::new(ef));
                    }
                    Ok(None) => {
                        if encoded_count == 0 {
                            tracing::debug!("Encoder returned None (buffering)");
                        }
                    }
                    Err(e) => tracing::warn!("Encode error: {e:#}"),
                }
            }
        });
    }

    // ── Spawn: input forwarding task ──────────────────────────────────────────
    tokio::spawn(async move {
        while let Some(ev) = input_rx.recv().await {
            compositor_input_tx.send(ev);
        }
    });

    // ── Spawn: audio fan-out task ─────────────────────────────────────────────
    {
        let session_manager = session_manager.clone();
        let mut audio_rx = audio_rx;
        tokio::spawn(async move {
            loop {
                let packet = match audio_rx.recv().await {
                    Some(p) => p,
                    None => break,
                };
                let sessions = session_manager.all_sessions().await;
                for session in sessions {
                    let mut s = session.lock().await;
                    if let Err(e) = s.push_audio(&packet) {
                        tracing::debug!("Audio push error: {e:#}");
                    }
                }
            }
        });
    }

    // ── Spawn: video fan-out task ─────────────────────────────────────────────
    {
        let session_manager = session_manager.clone();
        let mut encoded_rx = encoded_tx.subscribe();
        tokio::spawn(async move {
            loop {
                let frame = match encoded_rx.recv().await {
                    Ok(f) => f,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Video fan-out lagged {n}");
                        continue;
                    }
                    Err(_) => break,
                };
                let sessions = session_manager.all_sessions().await;
                for session in sessions {
                    let mut s = session.lock().await;
                    if let Err(e) = s.push_video(&frame) {
                        tracing::debug!("Video push error: {e:#}");
                    }
                }
            }
        });
    }

    // ── Spawn: cursor fan-out task ────────────────────────────────────────────
    {
        let session_manager = session_manager.clone();
        let mut cursor_rx = cursor_rx;
        tokio::spawn(async move {
            loop {
                let ev = match cursor_rx.recv().await {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                let json = cursor_event_to_json(&ev);
                session_manager.broadcast_dc_message(json).await;
            }
        });
    }

    // ── Web server ────────────────────────────────────────────────────────────
    lumen_web::WebServer::new(lumen_web::WebServerConfig {
        bind_addr: args.bind_addr,
        static_dir: args.static_dir,
        session_manager,
        input_tx,
        keyframe_flag,
    })
    .run()
    .await
}

/// Encode a `CursorEvent` as a JSON byte string suitable for the data channel.
fn cursor_event_to_json(ev: &lumen_compositor::CursorEvent) -> Vec<u8> {
    use lumen_compositor::CursorEvent;
    match ev {
        CursorEvent::Default => br#"{"type":"cursor_update","kind":"default"}"#.to_vec(),
        CursorEvent::Hidden  => br#"{"type":"cursor_update","kind":"hidden"}"#.to_vec(),
        CursorEvent::Image { width, height, hotspot_x, hotspot_y, rgba } => {
            let data = base64::engine::general_purpose::STANDARD.encode(rgba);
            format!(
                r#"{{"type":"cursor_update","kind":"image","w":{width},"h":{height},"hotspot_x":{hotspot_x},"hotspot_y":{hotspot_y},"data":"{data}"}}"#
            ).into_bytes()
        }
    }
}
