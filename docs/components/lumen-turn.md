# lumen-turn

**Crate**: `crates/lumen-turn`

`lumen-turn` provides an embedded TURN relay server so that WebRTC peers can exchange media even when direct ICE candidates are unreachable (e.g. inside a Podman/Docker container where the host network stack sits between the browser and the lumen process).

Both the browser and the lumen WebRTC session connect to this server as TURN clients. The server relays UDP traffic between them.

## Responsibilities

- Start a UDP TURN/STUN server bound to a configurable port
- Authenticate TURN clients with a single static username/password pair
- Allocate relay ports from a configurable range for each TURN allocation
- Advertise a configurable external IP as the relay address

## Public API

### `TurnServer`

```rust
pub struct TurnServer {
    pub config: TurnServerConfig,
    // ...
}

impl TurnServer {
    pub async fn start(config: TurnServerConfig) -> Result<Self>;

    /// Returns the TURN URL to advertise to browsers.
    /// `host` is the hostname or IP the browser uses to reach this machine.
    pub fn turn_url(&self, host: &str) -> String;
}
```

Keep the returned `TurnServer` alive for the duration of the process. Dropping it shuts down the server and invalidates all active relay allocations.

### `TurnServerConfig`

```rust
pub struct TurnServerConfig {
    pub listen_port: u16,       // Default: 3478
    pub external_ip: IpAddr,    // Relay address advertised to peers; default: 127.0.0.1
    pub min_relay_port: u16,    // Default: 50000
    pub max_relay_port: u16,    // Default: 50010
    pub realm: String,          // Default: "lumen.local"
    pub username: String,       // Default: "lumen"
    pub password: String,       // Default: "lumen"
}
```

## Port Requirements

| Port | Protocol | Purpose |
|------|----------|---------|
| `listen_port` (default 3478) | UDP | TURN control channel and STUN binding requests |
| `min_relay_port`–`max_relay_port` (default 50000–50010) | UDP | Data relay range allocated per TURN client |

When running inside a container, both ranges must be port-mapped:

```sh
podman run -p 3478:3478/udp -p 50000-50010:50000-50010/udp ...
```

## Integration with lumen-webrtc

When the embedded TURN server is enabled, `main.rs`:

1. Starts `TurnServer` with the configured credentials and relay range.
2. Constructs a `TurnClientConfig` pointing at `127.0.0.1:<turn_port>`.
3. Passes the `TurnClientConfig` to `lumen_webrtc::SessionManager` via `SessionConfig`.
4. Sends the TURN URL (using the external IP) to the browser as the ICE server list.

Both sides — lumen's WebRTC session and the browser — allocate a relay address on the same TURN server, and the server relays packets between them.

## Design Notes

- **Static auth**: Only one username/password pair is accepted. The `StaticAuthHandler` implementation computes the HMAC-MD5 key at startup and rejects any other username.
- **Relay range**: The default range (50000–50010) supports up to ~5 simultaneous relay allocations. Widen `min_relay_port`/`max_relay_port` for environments with many concurrent users.
- **Disable with `--turn-port 0`**: Setting the port to zero skips TURN startup entirely; lumen falls back to the `--ice-servers` list for ICE negotiation.
