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
    if ip.is_loopback() {
        None
    } else {
        Some(ip)
    }
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
    anyhow::ensure!(
        !(args.ssh || args.turn_tcp) || args.turn_port != 0,
        "--ssh and --turn-tcp require an enabled TURN server (--turn-port must be nonzero)"
    );
    let bind_ip = args.turn_bind_ip.unwrap_or_else(|| {
        if args.ssh {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        }
    });
    anyhow::ensure!(
        bind_ip.is_ipv4(),
        "TURN currently requires an IPv4 bind address"
    );
    anyhow::ensure!(
        args.turn_external_ip.is_none_or(|ip| ip.is_ipv4()),
        "TURN currently requires an IPv4 relay address"
    );
    // Resolve the TURN external IP: explicit flag > auto-detect > 127.0.0.1.
    let turn_external_ip = args
        .turn_external_ip
        .or_else(|| args.ssh.then_some(std::net::Ipv4Addr::LOCALHOST.into()))
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
                tracing::info!(
                    "TURN credentials not configured; using auto-generated ephemeral credentials"
                );
                creds
            }
        };
        let turn_cfg = lumen_turn::TurnServerConfig {
            listen_port: args.turn_port,
            bind_ip,
            tcp: args.ssh || args.turn_tcp,
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

        let server_addr: SocketAddr = SocketAddr::new(
            if bind_ip.is_unspecified() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                bind_ip
            },
            args.turn_port,
        );

        let client_cfg = lumen_webrtc::types::TurnClientConfig {
            server_addr,
            username: turn_username.clone(),
            password: turn_password.clone(),
            relay_ip: turn_external_ip,
        };

        let turn_url = browser_turn_url(args, turn_external_ip);
        let ice_servers = vec![lumen_web::IceServerConfig {
            urls: turn_url,
            username: Some(turn_username),
            credential: Some(turn_password),
        }];
        Ok(TurnSetup {
            client_config: Some(client_cfg),
            ice_servers,
            _server: Some(server),
        })
    } else {
        // TURN disabled — fall back to whatever --ice-servers says.
        let ice_servers = args
            .ice_servers
            .split(',')
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

/// The forwarded client endpoint and the remote relay address are independent.
fn browser_turn_url(args: &Args, relay_ip: std::net::IpAddr) -> String {
    if args.ssh {
        format!(
            "turn:127.0.0.1:{}?transport=tcp",
            args.ssh_turn_port.unwrap_or(args.turn_port)
        )
    } else {
        format!(
            "turn:{relay_ip}:{}?transport={}",
            args.turn_port,
            if args.turn_tcp { "tcp" } else { "udp" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn browser_endpoint_is_independent_of_relay_address() {
        let relay = std::net::Ipv4Addr::new(10, 0, 0, 2).into();
        let normal = Args::try_parse_from(["lumen"]).expect("default CLI");
        assert_eq!(
            browser_turn_url(&normal, relay),
            "turn:10.0.0.2:3478?transport=udp"
        );
        let ssh =
            Args::try_parse_from(["lumen", "--ssh", "--ssh-turn-port", "13478"]).expect("SSH CLI");
        assert_eq!(
            browser_turn_url(&ssh, relay),
            "turn:127.0.0.1:13478?transport=tcp"
        );
        let tcp = Args::try_parse_from(["lumen", "--turn-tcp"]).expect("TCP CLI");
        assert_eq!(
            browser_turn_url(&tcp, relay),
            "turn:10.0.0.2:3478?transport=tcp"
        );
    }

    #[tokio::test]
    async fn ssh_requires_turn() {
        let args = Args::try_parse_from(["lumen", "--ssh", "--turn-port", "0"]).expect("CLI");
        assert!(setup(&args).await.is_err());
    }

    #[test]
    fn forwarded_port_requires_ssh_and_is_nonzero() {
        assert!(Args::try_parse_from(["lumen", "--ssh-turn-port", "13478"]).is_err());
        assert!(Args::try_parse_from(["lumen", "--ssh", "--ssh-turn-port", "0"]).is_err());
    }
    #[tokio::test]
    async fn session_teardown_releases_internal_turn_allocation() -> anyhow::Result<()> {
        use std::time::Duration;
        use tokio::net::UdpSocket;

        // A single relay port makes leaks observable on the next session.
        let reservation = UdpSocket::bind("127.0.0.1:0").await?;
        let relay_addr = reservation.local_addr()?;
        drop(reservation);
        let server = lumen_turn::TurnServer::start(lumen_turn::TurnServerConfig {
            listen_port: 0,
            bind_ip: std::net::Ipv4Addr::LOCALHOST.into(),
            min_relay_port: relay_addr.port(),
            max_relay_port: relay_addr.port(),
            username: "test".into(),
            password: "test".into(),
            ..Default::default()
        })
        .await?;
        let config = lumen_webrtc::SessionConfig {
            bind_addr: "127.0.0.1:0".parse()?,
            turn: Some(lumen_webrtc::types::TurnClientConfig {
                server_addr: SocketAddr::new(server.config.bind_ip, server.config.listen_port),
                username: "test".into(),
                password: "test".into(),
                relay_ip: server.config.external_ip,
            }),
        };
        let offer = [
            "v=0", "o=- 1 1 IN IP4 127.0.0.1", "s=-", "t=0 0",
            "a=group:BUNDLE 0", "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
            "c=IN IP4 0.0.0.0", "a=mid:0", "a=ice-ufrag:test",
            "a=ice-pwd:012345678901234567890123",
            "a=fingerprint:sha-256 01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01:01",
            "a=setup:actpass", "a=sctp-port:5000", "",
        ].join("\r\n");
        for valid in [true, false, true] {
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                lumen_webrtc::WebRtcSession::new(
                    config.clone(),
                    if valid { &offer } else { "invalid SDP" },
                ),
            )
            .await?;
            if valid {
                let (session, answer) = result?;
                anyhow::ensure!(
                    answer.contains("typ relay"),
                    "session must allocate its relay"
                );
                drop(session);
            } else {
                anyhow::ensure!(result.is_err(), "invalid SDP must fail");
            }
            // Both normal teardown and failed negotiation must release the port.
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match UdpSocket::bind(relay_addr).await {
                        Ok(socket) => break Ok::<_, std::io::Error>(socket),
                        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => break Err(error),
                    }
                }
            })
            .await??;
        }
        Ok(())
    }
}
