use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

use anyhow::{Context as _, Result};
use base64::Engine as _;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::broadcast;

/// Log output destination.
#[derive(Clone, Debug)]
enum LogOutput {
    Stderr,
    Journald,
    File(PathBuf),
}

impl std::str::FromStr for LogOutput {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stderr" => Ok(Self::Stderr),
            "journald" => Ok(Self::Journald),
            s if s.starts_with("file:") => Ok(Self::File(PathBuf::from(&s[5..]))),
            _ => Err(format!(
                "unknown log output {s:?}; expected 'stderr', 'journald', or 'file:/path/to/log'"
            )),
        }
    }
}

///
/// Authentication mode for the web server.
#[derive(clap::ValueEnum, Clone, Debug, Default)]
enum AuthMode {
    None,
    #[default]
    Basic,
    Bearer,
    Oauth2,
}

/// Named desktop environment preset.
///
/// Each preset provides a default launch command and the environment variables
/// required by that desktop.  `LUMEN_LAUNCH` overrides the launch command but
/// the preset's env vars are still applied to the child process.
#[derive(clap::ValueEnum, Clone, Debug)]
enum DesktopPreset {
    /// labwc — a lightweight wlroots-based Wayland compositor.
    Labwc,
    /// KDE Plasma — full desktop session via `dbus-run-session startplasma-wayland`.
    /// KDE packages must be installed separately; they are not a lumen dependency.
    Kde,
}

impl DesktopPreset {
    /// Default shell command to launch for this preset (passed to `/bin/sh -c`).
    fn default_launch_cmd(&self) -> &'static str {
        match self {
            Self::Labwc => "labwc",
            Self::Kde => "dbus-run-session startplasma-wayland",
        }
    }

    /// Environment variables to inject into the child process.
    fn env_vars(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Labwc => &[],
            Self::Kde => &[
                ("QT_QPA_PLATFORM", "wayland"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
                ("XDG_SESSION_TYPE", "wayland"),
                ("KDE_SESSION_VERSION", "6"),
                ("XDG_MENU_PREFIX", "plasma-"),
                ("PLASMA_SKIP_SPLASH", "1"),
                ("KDE_FULL_SESSION", "true"),
                ("DESKTOP_SESSION", "plasma"),
            ],
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "lumen", about = "Wayland WebRTC streaming compositor", version = env!("LUMEN_VERSION"))]
struct Args {
    /// Log output destination. Accepted values: `stderr` (default), `journald`,
    /// or `file:/absolute/path/to/lumen.log`.
    #[arg(long, env = "LUMEN_LOG_OUTPUT", default_value = "stderr")]
    log_output: LogOutput,

    /// Syslog identifier to use when logging to journald. Defaults to the binary name.
    /// Set to the systemd unit instance name (e.g. `lumen@alice`) so that
    /// `journalctl -u lumen@alice` collects the right logs.
    #[arg(long, env = "LUMEN_SYSLOG_IDENTIFIER")]
    syslog_identifier: Option<String>,

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
    /// Peak bitrate cap in kbps for VBR encoding. Defaults to 2× `--video-bitrate-kbps`.
    #[arg(long, env = "LUMEN_MAX_BITRATE_KBPS")]
    max_bitrate_kbps: Option<u32>,
    #[arg(long, env = "LUMEN_AUDIO_BITRATE_BPS", default_value_t = 128_000)]
    audio_bitrate_bps: i32,
    #[arg(long, env = "LUMEN_AUDIO_DEVICE")]
    audio_device: Option<String>,
    #[arg(long, env = "LUMEN_DRI_NODE")]
    dri_node: Option<PathBuf>,
    /// Wayland socket name of a nested inner compositor whose clipboard should be bridged
    /// (e.g. `wayland-inner`). Defaults to `auto`, which scans `$XDG_RUNTIME_DIR` for a
    /// compositor advertising `zwlr_data_control_manager_v1`. Set to an empty string to
    /// disable clipboard bridging entirely.
    #[arg(long, env = "LUMEN_INNER_DISPLAY", default_value = "auto")]
    inner_display: String,
    #[arg(long, env = "LUMEN_ICE_SERVERS", default_value = "stun:stun.l.google.com:19302")]
    ice_servers: String,

    // ── TURN server ───────────────────────────────────────────────────────────
    /// UDP port for the embedded TURN server. Set to 0 to disable.
    #[arg(long, env = "LUMEN_TURN_PORT", default_value_t = 3478)]
    turn_port: u16,
    /// External/public IP of this machine, used as the TURN relay address.
    /// When not set, lumen auto-detects the outbound IP using a routing probe.
    /// Falls back to 127.0.0.1 (localhost-only) if detection fails.
    #[arg(long, env = "LUMEN_TURN_EXTERNAL_IP")]
    turn_external_ip: Option<std::net::IpAddr>,
    /// TURN username. When not set, a random credential is generated at startup.
    #[arg(long, env = "LUMEN_TURN_USERNAME")]
    turn_username: Option<String>,
    /// TURN password. When not set, a random credential is generated at startup.
    #[arg(long, env = "LUMEN_TURN_PASSWORD", hide_env_values = true)]
    turn_password: Option<String>,
    /// Lowest UDP port in the TURN relay range.
    #[arg(long, env = "LUMEN_TURN_MIN_PORT", default_value_t = 50000)]
    turn_min_port: u16,
    /// Highest UDP port in the TURN relay range.
    #[arg(long, env = "LUMEN_TURN_MAX_PORT", default_value_t = 50010)]
    turn_max_port: u16,

    // ── Authentication ────────────────────────────────────────────────────────
    /// Authentication mode: none (default), basic (PAM), bearer (preshared token), or oauth2 (OIDC).
    #[arg(long, env = "LUMEN_AUTH", default_value = "basic")]
    auth: AuthMode,

    /// [bearer] Preshared token for bearer-token authentication.
    /// Every request must include `Authorization: Bearer <token>` with this value.
    /// Intended for use behind a reverse proxy that injects the header.
    #[arg(long, env = "LUMEN_AUTH_BEARER_TOKEN", hide_env_values = true)]
    auth_bearer_token: Option<String>,

    /// [oauth2] OIDC issuer URL.  The discovery document is fetched from
    /// `{issuer_url}/.well-known/openid-configuration`.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_ISSUER_URL")]
    auth_oauth2_issuer_url: Option<String>,

    /// [oauth2] OAuth2 client ID.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_CLIENT_ID")]
    auth_oauth2_client_id: Option<String>,

    /// [oauth2] OAuth2 client secret.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_CLIENT_SECRET", hide_env_values = true)]
    auth_oauth2_client_secret: Option<String>,

    /// [oauth2] Full redirect URI registered with the provider,
    /// e.g. `http://localhost:8080/auth/callback`.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_REDIRECT_URI")]
    auth_oauth2_redirect_uri: Option<String>,

    /// [oauth2] Expected `sub` claim in the validated ID token; access is
    /// denied if it does not match.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_SUBJECT")]
    auth_oauth2_subject: Option<String>,

    // ── Launch ────────────────────────────────────────────────────────────────
    /// Named desktop environment preset to launch.  Accepted values: `labwc`
    /// (default when set via the service unit), `kde`.  Each preset provides a
    /// default launch command and the environment variables required by that
    /// desktop.  `--launch` / `LUMEN_LAUNCH` overrides the launch command but
    /// the preset's env vars are still applied.
    #[arg(long, env = "LUMEN_DESKTOP")]
    desktop: Option<DesktopPreset>,

    /// Shell command to launch as a Wayland client once the compositor socket
    /// is ready.  Passed to `/bin/sh -c`, so arguments and shell syntax are
    /// supported (e.g. `--launch "labwc"` or `--launch "weston --backend=wayland"`).
    /// The child receives `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`; `DISPLAY` is
    /// unset so it connects via Wayland rather than X11.
    /// When `--desktop` is also set, this overrides only the launch command;
    /// the preset's required environment variables are still applied.
    #[arg(long, env = "LUMEN_LAUNCH")]
    launch: Option<String>,

    // ── TLS ───────────────────────────────────────────────────────────────────
    /// Path to a PEM-encoded TLS certificate chain. When both `--tls-cert` and
    /// `--tls-key` are provided the server binds an HTTPS endpoint instead of
    /// plain HTTP. Both arguments must be supplied together.
    #[arg(long, env = "LUMEN_TLS_CERT")]
    tls_cert: Option<PathBuf>,

    /// Path to a PEM-encoded TLS private key. Must be provided together with
    /// `--tls-cert`.
    #[arg(long, env = "LUMEN_TLS_KEY")]
    tls_key: Option<PathBuf>,
}

/// Generate a random TURN username and password for ephemeral credentials.
///
/// Uses 16 random bytes (hex-encoded) for the username and 24 random bytes
/// (base64-encoded) for the password, giving ~128 bits of entropy each.
fn generate_turn_credentials() -> (String, String) {
    let username_bytes: [u8; 16] = rand::random();
    let password_bytes: [u8; 24] = rand::random();
    let username = username_bytes.iter().map(|b| format!("{b:02x}")).collect();
    let password = base64::engine::general_purpose::STANDARD.encode(password_bytes);
    (username, password)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Rustls requires an explicit CryptoProvider when multiple backends (ring,
    // aws-lc-rs) are present in the dependency tree.  Install ring as the default
    // before any TLS code runs.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls ring CryptoProvider");

    // Parse CLI arguments first so --log-output (or LUMEN_LOG_OUTPUT) is available
    // before the tracing subscriber is initialized.
    let args = Args::parse();

    // If RUST_LOG is set, use it as-is. Otherwise fall back to per-crate info defaults with
    // targeted Smithay selection/keyboard debug enabled for clipboard troubleshooting.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("")
                .add_directive("lumen=info".parse().unwrap())
                .add_directive("lumen_compositor=info".parse().unwrap())
                .add_directive("lumen_audio=info".parse().unwrap())
                .add_directive("lumen_encode=info".parse().unwrap())
                .add_directive("lumen_webrtc=info".parse().unwrap())
                .add_directive("lumen_web=info".parse().unwrap())
        });
    match &args.log_output {
        LogOutput::Stderr => {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .init();
        }
        LogOutput::Journald => {
            use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
            match tracing_journald::layer() {
                Ok(journald_layer) => {
                    let journald_layer = match &args.syslog_identifier {
                        Some(id) => journald_layer.with_syslog_identifier(id.clone()),
                        None => journald_layer,
                    };
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(journald_layer)
                        .init();
                }
                Err(e) => {
                    tracing_subscriber::fmt()
                        .with_env_filter(env_filter)
                        .init();
                    tracing::warn!("journald unavailable ({e}), falling back to stderr");
                }
            }
        }
        LogOutput::File(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("failed to open log file: {}", path.display()))?;
            tracing_subscriber::fmt()
                .with_writer(Mutex::new(file))
                .with_env_filter(env_filter)
                .init();
        }
    }

    tracing::info!(?args.bind_addr, "Starting lumen");

    // ── Auth config ───────────────────────────────────────────────────────────
    let auth = build_auth_config(&args)?;

    // ── TURN server ───────────────────────────────────────────────────────────
    // Resolve the TURN external IP: explicit flag > auto-detect > 127.0.0.1.
    let turn_external_ip = args.turn_external_ip
        .or_else(|| {
            let ip = detect_outbound_ip();
            if let Some(ref detected) = ip {
                tracing::info!(%detected, "Auto-detected TURN external IP");
            } else {
                tracing::warn!(
                    "Could not detect a non-loopback outbound IP; \
                     TURN relay will use 127.0.0.1 (localhost-only). \
                     Set LUMEN_TURN_EXTERNAL_IP to expose lumen outside this machine."
                );
            }
            ip
        })
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap());

    // Start the embedded TURN server unless --turn-port 0 is passed.
    // The server allocates relay ports for both lumen and the browser so they
    // can reach each other through Podman's virtual network.
    //
    // IMPORTANT: `_turn_server` must remain bound for the lifetime of main().
    // Dropping it shuts down the TURN server and invalidates all relay allocations.
    let _turn_server;
    let (turn_client_config, ice_server_list) = if args.turn_port > 0 {
        // Resolve credentials: use explicit values if provided, otherwise generate
        // a random ephemeral pair that exists only for the lifetime of this process.
        let (turn_username, turn_password) = match (args.turn_username, args.turn_password) {
            (Some(u), Some(p)) => (u, p),
            (Some(u), None) => {
                let (_, p) = generate_turn_credentials();
                (u, p)
            }
            (None, Some(p)) => {
                let (u, _) = generate_turn_credentials();
                (u, p)
            }
            (None, None) => {
                let creds = generate_turn_credentials();
                tracing::info!("TURN credentials not configured; using auto-generated ephemeral credentials");
                creds
            }
        };
        let turn_cfg = lumen_turn::TurnServerConfig {
            listen_port: args.turn_port,
            external_ip: turn_external_ip,
            min_relay_port: args.turn_min_port,
            max_relay_port: args.turn_max_port,
            username: turn_username.clone(),
            password: turn_password.clone(),
            ..Default::default()
        };
        _turn_server = Some(lumen_turn::TurnServer::start(turn_cfg).await?);
        tracing::info!(
            port = args.turn_port,
            relay_ip = %turn_external_ip,
            "Embedded TURN server started"
        );

        let server_addr: SocketAddr =
            format!("127.0.0.1:{}", args.turn_port).parse()?;

        let client_cfg = lumen_webrtc::types::TurnClientConfig {
            server_addr,
            username: turn_username.clone(),
            password: turn_password.clone(),
            relay_ip: turn_external_ip,
        };

        let turn_url = format!("turn:{}:{}?transport=udp",
            turn_external_ip, args.turn_port);
        let ice_servers = vec![
            lumen_web::IceServerConfig {
                urls: turn_url,
                username: Some(turn_username),
                credential: Some(turn_password),
            },
        ];
        (Some(client_cfg), ice_servers)
    } else {
        _turn_server = None;
        // TURN disabled — fall back to whatever --ice-servers says.
        let ice_servers = args.ice_servers.split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| lumen_web::IceServerConfig {
                urls: s.trim().to_string(),
                username: None,
                credential: None,
            })
            .collect();
        (None, ice_servers)
    };

    // ── Session manager ───────────────────────────────────────────────────────
    // Created early so peer_count can be passed to the compositor and audio.
    let session_manager = lumen_webrtc::SessionManager::new(lumen_webrtc::SessionConfig {
        turn: turn_client_config,
        bind_addr: "0.0.0.0:0".parse()?,
    });
    let peer_count = session_manager.peer_count();

    // ── Resolve effective DRI node ────────────────────────────────────────────
    // If --dri-node was provided we want GPU rendering + VA-API encoding.
    // When not provided, auto-detect the first available render node.
    // However, VA-API can fail at runtime (missing driver, permissions, etc.).
    // When that happens create_encoder() silently falls back to x264, which
    // cannot consume the DMA-BUF frames produced by the GPU renderer — causing
    // an encode error on every frame.  To prevent this mismatch we probe VA-API
    // here, before the compositor is created, and clear the DRI node if VA-API
    // is unavailable so both the compositor and encoder use the CPU path.
    let dri_node = args.dri_node.or_else(detect_dri_node);
    let effective_dri_node: Option<std::path::PathBuf> = if let Some(ref node) = dri_node {
        let probe_config = lumen_encode::EncoderConfig {
            render_node: Some(node.clone()),
            ..Default::default()
        };
        if lumen_encode::probe_vaapi(&probe_config) {
            Some(node.clone())
        } else {
            tracing::warn!(
                node = %node.display(),
                "VA-API unavailable on the requested DRI node; \
                 falling back to CPU (Pixman) rendering and software x264 encoder"
            );
            None
        }
    } else {
        None
    };

    // ── Compositor ────────────────────────────────────────────────────────────
    // Set up a channel so the compositor can notify us when its Wayland socket
    // is ready — used by the --launch task below.
    let (socket_name_tx, socket_name_rx) = std::sync::mpsc::sync_channel::<String>(1);

    // Treat an empty --inner-display as "disabled".
    let inner_display = if args.inner_display.is_empty() {
        None
    } else {
        Some(args.inner_display.clone())
    };

    let mut compositor = lumen_compositor::Compositor::new(lumen_compositor::CompositorConfig {
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
    let (mut audio_capture, audio_rx) = lumen_audio::AudioCapture::new(lumen_audio::AudioConfig {
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
        ..Default::default()
    };

    // Broadcast channel distributes encoded frames to all active WebRTC sessions.
    let (encoded_tx, _) = broadcast::channel::<Arc<lumen_encode::EncodedFrame>>(8);

    // Shared keyframe request flag: set by a drive task, polled by the encoder.
    let keyframe_flag = Arc::new(AtomicBool::new(false));

    // ── Resize channels ───────────────────────────────────────────────────────
    // Web server → resize coordinator (async).
    let (resize_tx, mut resize_rx) = tokio::sync::mpsc::channel::<(u32, u32)>(4);
    // Resize coordinator → encoder task (std channel, non-blocking try_recv).
    let (enc_resize_tx, enc_resize_rx) = std::sync::mpsc::channel::<(u32, u32)>();

    // ── Input forwarding channel ──────────────────────────────────────────────
    // Drive tasks drop events into this channel; a dedicated task drains them
    // into the compositor's input sender.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<lumen_compositor::InputEvent>(256);

    // ── Gamepad channel ───────────────────────────────────────────────────────
    // Gamepad events from the browser are forwarded to the gamepad manager which
    // creates and drives virtual uinput devices.
    let (gamepad_tx, gamepad_rx) =
        tokio::sync::mpsc::channel::<lumen_gamepad::GamepadEvent>(64);

    // ── Spawn: compositor ─────────────────────────────────────────────────────
    std::thread::spawn(move || {
        if let Err(e) = compositor.run() {
            tracing::error!("Compositor: {e:#}");
        }
    });

    // ── Shutdown signal ───────────────────────────────────────────────────────
    // When --launch is used, the child exiting triggers a graceful shutdown of
    // the web server. Without --launch, the server runs until interrupted.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // ── Spawn: --launch / --desktop task ─────────────────────────────────────
    // Resolve the effective launch command and any preset env vars, then wait
    // for the compositor's Wayland socket to be ready and spawn the child.
    // When the child exits, send on shutdown_tx to stop the web server gracefully.
    let effective_launch: Option<(String, &'static [(&'static str, &'static str)])> =
        match (&args.desktop, &args.launch) {
            (Some(preset), Some(cmd)) => Some((cmd.clone(), preset.env_vars())),
            (Some(preset), None) => Some((preset.default_launch_cmd().to_string(), preset.env_vars())),
            (None, Some(cmd)) => Some((cmd.clone(), &[])),
            (None, None) => None,
        };

    let _shutdown_keep_alive = if let Some((launch_cmd, preset_env)) = effective_launch {
        tokio::task::spawn_blocking(move || {
            let socket_name = match socket_name_rx.recv() {
                Ok(s) => s,
                Err(_) => {
                    tracing::error!("launch: compositor exited before socket was ready");
                    let _ = shutdown_tx.send(());
                    return;
                }
            };
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| "/tmp".to_string());
            tracing::info!(cmd = %launch_cmd, wayland_display = %socket_name, "Launching client");
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", &launch_cmd])
                .env("WAYLAND_DISPLAY", &socket_name)
                .env("XDG_RUNTIME_DIR", &runtime_dir)
                .env_remove("DISPLAY");
            // Strip all LUMEN_ vars so secrets don't leak into the child process.
            for (key, _) in std::env::vars() {
                if key.starts_with("LUMEN_") {
                    cmd.env_remove(&key);
                }
            }
            for (key, val) in preset_env {
                cmd.env(key, val);
            }
            match cmd.status() {
                Ok(s) => tracing::info!(cmd = %launch_cmd, status = %s, "Launched client exited; shutting down"),
                Err(e) => tracing::error!(cmd = %launch_cmd, "Failed to launch client: {e:#}"),
            }
            let _ = shutdown_tx.send(());
        });
        // shutdown_tx was moved into the task; no keep-alive needed
        None::<tokio::sync::oneshot::Sender<()>>
    } else {
        // No launch command — keep the sender alive so the server runs forever.
        Some(shutdown_tx)
    };

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
            let mut encoder_width = encoder_config.width;
            let mut encoder_height = encoder_config.height;
            loop {
                // Check for a pending resize before blocking on the next frame.
                match enc_resize_rx.try_recv() {
                    Ok((w, h)) => {
                        tracing::info!("Encoder resizing to {w}x{h}");
                        match encoder.resize(w, h) {
                            Ok(()) => { encoder_width = w; encoder_height = h; }
                            Err(e) => tracing::error!("Encoder resize failed: {e:#}"),
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }

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
                // Skip frames from the old resolution that arrived after a resize.
                if frame.width != encoder_width || frame.height != encoder_height {
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
                            tracing::debug!(encoded_count, keyframe = ef.is_keyframe,
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

    // ── Spawn: gamepad manager ────────────────────────────────────────────────
    // Runs in spawn_blocking because uinput file descriptor writes are
    // synchronous. A channel bridges the async world to the blocking loop.
    {
        let mut rx = gamepad_rx;
        tokio::task::spawn_blocking(move || {
            let mut manager = lumen_gamepad::GamepadManager::new();
            // Convert the tokio receiver to a blocking recv loop.
            while let Some(ev) = rx.blocking_recv() {
                manager.handle_event(ev);
            }
            tracing::debug!("Gamepad manager: channel closed, exiting");
        });
    }

    // ── Spawn: input forwarding task ──────────────────────────────────────────
    {
        let compositor_input_tx = compositor_input_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = input_rx.recv().await {
                match ev {
                    lumen_compositor::InputEvent::ClipboardWrite { text } => {
                        compositor_input_tx.clipboard_write(text);
                    }
                    lumen_compositor::InputEvent::GamepadConnected { index, name, num_axes, num_buttons } => {
                        tracing::debug!("Routing GamepadConnected: index={index} name={name:?} axes={num_axes} buttons={num_buttons}");
                        if gamepad_tx.send(lumen_gamepad::GamepadEvent::Connected {
                            index, name, num_axes, num_buttons,
                        }).await.is_err() {
                            tracing::warn!("Gamepad manager channel closed; dropping GamepadConnected for index={index}");
                        }
                    }
                    lumen_compositor::InputEvent::GamepadDisconnected { index } => {
                        tracing::debug!("Routing GamepadDisconnected: index={index}");
                        let _ = gamepad_tx.send(lumen_gamepad::GamepadEvent::Disconnected { index }).await;
                    }
                    lumen_compositor::InputEvent::GamepadButton { index, button, value, pressed } => {
                        let _ = gamepad_tx.send(lumen_gamepad::GamepadEvent::Button {
                            index, button, value, pressed,
                        }).await;
                    }
                    lumen_compositor::InputEvent::GamepadAxis { index, axis, value } => {
                        let _ = gamepad_tx.send(lumen_gamepad::GamepadEvent::Axis {
                            index, axis, value,
                        }).await;
                    }
                    other => {
                        compositor_input_tx.send(other);
                    }
                }
            }
        });
    }

    // ── Spawn: resize coordinator task ────────────────────────────────────────
    // Receives (width, height) from the web layer, fans the command out to the
    // compositor and the encoder, then triggers a keyframe at the new size.
    {
        let compositor_input_tx = compositor_input_tx.clone();
        let keyframe_flag = keyframe_flag.clone();
        tokio::spawn(async move {
            while let Some((w, h)) = resize_rx.recv().await {
                tracing::info!("Resize requested: {w}x{h}");
                compositor_input_tx.resize(w, h);
                let _ = enc_resize_tx.send((w, h));
                keyframe_flag.store(true, Ordering::Relaxed);
            }
        });
    }

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
    let last_cursor_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    {
        let session_manager = session_manager.clone();
        let mut cursor_rx = cursor_rx;
        let last_cursor_json = last_cursor_json.clone();
        tokio::spawn(async move {
            loop {
                let ev = match cursor_rx.recv().await {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                let json = cursor_event_to_json(&ev);
                *last_cursor_json.lock().await = Some(json.clone());
                session_manager.broadcast_dc_message(json).await;
            }
        });
    }

    // ── Spawn: clipboard fan-out task ─────────────────────────────────────────
    let last_clipboard_json: Arc<tokio::sync::Mutex<Option<Vec<u8>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    {
        let session_manager = session_manager.clone();
        let mut clipboard_rx = clipboard_rx;
        let last_clipboard_json = last_clipboard_json.clone();
        tokio::spawn(async move {
            loop {
                let ev = match clipboard_rx.recv().await {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                let json_opt = clipboard_event_to_json(&ev);
                *last_clipboard_json.lock().await = json_opt.clone();
                if let Some(json) = json_opt {
                    session_manager.broadcast_dc_message(json).await;
                }
            }
        });
    }

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
        ice_servers: ice_server_list,
        shutdown_signal: Some(shutdown_rx),
        tls_cert: args.tls_cert,
        tls_key: args.tls_key,
    })
    .run()
    .await?;

    // The web server has shut down (either via graceful shutdown signal or
    // by returning normally).  Stop the compositor, which closes the broadcast
    // channels and causes the encoder to exit.  Then force-exit the process to
    // clean up threads that don't have explicit stop signals (audio capture).
    tracing::info!("Web server stopped; shutting down");
    compositor_input_tx.stop();
    std::process::exit(0);
}

/// Construct [`lumen_web::AuthConfig`] from parsed CLI arguments.
///
/// Validates that all required OAuth2 flags are present when `--auth oauth2`
/// is selected and returns a descriptive error otherwise.
fn build_auth_config(args: &Args) -> Result<lumen_web::AuthConfig> {
    match args.auth {
        AuthMode::None => Ok(lumen_web::AuthConfig::None),
        AuthMode::Basic => Ok(lumen_web::AuthConfig::Basic),
        AuthMode::Bearer => {
            let token = args
                .auth_bearer_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!(
                    "--auth-bearer-token / LUMEN_AUTH_BEARER_TOKEN is required when --auth bearer is set"
                ))?;
            Ok(lumen_web::AuthConfig::Bearer { token })
        }
        AuthMode::Oauth2 => {
            fn require<T: Clone>(
                val: &Option<T>,
                flag: &str,
            ) -> Result<T> {
                val.clone()
                    .ok_or_else(|| anyhow::anyhow!("{flag} is required when --auth oauth2 is set"))
            }
            Ok(lumen_web::AuthConfig::OAuth2 {
                issuer_url: require(
                    &args.auth_oauth2_issuer_url,
                    "--auth-oauth2-issuer-url / LUMEN_AUTH_OAUTH2_ISSUER_URL",
                )?,
                client_id: require(
                    &args.auth_oauth2_client_id,
                    "--auth-oauth2-client-id / LUMEN_AUTH_OAUTH2_CLIENT_ID",
                )?,
                client_secret: require(
                    &args.auth_oauth2_client_secret,
                    "--auth-oauth2-client-secret / LUMEN_AUTH_OAUTH2_CLIENT_SECRET",
                )?,
                redirect_uri: require(
                    &args.auth_oauth2_redirect_uri,
                    "--auth-oauth2-redirect-uri / LUMEN_AUTH_OAUTH2_REDIRECT_URI",
                )?,
                expected_subject: require(
                    &args.auth_oauth2_subject,
                    "--auth-oauth2-subject / LUMEN_AUTH_OAUTH2_SUBJECT",
                )?,
            })
        }
    }
}


fn cursor_event_to_json(ev: &lumen_compositor::CursorEvent) -> Vec<u8> {
    use lumen_compositor::CursorEvent;
    let json = match ev {
        CursorEvent::Default => br#"{"type":"cursor_update","kind":"default"}"#.to_vec(),
        CursorEvent::Named(css) => {
            format!(r#"{{"type":"cursor_update","kind":"named","css":"{css}"}}"#).into_bytes()
        }
        CursorEvent::Hidden  => br#"{"type":"cursor_update","kind":"hidden"}"#.to_vec(),
        CursorEvent::Image { width, height, hotspot_x, hotspot_y, rgba } => {
            let data = base64::engine::general_purpose::STANDARD.encode(rgba);
            format!(
                r#"{{"type":"cursor_update","kind":"image","w":{width},"h":{height},"hotspot_x":{hotspot_x},"hotspot_y":{hotspot_y},"data":"{data}"}}"#
            ).into_bytes()
        }
    };
    tracing::debug!("cursor -> browser: {}", String::from_utf8_lossy(&json).chars().take(120).collect::<String>());
    json
}

/// Encode a `ClipboardEvent` as a JSON byte string for the data channel.
/// Returns `None` for `Cleared` (no meaningful data to send to the browser).
fn clipboard_event_to_json(ev: &lumen_compositor::ClipboardEvent) -> Option<Vec<u8>> {
    use lumen_compositor::ClipboardEvent;
    match ev {
        ClipboardEvent::Text(text) => {
            let text_json = serde_json::to_string(text).unwrap_or_default();
            Some(format!(r#"{{"type":"clipboard_update","text":{text_json}}}"#).into_bytes())
        }
        ClipboardEvent::Cleared => None,
    }
}

/// Scan `/dev/dri/` for the first `renderD*` node. Returns `None` if none is
/// found (triggers the CPU/Pixman renderer path).
fn detect_dri_node() -> Option<std::path::PathBuf> {
    let mut nodes: Vec<std::path::PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("renderD"))
                .unwrap_or(false)
        })
        .collect();
    nodes.sort();
    if let Some(ref node) = nodes.first() {
        tracing::info!(node = %node.display(), "Auto-detected DRI render node");
    } else {
        tracing::info!("No /dev/dri/renderD* found; using CPU (Pixman) renderer");
    }
    nodes.into_iter().next()
}

/// Detect the machine's preferred outbound IP by making a non-sending UDP
/// "connection" to a public address. No packets are transmitted.
/// Returns `None` if detection fails or the result is a loopback address.
fn detect_outbound_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_loopback() { None } else { Some(ip) }
}
