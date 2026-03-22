use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

/// Log output destination.
#[derive(Clone, Debug)]
pub enum LogOutput {
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
pub enum AuthMode {
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
pub enum DesktopPreset {
    /// labwc — a lightweight wlroots-based Wayland compositor.
    Labwc,
    /// KDE Plasma — starts `kwin_wayland` as a nested compositor on a dedicated
    /// socket (`kwin-wayland`), waits for it to be ready, then launches
    /// `plasmashell` pointed at that socket.  This ensures all KDE apps connect
    /// to kwin's socket rather than directly to the lumen compositor socket.
    /// KDE packages must be installed separately; they are not a lumen dependency.
    Kde,
}

impl DesktopPreset {
    /// Default shell command to launch for this preset (passed to `/bin/sh -c`).
    pub fn default_launch_cmd(&self) -> &'static str {
        match self {
            Self::Labwc => "labwc",
            // Start kwin_wayland with a known socket name so that we can
            // reliably redirect WAYLAND_DISPLAY to it before launching
            // plasmashell.  Using `startplasma-wayland` directly is unreliable
            // in nested mode because it does not guarantee that WAYLAND_DISPLAY
            // is updated to kwin's new socket before child apps are launched,
            // causing them to connect to the outer (lumen) compositor instead.
            Self::Kde => r#"dbus-run-session startplasma-wayland"#,
        }
    }

    /// Environment variables to inject into the child process.
    pub fn env_vars(&self) -> &'static [(&'static str, &'static str)] {
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

fn default_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string())
}

#[derive(Parser, Debug)]
#[command(name = "lumen", about = "Wayland WebRTC streaming compositor", version = env!("LUMEN_VERSION"))]
pub struct Args {
    /// Log output destination. Accepted values: `stderr` (default), `journald`,
    /// or `file:/absolute/path/to/lumen.log`.
    #[arg(long, env = "LUMEN_LOG_OUTPUT", default_value = "stderr")]
    pub log_output: LogOutput,

    /// Syslog identifier to use when logging to journald. Defaults to the binary name.
    /// Set to the systemd unit instance name (e.g. `lumen@alice`) so that
    /// `journalctl -u lumen@alice` collects the right logs.
    #[arg(long, env = "LUMEN_SYSLOG_IDENTIFIER")]
    pub syslog_identifier: Option<String>,

    #[arg(long, env = "LUMEN_BIND", default_value = "0.0.0.0:8080")]
    pub bind_addr: SocketAddr,

    /// Hostname shown in the browser tab title and PWA app name
    /// (default: OS hostname).
    #[arg(long, env = "LUMEN_HOSTNAME", default_value_t = default_hostname())]
    pub hostname: String,
    #[arg(long, env = "LUMEN_WIDTH", default_value_t = 1920)]
    pub width: u32,
    #[arg(long, env = "LUMEN_HEIGHT", default_value_t = 1080)]
    pub height: u32,
    #[arg(long, env = "LUMEN_FPS", default_value_t = 30.0)]
    pub fps: f64,
    #[arg(long, env = "LUMEN_VIDEO_BITRATE_KBPS", default_value_t = 4000)]
    pub video_bitrate_kbps: u32,
    /// Peak bitrate cap in kbps for VBR encoding. Defaults to 2× `--video-bitrate-kbps`.
    #[arg(long, env = "LUMEN_MAX_BITRATE_KBPS")]
    pub max_bitrate_kbps: Option<u32>,
    #[arg(long, env = "LUMEN_AUDIO_BITRATE_BPS", default_value_t = 128_000)]
    pub audio_bitrate_bps: i32,
    #[arg(long, env = "LUMEN_AUDIO_DEVICE")]
    pub audio_device: Option<String>,
    #[arg(long, env = "LUMEN_DRI_NODE")]
    pub dri_node: Option<PathBuf>,
    /// Wayland socket name of a nested inner compositor whose clipboard should be bridged
    /// (e.g. `wayland-inner`). Defaults to `auto`, which scans `$XDG_RUNTIME_DIR` for a
    /// compositor advertising `zwlr_data_control_manager_v1`. Set to an empty string to
    /// disable clipboard bridging entirely.
    #[arg(long, env = "LUMEN_INNER_DISPLAY", default_value = "auto")]
    pub inner_display: String,
    #[arg(long, env = "LUMEN_ICE_SERVERS", default_value = "stun:stun.l.google.com:19302")]
    pub ice_servers: String,

    // ── TURN server ───────────────────────────────────────────────────────────
    /// UDP port for the embedded TURN server. Set to 0 to disable.
    #[arg(long, env = "LUMEN_TURN_PORT", default_value_t = 3478)]
    pub turn_port: u16,
    /// External/public IP of this machine, used as the TURN relay address.
    /// When not set, lumen auto-detects the outbound IP using a routing probe.
    /// Falls back to 127.0.0.1 (localhost-only) if detection fails.
    #[arg(long, env = "LUMEN_TURN_EXTERNAL_IP")]
    pub turn_external_ip: Option<std::net::IpAddr>,
    /// TURN username. When not set, a random credential is generated at startup.
    #[arg(long, env = "LUMEN_TURN_USERNAME")]
    pub turn_username: Option<String>,
    /// TURN password. When not set, a random credential is generated at startup.
    #[arg(long, env = "LUMEN_TURN_PASSWORD", hide_env_values = true)]
    pub turn_password: Option<String>,
    /// Lowest UDP port in the TURN relay range.
    #[arg(long, env = "LUMEN_TURN_MIN_PORT", default_value_t = 50000)]
    pub turn_min_port: u16,
    /// Highest UDP port in the TURN relay range.
    #[arg(long, env = "LUMEN_TURN_MAX_PORT", default_value_t = 50010)]
    pub turn_max_port: u16,

    // ── Authentication ────────────────────────────────────────────────────────
    /// Authentication mode: none (default), basic (PAM), bearer (preshared token), or oauth2 (OIDC).
    #[arg(long, env = "LUMEN_AUTH", default_value = "basic")]
    pub auth: AuthMode,

    /// [bearer] Preshared token for bearer-token authentication.
    /// Every request must include `Authorization: Bearer <token>` with this value.
    /// Intended for use behind a reverse proxy that injects the header.
    #[arg(long, env = "LUMEN_AUTH_BEARER_TOKEN", hide_env_values = true)]
    pub auth_bearer_token: Option<String>,

    /// [oauth2] OIDC issuer URL.  The discovery document is fetched from
    /// `{issuer_url}/.well-known/openid-configuration`.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_ISSUER_URL")]
    pub auth_oauth2_issuer_url: Option<String>,

    /// [oauth2] OAuth2 client ID.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_CLIENT_ID")]
    pub auth_oauth2_client_id: Option<String>,

    /// [oauth2] OAuth2 client secret.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_CLIENT_SECRET", hide_env_values = true)]
    pub auth_oauth2_client_secret: Option<String>,

    /// [oauth2] Full redirect URI registered with the provider,
    /// e.g. `http://localhost:8080/auth/callback`.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_REDIRECT_URI")]
    pub auth_oauth2_redirect_uri: Option<String>,

    /// [oauth2] Expected `sub` claim in the validated ID token; access is
    /// denied if it does not match.
    #[arg(long, env = "LUMEN_AUTH_OAUTH2_SUBJECT")]
    pub auth_oauth2_subject: Option<String>,

    // ── Launch ────────────────────────────────────────────────────────────────
    /// Named desktop environment preset to launch.  Accepted values: `labwc`
    /// (default when set via the service unit), `kde`.  Each preset provides a
    /// default launch command and the environment variables required by that
    /// desktop.  `--launch` / `LUMEN_LAUNCH` overrides the launch command but
    /// the preset's env vars are still applied.
    #[arg(long, env = "LUMEN_DESKTOP")]
    pub desktop: Option<DesktopPreset>,

    /// Shell command to launch as a Wayland client once the compositor socket
    /// is ready.  Passed to `/bin/sh -c`, so arguments and shell syntax are
    /// supported (e.g. `--launch "labwc"` or `--launch "weston --backend=wayland"`).
    /// The child receives `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`; `DISPLAY` is
    /// unset so it connects via Wayland rather than X11.
    /// When `--desktop` is also set, this overrides only the launch command;
    /// the preset's required environment variables are still applied.
    #[arg(long, env = "LUMEN_LAUNCH")]
    pub launch: Option<String>,

    // ── TLS ───────────────────────────────────────────────────────────────────
    /// Path to a PEM-encoded TLS certificate chain. When both `--tls-cert` and
    /// `--tls-key` are provided the server binds an HTTPS endpoint instead of
    /// plain HTTP. Both arguments must be supplied together.
    #[arg(long, env = "LUMEN_TLS_CERT")]
    pub tls_cert: Option<PathBuf>,

    /// Path to a PEM-encoded TLS private key. Must be provided together with
    /// `--tls-cert`.
    #[arg(long, env = "LUMEN_TLS_KEY")]
    pub tls_key: Option<PathBuf>,
}

impl Args {
    /// Resolve the effective launch command and preset env vars from `--desktop`
    /// and `--launch` flags.  Returns `None` when neither is set.
    pub fn effective_launch(&self) -> Option<(String, &'static [(&'static str, &'static str)])> {
        match (&self.desktop, &self.launch) {
            (Some(preset), Some(cmd)) => Some((cmd.clone(), preset.env_vars())),
            (Some(preset), None) => Some((preset.default_launch_cmd().to_string(), preset.env_vars())),
            (None, Some(cmd)) => Some((cmd.clone(), &[])),
            (None, None) => None,
        }
    }
}

/// Construct [`lumen_web::AuthConfig`] from parsed CLI arguments.
///
/// Validates that all required OAuth2 flags are present when `--auth oauth2`
/// is selected and returns a descriptive error otherwise.
pub fn build_auth_config(args: &Args) -> Result<lumen_web::AuthConfig> {
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
