use std::sync::{atomic::AtomicBool, Arc};

use anyhow::Result;
use clap::Parser;
use tokio::sync::broadcast;

mod cli;
mod hardware;
mod logging;
mod tasks;
mod turn;

#[tokio::main]
async fn main() -> Result<()> {
    // Rustls requires an explicit CryptoProvider when multiple backends (ring,
    // aws-lc-rs) are present in the dependency tree.  Install ring as the default
    // before any TLS code runs.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls ring CryptoProvider");

    let args = cli::Args::parse();
    logging::init(&args)?;

    tracing::info!(?args.bind_addr, "Starting lumen");

    let auth = cli::build_auth_config(&args)?;

    // ── TURN + ICE ────────────────────────────────────────────────────────────
    // `turn_setup._server` must remain bound for the lifetime of main() to keep
    // the embedded TURN server running.
    let turn_setup = turn::setup(&args).await?;

    // ── Session manager ───────────────────────────────────────────────────────
    // Created early so peer_count can be passed to the compositor and audio.
    let session_manager = lumen_webrtc::SessionManager::new(lumen_webrtc::SessionConfig {
        turn: turn_setup.client_config,
        bind_addr: "0.0.0.0:0".parse()?,
    });
    let peer_count = session_manager.peer_count();

    // ── GPU detection ─────────────────────────────────────────────────────────
    let effective_dri_node = hardware::detect_and_probe_gpu(&args);

    // ── Compositor ────────────────────────────────────────────────────────────
    // Set up a channel so the compositor can notify us when its Wayland socket
    // is ready — used by the --launch task below.
    let (socket_name_tx, socket_name_rx) = std::sync::mpsc::sync_channel::<String>(1);

    let inner_display = if args.inner_display.is_empty() {
        None
    } else {
        Some(args.inner_display.clone())
    };

    let compositor = lumen_compositor::Compositor::new(lumen_compositor::CompositorConfig {
        width: args.width,
        height: args.height,
        target_fps: args.fps,
        render_node: effective_dri_node.clone(),
        inner_display,
        peer_count: Some(peer_count.clone()),
        socket_name_tx: Some(socket_name_tx),
        ..Default::default()
    })?;
    let frame_rx = compositor.frame_receiver();
    let cursor_rx = compositor.cursor_receiver();
    let clipboard_rx = compositor.clipboard_receiver();
    let compositor_input_tx = compositor.input_sender();

    // ── Audio ─────────────────────────────────────────────────────────────────
    let (audio_capture, audio_rx) = lumen_audio::AudioCapture::new(lumen_audio::AudioConfig {
        device_name: args.audio_device.clone(),
        bitrate_bps: args.audio_bitrate_bps,
        peer_count: Some(peer_count.clone()),
        ..Default::default()
    })?;

    // ── Encoder ───────────────────────────────────────────────────────────────
    let encoder_config = lumen_encode::EncoderConfig {
        width: args.width,
        height: args.height,
        fps: args.fps,
        bitrate_kbps: args.video_bitrate_kbps,
        max_bitrate_kbps: args.max_bitrate_kbps.unwrap_or(args.video_bitrate_kbps * 2),
        render_node: effective_dri_node,
        #[cfg(feature = "nvenc")]
        cuda_device: hardware::resolve_cuda_device(&args),
        ..Default::default()
    };

    // ── Channels ──────────────────────────────────────────────────────────────
    // Broadcast channel distributes encoded frames to all active WebRTC sessions.
    let (encoded_tx, _) = broadcast::channel::<Arc<lumen_encode::EncodedFrame>>(8);
    // Shared keyframe request flag: set by a drive task, polled by the encoder.
    let keyframe_flag = Arc::new(AtomicBool::new(false));
    // Web server → resize coordinator (async).
    let (resize_tx, resize_rx) = tokio::sync::mpsc::channel::<(u32, u32)>(4);
    // Resize coordinator → encoder task (std channel, non-blocking try_recv).
    let (enc_resize_tx, enc_resize_rx) = std::sync::mpsc::channel::<(u32, u32)>();
    // Drive tasks drop events into this channel; a dedicated task drains them
    // into the compositor's input sender.
    let (input_tx, input_rx) = tokio::sync::mpsc::channel::<lumen_compositor::InputEvent>(256);
    // Gamepad events from the browser are forwarded to the gamepad manager which
    // creates and drives virtual uinput devices.
    let (gamepad_tx, gamepad_rx) = tokio::sync::mpsc::channel::<lumen_gamepad::GamepadEvent>(64);
    // Haptic commands from the gamepad manager are broadcast back to browsers.
    let (haptic_tx, haptic_rx) = tokio::sync::mpsc::channel::<(u8, lumen_gamepad::HapticCommand)>(64);
    // Encoder metrics watch channel: encoder writes latest metrics, signaling layer reads.
    let (encoder_metrics_tx, encoder_metrics_rx) =
        tokio::sync::watch::channel(lumen_web::metrics::EncoderMetrics::default());

    // ── Shutdown signal ───────────────────────────────────────────────────────
    // When --launch is used, the child exiting triggers a graceful shutdown of
    // the web server. Without --launch, the server runs until interrupted.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // ── Spawn tasks ───────────────────────────────────────────────────────────
    tasks::spawn_compositor(compositor);
    tasks::spawn_audio(audio_capture);
    tasks::spawn_encoder(
        encoder_config,
        frame_rx,
        encoded_tx.clone(),
        keyframe_flag.clone(),
        enc_resize_rx,
        peer_count,
        Some(encoder_metrics_tx),
    );
    tasks::spawn_gamepad_manager(gamepad_rx, haptic_tx);
    let _shutdown_keep_alive = tasks::spawn_launch_task(
        args.effective_launch(),
        socket_name_rx,
        shutdown_tx,
    );
    tasks::spawn_input_forwarder(input_rx, compositor_input_tx.clone(), gamepad_tx);
    tasks::spawn_haptic_fanout(haptic_rx, session_manager.clone());
    tasks::spawn_resize_coordinator(
        resize_rx,
        compositor_input_tx.clone(),
        keyframe_flag.clone(),
        enc_resize_tx,
    );
    tasks::spawn_audio_fanout(audio_rx, session_manager.clone());
    tasks::spawn_video_fanout(encoded_tx, session_manager.clone());
    let last_cursor_json = tasks::spawn_cursor_fanout(cursor_rx, session_manager.clone());
    let last_clipboard_json = tasks::spawn_clipboard_fanout(clipboard_rx, session_manager.clone());
    let system_metrics_rx = tasks::spawn_system_monitor();

    // ── Web server ────────────────────────────────────────────────────────────
    anyhow::ensure!(
        args.tls_cert.is_some() == args.tls_key.is_some(),
        "--tls-cert and --tls-key must be provided together; supply both or neither"
    );

    lumen_web::WebServer::new(lumen_web::WebServerConfig {
        bind_addr: args.bind_addr,
        session_manager,
        input_tx,
        keyframe_flag,
        last_cursor_json,
        last_clipboard_json,
        resize_tx,
        auth,
        ice_servers: turn_setup.ice_servers,
        hostname: args.hostname,
        shutdown_signal: Some(shutdown_rx),
        encoder_metrics_rx: Some(encoder_metrics_rx),
        system_metrics_rx,
        tls_cert: args.tls_cert,
        tls_key: args.tls_key,
    })
    .run()
    .await
    .map_err(|e| {
        tracing::error!("Web server error: {e:#}");
        e
    })?;

    // The web server has shut down (either via graceful shutdown signal or
    // by returning normally).  Stop the compositor, which closes the broadcast
    // channels and causes the encoder to exit.  Then force-exit the process to
    // clean up threads that don't have explicit stop signals (audio capture).
    tracing::info!("Web server stopped; shutting down");
    compositor_input_tx.stop();
    std::process::exit(0);
}
