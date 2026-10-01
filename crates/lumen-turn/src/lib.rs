//! lumen-turn — Embedded TURN relay server for container environments.
//!
//! Starts a TURN/STUN server so that WebRTC peers behind NAT (e.g. inside a
//! Podman/Docker container) can exchange media even when direct ICE candidates
//! are unreachable.  Both the browser and the lumen WebRTC session connect to
//! this server as TURN clients; the server relays traffic between them.
//!
//! # Port requirements
//!
//! Ordinary remote access requires the UDP listener and relay range to be
//! reachable. With `tcp` enabled, the listener also accepts TURN over TCP.
//! SSH deployments forward only HTTP and the TURN TCP listener; UDP stays
//! inside the remote host/container. See `dev-docs/ssh-forwarding.md`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use turn::auth::{generate_auth_key, AuthHandler};
use turn::relay::relay_range::RelayAddressGeneratorRanges;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use webrtc_util::vnet::net::Net;

mod tcp;

/// Errors starting the embedded TURN listeners.
#[derive(Debug, thiserror::Error)]
pub enum TurnError {
    /// A socket could not be opened.
    #[error("TURN socket: {0}")]
    Io(#[from] std::io::Error),
    /// The TURN library rejected the configuration.
    #[error("TURN server: {0}")]
    Server(#[from] turn::Error),
    /// The relay port range is invalid.
    #[error("TURN relay range must be nonzero and ordered")]
    InvalidRelayRange,
    /// This integration currently uses IPv4 relay sockets.
    #[error("TURN listener and relay addresses must be IPv4")]
    InvalidAddressFamily,
}

/// Configuration for the embedded TURN server.
#[derive(Debug, Clone)]
pub struct TurnServerConfig {
    /// UDP and optional TCP listener port (default: 3478; zero chooses a free port).
    pub listen_port: u16,
    /// Listener bind IP; use loopback for host-only access.
    pub bind_ip: IpAddr,
    /// Enable TCP on the same port, in addition to UDP.
    pub tcp: bool,
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
            bind_ip: std::net::Ipv4Addr::UNSPECIFIED.into(),
            tcp: false,
            external_ip: std::net::Ipv4Addr::LOCALHOST.into(),
            min_relay_port: 50000,
            max_relay_port: 50010,
            realm: "lumen.local".to_string(),
            // username and password have no meaningful defaults — they must be
            // set by the caller before passing this config to TurnServer::start().
            username: String::new(),
            password: String::new(),
        }
    }
}

/// A running embedded TURN server.
///
/// Keep this value alive for the duration of the process; dropping it shuts
/// down the server and invalidates all relay allocations.
pub struct TurnServer {
    _server: Server,
    tcp_task: Option<tokio::task::JoinHandle<()>>,
    /// Effective configuration, including the selected listener port.
    pub config: TurnServerConfig,
}

impl TurnServer {
    /// Start the embedded TURN server with the given configuration.
    pub async fn start(mut config: TurnServerConfig) -> Result<Self, TurnError> {
        if config.min_relay_port == 0 || config.min_relay_port > config.max_relay_port {
            return Err(TurnError::InvalidRelayRange);
        }
        if !config.bind_ip.is_ipv4() || !config.external_ip.is_ipv4() {
            return Err(TurnError::InvalidAddressFamily);
        }
        let udp_socket =
            UdpSocket::bind(SocketAddr::new(config.bind_ip, config.listen_port)).await?;
        config.listen_port = udp_socket.local_addr()?.port();
        // Bind both listeners before spawning anything so startup failures are atomic.
        let tcp_listener = if config.tcp {
            Some(
                tokio::net::TcpListener::bind(SocketAddr::new(config.bind_ip, config.listen_port))
                    .await?,
            )
        } else {
            None
        };
        let server = start_server(&config, Arc::new(udp_socket)).await?;
        let tcp_task =
            tcp_listener.map(|listener| tokio::spawn(tcp::serve(listener, config.clone())));
        tracing::info!(
            port = config.listen_port,
            tcp = config.tcp,
            "TURN server started"
        );
        Ok(Self {
            _server: server,
            tcp_task,
            config,
        })
    }

    /// Returns the TURN URL to advertise to browsers.
    ///
    /// The `host` parameter is the hostname or IP the browser uses to reach
    /// this container (e.g. `localhost` or the host's LAN IP).
    pub fn turn_url(&self, host: &str) -> String {
        format!(
            "turn:{}:{}?transport={}",
            host,
            self.config.listen_port,
            if self.config.tcp { "tcp" } else { "udp" }
        )
    }
}

impl Drop for TurnServer {
    fn drop(&mut self) {
        if let Some(task) = &self.tcp_task {
            task.abort();
        }
    }
}

async fn start_server(
    config: &TurnServerConfig,
    conn: Arc<dyn webrtc_util::Conn + Send + Sync>,
) -> Result<Server, turn::Error> {
    Server::new(ServerConfig {
        conn_configs: vec![ConnConfig {
            conn,
            relay_addr_generator: Box::new(RelayAddressGeneratorRanges {
                relay_address: config.external_ip,
                min_port: config.min_relay_port,
                max_port: config.max_relay_port,
                max_retries: 10,
                address: config.bind_ip.to_string(),
                net: Arc::new(Net::new(None)),
            }),
        }],
        realm: config.realm.clone(),
        auth_handler: Arc::new(StaticAuthHandler {
            username: config.username.clone(),
            key: generate_auth_key(&config.username, &config.realm, &config.password),
        }),
        channel_bind_timeout: Duration::ZERO,
        alloc_close_notify: None,
    })
    .await
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
