//! TURN stream framing. Each TCP connection owns an independent TURN server.
use std::{any::Any, io, net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    sync::{watch, Mutex},
    task::JoinSet,
};
use webrtc_util::Conn;

use crate::TurnServerConfig;

// turn 0.17 uses a 1500-byte receive buffer. Reject larger frames explicitly
// rather than truncating them or allowing the stream to lose synchronization.
const MAX_FRAME: usize = 1500;
const MAX_CONNECTIONS: usize = 128;
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read exactly one STUN or ChannelData frame, consuming TCP alignment padding.
async fn read_frame(reader: &mut (impl AsyncRead + Unpin), buf: &mut [u8]) -> io::Result<usize> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).await?;
    let payload = u16::from_be_bytes([header[2], header[3]]) as usize;
    let channel = match header[0] {
        0x00..=0x3f => false,
        0x40..=0x4f => true,
        _ => return Err(invalid("invalid TURN frame type")),
    };
    if !channel && !payload.is_multiple_of(4) {
        return Err(invalid("unaligned STUN message"));
    }
    let len = payload + if channel { 4 } else { 20 };
    if len > MAX_FRAME || len > buf.len() {
        return Err(invalid("TURN frame exceeds receive buffer"));
    }
    buf[..4].copy_from_slice(&header);
    reader.read_exact(&mut buf[4..len]).await?;
    if channel {
        let mut padding = [0; 3];
        reader
            .read_exact(&mut padding[..(4 - payload % 4) % 4])
            .await?;
    } else if buf[4..8] != [0x21, 0x12, 0xa4, 0x42] {
        return Err(invalid("invalid STUN magic cookie"));
    }
    Ok(len)
}

pub(crate) struct TcpConn {
    reader: Mutex<OwnedReadHalf>,
    writer: Mutex<OwnedWriteHalf>,
    local: SocketAddr,
    peer: SocketAddr,
    closed: watch::Sender<bool>,
}

impl TcpConn {
    pub(crate) fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?;
        let peer = stream.peer_addr()?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            local,
            peer,
            closed: watch::channel(false).0,
        })
    }

    async fn until_closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    async fn receive(&self, buf: &mut [u8]) -> io::Result<usize> {
        tokio::select! {
            _ = self.until_closed() => Err(io::ErrorKind::ConnectionAborted.into()),
            result = tokio::time::timeout(IDLE_TIMEOUT, async {
                read_frame(&mut *self.reader.lock().await, buf).await
            }) => result.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?,
        }
    }

    async fn transmit(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() < 4 || buf.len() > MAX_FRAME {
            return Err(invalid("invalid outbound TURN frame length"));
        }
        tokio::select! {
            _ = self.until_closed() => Err(io::ErrorKind::ConnectionAborted.into()),
            result = tokio::time::timeout(Duration::from_secs(10), async {
                let mut writer = self.writer.lock().await;
                writer.write_all(buf).await?;
                if (0x40..=0x4f).contains(&buf[0]) {
                    writer.write_all(&[0; 3][..(4 - buf.len() % 4) % 4]).await?;
                }
                Ok(buf.len())
            }) => result.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?,
        }
    }
}

#[async_trait]
impl Conn for TcpConn {
    async fn connect(&self, addr: SocketAddr) -> webrtc_util::Result<()> {
        if addr != self.peer {
            return Err(invalid("TCP peer cannot change").into());
        }
        Ok(())
    }
    async fn recv(&self, buf: &mut [u8]) -> webrtc_util::Result<usize> {
        let result = self.receive(buf).await;
        if result.is_err() {
            self.closed.send_replace(true);
        }
        Ok(result?)
    }
    async fn recv_from(&self, buf: &mut [u8]) -> webrtc_util::Result<(usize, SocketAddr)> {
        Ok((self.recv(buf).await?, self.peer))
    }
    async fn send(&self, buf: &[u8]) -> webrtc_util::Result<usize> {
        let result = self.transmit(buf).await;
        if result.is_err() {
            self.closed.send_replace(true);
        }
        Ok(result?)
    }
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> webrtc_util::Result<usize> {
        self.connect(target).await?;
        self.send(buf).await
    }
    fn local_addr(&self) -> webrtc_util::Result<SocketAddr> {
        Ok(self.local)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        Some(self.peer)
    }
    async fn close(&self) -> webrtc_util::Result<()> {
        self.closed.send_replace(true);
        self.writer.lock().await.shutdown().await?;
        Ok(())
    }
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

pub(crate) async fn serve(listener: TcpListener, config: TurnServerConfig) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept() => match result {
                Ok((stream, peer)) => {
                    if connections.len() >= MAX_CONNECTIONS {
                        tracing::warn!(%peer, "TURN TCP connection limit reached");
                        continue;
                    }
                    let config = config.clone();
                    connections.spawn(async move {
                        let conn = match TcpConn::new(stream) {
                            Ok(conn) => Arc::new(conn),
                            Err(error) => { tracing::warn!(%error, "TURN TCP setup failed"); return; }
                        };
                        match crate::start_server(&config, conn.clone()).await {
                            Ok(server) => {
                                conn.until_closed().await;
                                let _ = server.close().await;
                            }
                            Err(error) => tracing::warn!(%error, "TURN TCP server failed"),
                        }
                    });
                }
                Err(error) => { tracing::error!(%error, "TURN TCP listener failed"); break; }
            },
            _ = connections.join_next(), if !connections.is_empty() => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn stun() -> Vec<u8> {
        let mut packet = vec![0; 20];
        packet[1] = 1;
        packet[4..8].copy_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
        packet
    }

    #[tokio::test]
    async fn fragmented_and_coalesced_frames() -> io::Result<()> {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        let expected = stun();
        let packet = expected.clone();
        let task = tokio::spawn(async move {
            for byte in packet {
                writer.write_all(&[byte]).await?;
            }
            writer
                .write_all(&[0x40, 1, 0, 1, 42, 0, 0, 0, 0x40, 2, 0, 0])
                .await
        });
        let mut buf = [0; MAX_FRAME];
        let n = read_frame(&mut reader, &mut buf).await?;
        assert_eq!(&buf[..n], expected);
        assert_eq!(read_frame(&mut reader, &mut buf).await?, 5);
        assert_eq!(buf[4], 42);
        assert_eq!(read_frame(&mut reader, &mut buf).await?, 4);
        task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn rejects_invalid_and_truncated_frames() {
        for packet in [
            vec![0x80, 0, 0, 0],
            vec![0, 1, 0, 1],
            vec![0, 1, 0xff, 0xfc],
            vec![0x40, 1, 0, 1],
            vec![0; 20],
        ] {
            assert!(read_frame(&mut packet.as_slice(), &mut [0; MAX_FRAME])
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn channel_padding_and_buffer_limits() -> io::Result<()> {
        for payload in 0usize..8 {
            let mut packet = vec![0x40, 1, 0, payload as u8];
            packet.resize((4 + payload).next_multiple_of(4), 42);
            let expected_len = 4 + payload;
            let mut input = packet.as_slice();
            assert_eq!(
                read_frame(&mut input, &mut [0; MAX_FRAME]).await?,
                expected_len
            );
            assert!(input.is_empty());
        }
        let mut buf = [0; 19];
        assert!(read_frame(&mut stun().as_slice(), &mut buf).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn writes_channel_padding_and_close_unblocks_read(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let client = TcpStream::connect(listener.local_addr()?).await?;
        let (mut server, _) = listener.accept().await?;
        let conn = TcpConn::new(client)?;
        conn.send(&[0x40, 1, 0, 1, 42]).await?;
        let mut wire = [0; 8];
        server.read_exact(&mut wire).await?;
        assert_eq!(wire, [0x40, 1, 0, 1, 42, 0, 0, 0]);
        conn.close().await?;
        assert!(conn.recv(&mut [0; MAX_FRAME]).await.is_err());
        Ok(())
    }
    #[tokio::test]
    async fn authenticated_tcp_and_udp_relay_round_trips() -> Result<(), Box<dyn std::error::Error>>
    {
        tokio::time::timeout(Duration::from_secs(10), async {
            let server = crate::TurnServer::start(TurnServerConfig {
                listen_port: 0,
                bind_ip: std::net::Ipv4Addr::LOCALHOST.into(),
                tcp: true,
                min_relay_port: 49152,
                max_relay_port: 65535,
                username: "test-user".into(),
                password: "test-password".into(),
                ..Default::default()
            })
            .await?;
            let addr = SocketAddr::new(server.config.bind_ip, server.config.listen_port);
            let tcp: Arc<dyn Conn + Send + Sync> =
                Arc::new(TcpConn::new(TcpStream::connect(addr).await?)?);
            let tcp2: Arc<dyn Conn + Send + Sync> =
                Arc::new(TcpConn::new(TcpStream::connect(addr).await?)?);
            let udp: Arc<dyn Conn + Send + Sync> =
                Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await?);
            // Keep both allocations active to exercise independent allocation managers.
            let mut clients = Vec::new();
            let mut relays = Vec::new();
            for conn in [tcp, tcp2, udp] {
                let client = turn::client::Client::new(turn::client::ClientConfig {
                    stun_serv_addr: addr.to_string(),
                    turn_serv_addr: addr.to_string(),
                    username: server.config.username.clone(),
                    password: server.config.password.clone(),
                    realm: String::new(),
                    software: "lumen-test".into(),
                    rto_in_ms: 100,
                    conn,
                    vnet: None,
                })
                .await?;
                client.listen().await?;
                let relay = client.allocate().await?;
                clients.push(client);
                relays.push(relay);
            }
            let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
            let mut buf = [0; MAX_FRAME];
            for relay in &relays {
                // Repeated packets exercise Send indications and subsequent ChannelData.
                for payload in [b"hello".as_slice(), b"second message", b"third"] {
                    relay.send_to(payload, peer.local_addr()?).await?;
                    let (n, source) = peer.recv_from(&mut buf).await?;
                    assert_eq!(&buf[..n], payload);
                    assert_eq!(source, relay.local_addr()?);
                    peer.send_to(&buf[..n], source).await?;
                    let (n, source) = relay.recv_from(&mut buf).await?;
                    assert_eq!(&buf[..n], payload);
                    assert_eq!(source, peer.local_addr()?);
                }
            }
            for relay in relays {
                relay.close().await?;
            }
            for client in clients {
                client.close().await?;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn server_drop_closes_tcp_connections() -> Result<(), Box<dyn std::error::Error>> {
        let server = crate::TurnServer::start(TurnServerConfig {
            listen_port: 0,
            tcp: true,
            bind_ip: std::net::Ipv4Addr::LOCALHOST.into(),
            username: "u".into(),
            password: "p".into(),
            ..Default::default()
        })
        .await?;
        let mut stream = TcpStream::connect(("127.0.0.1", server.config.listen_port)).await?;
        stream.write_all(&stun()).await?;
        let mut buf = [0; MAX_FRAME];
        // Receiving a binding response proves that the accepted task has started.
        tokio::time::timeout(Duration::from_secs(2), read_frame(&mut stream, &mut buf)).await??;
        drop(server);
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await??;
        assert_eq!(n, 0);
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_releases_allocation() -> Result<(), Box<dyn std::error::Error>> {
        let server = crate::TurnServer::start(TurnServerConfig {
            listen_port: 0,
            tcp: true,
            bind_ip: std::net::Ipv4Addr::LOCALHOST.into(),
            min_relay_port: 49152,
            max_relay_port: 65535,
            username: "u".into(),
            password: "p".into(),
            ..Default::default()
        })
        .await?;
        let addr = SocketAddr::new(server.config.bind_ip, server.config.listen_port);
        let conn = Arc::new(TcpConn::new(TcpStream::connect(addr).await?)?);
        let client = turn::client::Client::new(turn::client::ClientConfig {
            stun_serv_addr: addr.to_string(),
            turn_serv_addr: addr.to_string(),
            username: "u".into(),
            password: "p".into(),
            realm: String::new(),
            software: "test".into(),
            rto_in_ms: 100,
            conn: conn.clone(),
            vnet: None,
        })
        .await?;
        client.listen().await?;
        let relay = tokio::time::timeout(Duration::from_secs(2), client.allocate()).await??;
        let relay_addr = relay.local_addr()?;
        // Close the transport without sending an allocation deletion request.
        conn.close().await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match tokio::net::UdpSocket::bind(relay_addr).await {
                    Ok(socket) => break Ok::<_, io::Error>(socket),
                    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                        tokio::task::yield_now().await
                    }
                    Err(error) => break Err(error),
                }
            }
        })
        .await??;
        let _ = relay.close().await;
        client.close().await?;
        Ok(())
    }
}
