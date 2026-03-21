use std::net::SocketAddr;

use anyhow::Result;
use base64::Engine as _;

use crate::cli::Args;

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

/// Detect the machine's preferred outbound IP by making a non-sending UDP
/// "connection" to a public address. No packets are transmitted.
/// Returns `None` if detection fails or the result is a loopback address.
fn detect_outbound_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_loopback() { None } else { Some(ip) }
}

/// Result of setting up the TURN/ICE configuration.
pub struct TurnSetup {
    /// TURN client config to pass to the WebRTC session manager.
    /// `None` when the embedded TURN server is disabled (`--turn-port 0`).
    pub client_config: Option<lumen_webrtc::types::TurnClientConfig>,
    /// ICE server list to advertise to browsers via the signaling channel.
    pub ice_servers: Vec<lumen_web::IceServerConfig>,
    /// Keep-alive handle for the embedded TURN server.
    ///
    /// Dropping this shuts down the server and invalidates all relay
    /// allocations.  It must remain bound for the entire lifetime of `main`.
    pub _server: Option<lumen_turn::TurnServer>,
}

/// Start the embedded TURN server (when enabled) and build the ICE server list.
pub async fn setup(args: &Args) -> Result<TurnSetup> {
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

    if args.turn_port > 0 {
        // Resolve credentials: use explicit values if provided, otherwise generate
        // a random ephemeral pair that exists only for the lifetime of this process.
        let (turn_username, turn_password) = match (&args.turn_username, &args.turn_password) {
            (Some(u), Some(p)) => (u.clone(), p.clone()),
            (Some(u), None) => {
                let (_, p) = generate_turn_credentials();
                (u.clone(), p)
            }
            (None, Some(p)) => {
                let (u, _) = generate_turn_credentials();
                (u, p.clone())
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
        let server = lumen_turn::TurnServer::start(turn_cfg).await?;
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
        Ok(TurnSetup {
            client_config: Some(client_cfg),
            ice_servers,
            _server: Some(server),
        })
    } else {
        // TURN disabled — fall back to whatever --ice-servers says.
        let ice_servers = args.ice_servers.split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| lumen_web::IceServerConfig {
                urls: s.trim().to_string(),
                username: None,
                credential: None,
            })
            .collect();
        Ok(TurnSetup {
            client_config: None,
            ice_servers,
            _server: None,
        })
    }
}
