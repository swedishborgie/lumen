//! lumen-turn — Embedded TURN relay server for container environments.
//!
//! Starts a TURN/STUN server so that WebRTC peers behind NAT (e.g. inside a
//! Podman/Docker container) can exchange media even when direct ICE candidates
//! are unreachable.  Both the browser and the lumen WebRTC session connect to
//! this server as TURN clients; the server relays traffic between them.
//!
//! # Port requirements
//!
//! * `listen_port` (default 3478) — TURN control channel. Must be mapped:
//!   `podman run -p 3478:3478/udp`
//! * `min_relay_port`..`max_relay_port` — data relay range. Must be mapped:
//!   `podman run -p 50000-50010:50000-50010/udp`

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use turn::auth::{AuthHandler, generate_auth_key};
use turn::relay::relay_range::RelayAddressGeneratorRanges;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use webrtc_util::vnet::net::Net;

/// Configuration for the embedded TURN server.
#[derive(Debug, Clone)]
pub struct TurnServerConfig {
    /// UDP port the TURN server listens on (default: 3478).
    pub listen_port: u16,
    /// IP address advertised to peers as the relay address.
    ///
    /// Set to `127.0.0.1` for localhost-only access (the default, works when
    /// the browser opens the UI via `http://localhost:8080`).  Set to the
    /// host's LAN IP for access from other machines on the network.
    pub external_ip: IpAddr,
    /// Lowest UDP relay port (must be port-mapped when running in a container).
    pub min_relay_port: u16,
    /// Highest UDP relay port (inclusive).
    pub max_relay_port: u16,
    /// TURN credential realm (appears in auth challenges).
    pub realm: String,
    /// TURN username for lumen sessions.
    pub username: String,
    /// TURN password for lumen sessions.
    pub password: String,
}

impl Default for TurnServerConfig {
    fn default() -> Self {
        Self {
            listen_port: 3478,
            external_ip: "127.0.0.1".parse().unwrap(),
            min_relay_port: 50000,
            max_relay_port: 50010,
            realm: "lumen.local".to_string(),
            username: "lumen".to_string(),
            password: "lumen".to_string(),
        }
    }
}

/// A running embedded TURN server.
///
/// Keep this value alive for the duration of the process; dropping it shuts
/// down the server and invalidates all relay allocations.
pub struct TurnServer {
    _server: Server,
    pub config: TurnServerConfig,
}

impl TurnServer {
    /// Start the embedded TURN server with the given configuration.
    pub async fn start(config: TurnServerConfig) -> Result<Self> {
        let listen_addr: SocketAddr =
            format!("0.0.0.0:{}", config.listen_port).parse()?;

        let udp_socket = UdpSocket::bind(listen_addr)
            .await
            .with_context(|| format!("Failed to bind TURN UDP socket on {listen_addr}"))?;

        let auth_key = generate_auth_key(
            &config.username,
            &config.realm,
            &config.password,
        );
        let auth = Arc::new(StaticAuthHandler {
            username: config.username.clone(),
            key: auth_key,
        });

        let server = Server::new(ServerConfig {
            conn_configs: vec![ConnConfig {
                conn: Arc::new(udp_socket),
                relay_addr_generator: Box::new(RelayAddressGeneratorRanges {
                    relay_address: config.external_ip,
                    min_port: config.min_relay_port,
                    max_port: config.max_relay_port,
                    max_retries: 10,
                    address: "0.0.0.0".to_string(),
                    net: Arc::new(Net::new(None)),
                }),
            }],
            realm: config.realm.clone(),
            auth_handler: auth,
            channel_bind_timeout: Duration::ZERO,
            alloc_close_notify: None,
        })
        .await
        .context("Failed to start TURN server")?;

        tracing::info!(
            port = config.listen_port,
            external_ip = %config.external_ip,
            relay_range = %format!("{}-{}", config.min_relay_port, config.max_relay_port),
            "TURN server started"
        );

        Ok(Self {
            _server: server,
            config,
        })
    }

    /// Returns the TURN URL to advertise to browsers.
    ///
    /// The `host` parameter is the hostname or IP the browser uses to reach
    /// this container (e.g. `localhost` or the host's LAN IP).
    pub fn turn_url(&self, host: &str) -> String {
        format!("turn:{}:{}", host, self.config.listen_port)
    }
}

/// Simple static credential handler — accepts one username/key pair.
struct StaticAuthHandler {
    username: String,
    key: Vec<u8>,
}

impl AuthHandler for StaticAuthHandler {
    fn auth_handle(
        &self,
        username: &str,
        _realm: &str,
        _src: SocketAddr,
    ) -> Result<Vec<u8>, turn::Error> {
        if username == self.username {
            Ok(self.key.clone())
        } else {
            Err(turn::Error::ErrNoSuchUser)
        }
    }
}
