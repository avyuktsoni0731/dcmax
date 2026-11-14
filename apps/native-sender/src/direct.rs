//! Direct picture path. A public STUN server tells each app its own public
//! address. The apps exchange that address over the LiveKit data channel, punch
//! a UDP mapping, then carry the existing picture bytes on a reliable stream.
//! The video does not go through a server when the punch works.

use anyhow::{bail, Context, Result};
use livekit::prelude::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;

pub const TOPIC: &str = "picture-direct";

const COOKIE: u32 = 0x2112_A442;
const PUNCH: &[u8] = b"dcmax-punch";

#[derive(serde::Serialize, serde::Deserialize)]
struct Card {
    id: String,
    addr: String,
    cert: String,
}

struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
}

/// Sender side. Returns a local address the picture sender should connect to.
/// The other app's bytes are copied onto that connection.
pub async fn host(
    participant: LocalParticipant,
    identity: &str,
    incoming: &mut UnboundedReceiver<Vec<u8>>,
) -> Result<SocketAddr> {
    let (socket, mapped) = tokio::task::spawn_blocking(stun_bind)
        .await
        .context("stun task")??;
    let mine = make_cert()?;
    println!("picture direct: this app is at {mapped}");
    let card = Card {
        id: identity.to_string(),
        addr: mapped.to_string(),
        cert: hex(&mine.cert),
    };
    let peer = exchange(&participant, &card, incoming, false).await?;
    let peer_addr = peer.addr.parse().context("other app address")?;
    println!("picture direct: other app is at {peer_addr}");
    let socket = punch_owned(socket, peer_addr).await?;
    let endpoint = server_endpoint(socket, &mine)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let local = listener.local_addr()?;
    tokio::spawn(async move {
        let accepted = listener.accept();
        let quic = async {
            let incoming = endpoint
                .accept()
                .await
                .context("picture direct: the watcher did not connect")?;
            let connection = incoming.await.context("picture direct: handshake failed")?;
            connection
                .accept_bi()
                .await
                .context("picture direct: no stream")
        };
        let (tcp, streams) = tokio::join!(accepted, quic);
        match (tcp, streams) {
            (Ok((tcp, _)), Ok((send, recv))) => bridge(tcp, send, recv).await,
            (Err(err), _) => eprintln!("picture direct: local pipe {err}"),
            (_, Err(err)) => eprintln!("picture direct: {err:#}"),
        }
    });
    Ok(local)
}

/// Watch side. Joins the room for the address exchange, then feeds the picture
/// into the local playback listener.
pub async fn join_watch(config: crate::config::AppConfig, playback: SocketAddr) -> Result<()> {
    let http = reqwest::Client::new();
    api_health(&http, &config).await?;
    let token = crate::api::fetch_token(
        &http,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        &config.client_type,
        config.invite.as_deref(),
    )
    .await?;
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    let (room, mut events) = Room::connect(&token.url, &token.token, options)
        .await
        .context("LiveKit room connect failed")?;
    let (tx, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let RoomEvent::DataReceived { payload, topic, .. } = event {
                if topic.as_deref() == Some(TOPIC) {
                    let _ = tx.send(payload.to_vec());
                }
            }
        }
    });
    let (socket, mapped) = tokio::task::spawn_blocking(stun_bind)
        .await
        .context("stun task")??;
    println!("picture direct: this app is at {mapped}");
    let card = Card {
        id: config.identity.clone(),
        addr: mapped.to_string(),
        cert: String::new(),
    };
    let participant = room.local_participant();
    let peer = exchange(&participant, &card, &mut incoming, true).await?;
    let peer_addr = peer.addr.parse().context("other app address")?;
    let expected = unhex(&peer.cert)?;
    println!("picture direct: other app is at {peer_addr}");
    let socket = punch_owned(socket, peer_addr).await?;
    let mut endpoint = client_endpoint(socket, expected)?;
    let connection = connect_with_retry(&mut endpoint, peer_addr).await?;
    let (send, recv) = connection
        .open_bi()
        .await
        .context("picture direct: stream")?;
    let local = tokio::net::TcpStream::connect(playback)
        .await
        .context("picture direct: playback pipe")?;
    println!("picture direct: connected");
    bridge(local, send, recv).await;
    Ok(())
}

async fn api_health(http: &reqwest::Client, config: &crate::config::AppConfig) -> Result<()> {
    crate::api::health_check(http, &config.api_base_url)
        .await
        .context("token service is not reachable")
}

async fn exchange(
    participant: &LocalParticipant,
    me: &Card,
    incoming: &mut UnboundedReceiver<Vec<u8>>,
    want_cert: bool,
) -> Result<Card> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("the other app did not answer. It needs --picture-direct in the same room");
        }
        let _ = participant
            .publish_data(DataPacket {
                payload: serde_json::to_vec(me)?,
                topic: Some(TOPIC.to_string()),
                reliable: true,
                destination_identities: Vec::new(),
            })
            .await;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            message = incoming.recv() => {
                let Some(message) = message else {
                    bail!("the room closed before the other app answered");
                };
                let Ok(card) = serde_json::from_slice::<Card>(&message) else {
                    continue;
                };
                if card.id == me.id || card.addr.parse::<SocketAddr>().is_err() {
                    continue;
                }
                if want_cert && card.cert.is_empty() {
                    continue;
                }
                return Ok(card);
            }
        }
    }
}

async fn punch_owned(socket: UdpSocket, peer: SocketAddr) -> Result<UdpSocket> {
    tokio::task::spawn_blocking(move || {
        punch(&socket, peer)?;
        Ok(socket)
    })
    .await
    .context("punch task")?
}

fn punch(socket: &UdpSocket, peer: SocketAddr) -> Result<()> {
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    let mut seen = false;
    for _ in 0..25 {
        socket.send_to(PUNCH, peer).ok();
        let mut buf = [0u8; 64];
        if let Ok((size, _)) = socket.recv_from(&mut buf) {
            if &buf[..size] == PUNCH {
                seen = true;
                for _ in 0..4 {
                    socket.send_to(PUNCH, peer).ok();
                }
                break;
            }
        }
    }
    socket.set_nonblocking(true).ok();
    let mut buf = [0u8; 1500];
    while socket.recv_from(&mut buf).is_ok() {}
    socket.set_nonblocking(false).ok();
    if !seen {
        println!("picture direct: no punch packet back yet, trying the connection anyway");
    }
    Ok(())
}

async fn connect_with_retry(
    endpoint: &mut quinn::Endpoint,
    peer: SocketAddr,
) -> Result<quinn::Connection> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let connecting = endpoint
            .connect(peer, "dcmax")
            .context("picture direct: connect")?;
        match connecting.await {
            Ok(connection) => return Ok(connection),
            Err(err) if tokio::time::Instant::now() < deadline => {
                eprintln!("picture direct: handshake retry ({err})");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(err) => return Err(err).context("picture direct: handshake failed"),
        }
    }
}

async fn bridge(local: tokio::net::TcpStream, mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    local.set_nodelay(true).ok();
    let (mut reader, mut writer) = local.into_split();
    let mut outbound = vec![0u8; 32 * 1024];
    let mut inbound = vec![0u8; 32 * 1024];
    loop {
        tokio::select! {
            read = reader.read(&mut outbound) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        if send.write_all(&outbound[..size]).await.is_err() {
                            break;
                        }
                    }
                }
            }
            read = recv.read(&mut inbound) => {
                match read {
                    Ok(Some(size)) if size > 0 => {
                        if writer.write_all(&inbound[..size]).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
}

fn stun_bind() -> Result<(UdpSocket, SocketAddr)> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("udp bind")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .ok();
    let mut last = anyhow::anyhow!("no STUN server answered");
    for server in ["stun.l.google.com:19302", "stun.cloudflare.com:3478"] {
        match stun_query(&socket, server) {
            Ok(mapped) => return Ok((socket, mapped)),
            Err(err) => last = err,
        }
    }
    Err(last)
}

fn stun_query(socket: &UdpSocket, server: &str) -> Result<SocketAddr> {
    let server = server
        .to_socket_addrs_first()
        .with_context(|| format!("resolve {server}"))?;
    let txid = transaction_id();
    socket
        .send_to(&binding_request(&txid), server)
        .with_context(|| format!("stun {server}"))?;
    let mut buf = [0u8; 1500];
    let (size, _) = socket
        .recv_from(&mut buf)
        .with_context(|| format!("stun {server} timed out"))?;
    let mapped = parse_xor_mapped(&buf[..size])?;
    if buf.len() >= 20 && buf[8..20] != txid {
        bail!("stun response was for a different request");
    }
    Ok(mapped)
}

trait FirstAddr {
    fn to_socket_addrs_first(&self) -> Result<SocketAddr>;
}

impl FirstAddr for str {
    fn to_socket_addrs_first(&self) -> Result<SocketAddr> {
        use std::net::ToSocketAddrs;
        self.to_socket_addrs()
            .with_context(|| format!("resolve {self}"))?
            .next()
            .context("no address")
    }
}

fn binding_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut packet = [0u8; 20];
    packet[1] = 0x01;
    packet[4..8].copy_from_slice(&COOKIE.to_be_bytes());
    packet[8..20].copy_from_slice(txid);
    packet
}

fn transaction_id() -> [u8; 12] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .unwrap_or(1);
    let mut id = [0u8; 12];
    id[..8].copy_from_slice(&nanos.to_le_bytes()[..8]);
    id[8] = 0xdc;
    id
}

pub(crate) fn parse_xor_mapped(packet: &[u8]) -> Result<SocketAddr> {
    if packet.len() < 20 {
        bail!("short stun response");
    }
    let message_type = u16::from_be_bytes([packet[0], packet[1]]);
    if message_type != 0x0101 {
        bail!("not a binding response");
    }
    let cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if cookie != COOKIE {
        bail!("bad stun cookie");
    }
    let txid = &packet[8..20];
    let message_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    let end = 20 + message_len;
    if packet.len() < end {
        bail!("truncated stun response");
    }
    let mut offset = 20;
    while offset + 4 <= end {
        let attribute = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let len = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        let value = offset + 4;
        if value + len > end {
            bail!("bad stun attribute");
        }
        if attribute == 0x0020 {
            return decode_xor(&packet[value..value + len], cookie, txid);
        }
        let padded = len + (4 - len % 4) % 4;
        offset = value + padded;
    }
    bail!("stun response had no mapped address")
}

fn decode_xor(value: &[u8], cookie: u32, txid: &[u8]) -> Result<SocketAddr> {
    if value.len() < 4 {
        bail!("short mapped address");
    }
    let family = value[1];
    let xport = u16::from_be_bytes([value[2], value[3]]);
    let port = xport ^ ((cookie >> 16) as u16);
    if family == 0x01 {
        if value.len() < 8 {
            bail!("short ipv4 address");
        }
        let raw = u32::from_be_bytes([value[4], value[5], value[6], value[7]]);
        return Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(raw ^ cookie)), port));
    }
    if family == 0x02 {
        if value.len() < 20 || txid.len() < 12 {
            bail!("short ipv6 address");
        }
        let mut mask = [0u8; 16];
        mask[..4].copy_from_slice(&cookie.to_be_bytes());
        mask[4..16].copy_from_slice(&txid[..12]);
        let mut addr = [0u8; 16];
        for (index, byte) in addr.iter_mut().enumerate() {
            *byte = value[4 + index] ^ mask[index];
        }
        return Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), port));
    }
    bail!("unknown address family")
}

fn make_cert() -> Result<Identity> {
    let certified = rcgen::generate_simple_self_signed(vec!["dcmax".to_string()])
        .context("picture certificate")?;
    Ok(Identity {
        cert: certified.cert.der().to_vec(),
        key: certified.key_pair.serialize_der(),
    })
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    Arc::new(transport)
}

fn server_endpoint(socket: UdpSocket, identity: &Identity) -> Result<quinn::Endpoint> {
    let provider = provider();
    let cert = CertificateDer::from(identity.cert.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone()));
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls versions")?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("tls certificate")?;
    crypto.alpn_protocols = vec![b"dcmax".to_vec()];
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
            .map_err(|err| anyhow::anyhow!("quic server: {err}"))?,
    ));
    server.transport_config(transport());
    quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server),
        socket,
        quinn::default_runtime().context("quic runtime")?,
    )
    .context("quic endpoint")
}

fn client_endpoint(socket: UdpSocket, expected: Vec<u8>) -> Result<quinn::Endpoint> {
    let provider = provider();
    let verifier = Arc::new(PinCert {
        expected,
        provider: Arc::clone(&provider),
    });
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls versions")?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"dcmax".to_vec()];
    let mut client = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|err| anyhow::anyhow!("quic client: {err}"))?,
    ));
    client.transport_config(transport());
    let mut endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        socket,
        quinn::default_runtime().context("quic runtime")?,
    )
    .context("quic endpoint")?;
    endpoint.set_default_client_config(client);
    Ok(endpoint)
}

#[derive(Debug)]
struct PinCert {
    expected: Vec<u8>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() != self.expected.as_slice() {
            return Err(rustls::Error::General(
                "picture certificate does not match the one announced in the room".into(),
            ));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    if text.len() % 2 != 0 {
        bail!("bad certificate");
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index])?;
        let low = hex_value(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("bad certificate"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stun_xor_mapped_address_decodes() {
        let mut packet = vec![
            0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xe2, 0x42, 0xea, 0x12, 0xd5, 0x48,
        ];
        packet[8] = 0x11;
        let mapped = parse_xor_mapped(&packet).unwrap();
        assert_eq!(mapped, "203.0.113.10:50000".parse().unwrap());
    }

    #[tokio::test]
    async fn quic_stream_delivers_a_frame() {
        let identity = make_cert().unwrap();
        let server_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server_socket.local_addr().unwrap();
        let expected = identity.cert.clone();
        let server = tokio::spawn(async move {
            let endpoint = server_endpoint(server_socket, &identity).unwrap();
            let incoming = endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let mut buf = [0u8; 5];
            recv.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"frame");
            send.write_all(b"audio").await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        let mut endpoint = client_endpoint(client_socket, expected).unwrap();
        let connection = connect_with_retry(&mut endpoint, server_addr)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"frame").await.unwrap();
        let mut buf = [0u8; 5];
        recv.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"audio");
        server.await.unwrap();
    }
}
