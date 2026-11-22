//! Direct picture path. A public STUN server tells each app its own public
//! address. The apps exchange that address over the LiveKit data channel, punch
//! a UDP mapping, then carry the existing picture bytes on a reliable stream.
//! The video does not go through a server when the punch works.
//!
//! Two networks that cannot punch still need a picture. The same bytes then
//! ride the room that already carries the microphone, in order, and the watcher
//! plays them through the same local player.

use anyhow::{bail, Context, Result};
use livekit::prelude::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;

pub const TOPIC: &str = "picture-direct";
/// Picture bytes when the two computers cannot punch a direct path.
pub const ROOM_TOPIC: &str = "picture-room";

const COOKIE: u32 = 0x2112_A442;
const PUNCH: &[u8] = b"dcmax-punch";

#[derive(serde::Serialize, serde::Deserialize)]
struct Card {
    id: String,
    /// "host" publishes the picture. "watch" receives it. Empty means an older app.
    #[serde(default)]
    role: String,
    #[serde(default)]
    addr: String,
    #[serde(default)]
    addrs: Vec<String>,
    cert: String,
}

impl Card {
    fn targets(&self) -> Vec<SocketAddr> {
        let mut out: Vec<SocketAddr> = self.addrs.iter().filter_map(|item| item.parse().ok()).collect();
        if let Ok(one) = self.addr.parse() {
            if !out.contains(&one) {
                out.push(one);
            }
        }
        out.sort_by_key(rank);
        out.dedup();
        out
    }
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
    incoming: UnboundedReceiver<Vec<u8>>,
) -> Result<SocketAddr> {
    let (socket, candidates) = tokio::task::spawn_blocking(prepare)
        .await
        .context("address lookup")??;
    let mine = make_cert()?;
    log_line(&format!("picture direct: this app is at {}", join_addrs(&candidates)));
    let endpoint = server_endpoint(socket, &mine)?;
    log_line("picture direct: listening");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let local = listener.local_addr()?;
    let card = Card {
        id: identity.to_string(),
        role: "host".to_string(),
        addr: candidates.first().map(|addr| addr.to_string()).unwrap_or_default(),
        addrs: candidates.iter().map(|addr| addr.to_string()).collect(),
        cert: hex(&mine.cert),
    };
    let payload = serde_json::to_vec(&card)?;
    let (alive_tx, alive_rx) = tokio::sync::watch::channel(true);
    let (announce_tx, announce_rx) = tokio::sync::watch::channel(true);
    let announcer = participant.clone();
    tokio::spawn(async move {
        let mut incoming = incoming;
        let announce_rx = announce_rx;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if !*announce_rx.borrow() {
                        continue;
                    }
                    let _ = announcer
                        .publish_data(DataPacket {
                            payload: payload.clone(),
                            topic: Some(TOPIC.to_string()),
                            reliable: true,
                            destination_identities: Vec::new(),
                        })
                        .await;
                }
                message = incoming.recv() => {
                    if message.is_none() {
                        let _ = alive_tx.send(false);
                        break;
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        serve(endpoint, listener, alive_rx, announce_tx, participant).await;
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
    let (tx, incoming) = tokio::sync::mpsc::unbounded_channel();
    let (room_tx, room_rx) = tokio::sync::mpsc::unbounded_channel();
    let room_playback = playback;
    tokio::spawn(async move {
        play_room(room_rx, room_playback, Arc::new(AtomicBool::new(false))).await;
    });
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let RoomEvent::DataReceived { payload, topic, participant, .. } = event {
                if topic.as_deref() == Some(TOPIC) {
                    let _ = tx.send(payload.to_vec());
                } else if topic.as_deref() == Some(ROOM_TOPIC) {
                    let from = participant
                        .as_ref()
                        .map(|person| person.identity().to_string())
                        .unwrap_or_default();
                    let _ = room_tx.send((from, payload.to_vec()));
                }
            }
        }
    });
    let participant = room.local_participant();
    follow(participant, &config.identity, incoming, playback).await
}

/// Watch side on a room that is already connected. `playback` is the local
/// picture listener. A host card from the same identity is ignored, so this
/// can run beside [`host`] in one process.
pub async fn follow(
    participant: LocalParticipant,
    identity: &str,
    mut incoming: UnboundedReceiver<Vec<u8>>,
    playback: SocketAddr,
) -> Result<()> {
    let (socket, candidates) = tokio::task::spawn_blocking(prepare)
        .await
        .context("address lookup")??;
    log_line(&format!("picture direct: this app is at {}", join_addrs(&candidates)));
    let card = Card {
        id: identity.to_string(),
        role: "watch".to_string(),
        addr: candidates.first().map(|addr| addr.to_string()).unwrap_or_default(),
        addrs: candidates.iter().map(|addr| addr.to_string()).collect(),
        cert: String::new(),
    };
    let peer = exchange(&participant, &card, &mut incoming, true).await?;
    let targets = peer.targets();
    let expected = unhex(&peer.cert)?;
    log_line(&format!("picture direct: other app is at {}", join_addrs(&targets)));
    let (socket, reached) = punch_owned(socket, targets.clone()).await?;
    if !reached.is_empty() {
        log_line(&format!("picture direct: reached {}", join_addrs(&reached)));
    }
    let mut endpoint = client_endpoint(socket, expected)?;
    let connection = connect_first(&mut endpoint, dial_order(&reached, &targets)).await?;
    // Open the stream before touching playback. A direct attempt that never
    // produces a stream must not sit on the player the room path needs.
    let (send, recv) = connection
        .accept_bi()
        .await
        .context("picture direct: stream")?;
    let local = tokio::net::TcpStream::connect(playback)
        .await
        .context("picture direct: playback pipe")?;
    log_line("picture direct: picture stream open");
    // The host repeats its card once a second while it is sharing. When those
    // cards stop, this picture is over. Closing here lets the watcher drop the
    // frozen frame and accept the next share without leaving the room.
    let mut last_host = tokio::time::Instant::now();
    let mut bridge_task = tokio::spawn(async move {
        bridge(local, send, recv).await;
    });
    loop {
        tokio::select! {
            finished = &mut bridge_task => {
                if let Err(err) = finished {
                    eprintln!("picture direct: picture task stopped ({err})");
                }
                break;
            }
            message = incoming.recv() => {
                let Some(message) = message else { break };
                if let Ok(card) = serde_json::from_slice::<Card>(&message) {
                    if card.role == "host" && card.id != identity {
                        last_host = tokio::time::Instant::now();
                    }
                }
            }
            _ = tokio::time::sleep_until(last_host + Duration::from_millis(2500)) => {
                connection.close(0u32.into(), b"share ended");
                break;
            }
        }
    }
    connection.close(0u32.into(), b"share ended");
    bridge_task.abort();
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
                if card.id == me.id || card.targets().is_empty() {
                    continue;
                }
                if !me.role.is_empty() && card.role == me.role {
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

async fn punch_owned(socket: UdpSocket, peers: Vec<SocketAddr>) -> Result<(UdpSocket, Vec<SocketAddr>)> {
    tokio::task::spawn_blocking(move || {
        let reached = punch(&socket, &peers);
        Ok((socket, reached))
    })
    .await
    .context("punch task")?
}

fn punch(socket: &UdpSocket, peers: &[SocketAddr]) -> Vec<SocketAddr> {
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    let mut reached = Vec::new();
    for _ in 0..8 {
        for peer in peers {
            socket.send_to(PUNCH, peer).ok();
        }
        let mut buf = [0u8; 64];
        if let Ok((size, from)) = socket.recv_from(&mut buf) {
            if &buf[..size] == PUNCH && !reached.contains(&from) {
                reached.push(from);
            }
        }
    }
    for peer in &reached {
        for _ in 0..4 {
            socket.send_to(PUNCH, peer).ok();
        }
    }
    socket.set_nonblocking(true).ok();
    let mut buf = [0u8; 1500];
    while socket.recv_from(&mut buf).is_ok() {}
    socket.set_nonblocking(false).ok();
    reached
}

fn dial_order(reached: &[SocketAddr], targets: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut order = reached.to_vec();
    order.extend(targets.iter().copied());
    order.sort_by_key(|addr| (rank(addr), !reached.contains(addr)));
    order.dedup();
    order
}

async fn connect_first(
    endpoint: &mut quinn::Endpoint,
    peers: Vec<SocketAddr>,
) -> Result<quinn::Connection> {
    if peers.is_empty() {
        bail!("the other app did not share an address");
    }
    let mut last = anyhow::anyhow!("picture direct: handshake failed");
    for peer in peers {
        println!("picture direct: trying {peer}");
        match connect_with_retry(endpoint, peer, Duration::from_secs(2)).await {
            Ok(connection) => {
                println!("picture direct: connected via {peer}");
                return Ok(connection);
            }
            Err(err) => {
                eprintln!("picture direct: {peer} did not connect ({err:#})");
                last = err;
            }
        }
    }
    Err(last)
}

async fn connect_with_retry(
    endpoint: &mut quinn::Endpoint,
    peer: SocketAddr,
    budget: Duration,
) -> Result<quinn::Connection> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let connecting = endpoint
            .connect(peer, "dcmax")
            .context("picture direct: connect")?;
        match tokio::time::timeout(Duration::from_secs(2), connecting).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(err)) if tokio::time::Instant::now() < deadline => {
                eprintln!("picture direct: handshake retry ({err})");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(Err(err)) => return Err(err).context("picture direct: handshake failed"),
            Err(_) => return Err(anyhow::anyhow!("picture direct: {peer} timed out")),
        }
    }
}

const DIRECT_WAIT: Duration = Duration::from_secs(4);
const ROOM_CHUNK: usize = 8_000;
const ROOM_START: u8 = 1;
const ROOM_MORE: u8 = 0;
const ROOM_END: u8 = 2;
const HOLD_CAP: usize = 8 * 1024 * 1024;
const FRAME_HEADER: usize = 28;
const FRAME_MAGIC: u32 = 0x4443_4D58;
const FRAME_FLAG_KEY: u8 = 0x01;
const FRAME_FLAG_AUDIO: u8 = 0x02;

enum Choice {
    Direct {
        connection: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        tcp: tokio::net::TcpStream,
        prelude: Vec<u8>,
    },
    Room {
        tcp: tokio::net::TcpStream,
        prelude: Vec<u8>,
    },
    Stop,
}

async fn serve(
    endpoint: quinn::Endpoint,
    listener: tokio::net::TcpListener,
    mut alive: tokio::sync::watch::Receiver<bool>,
    announce: tokio::sync::watch::Sender<bool>,
    participant: LocalParticipant,
) {
    loop {
        if !*alive.borrow() {
            break;
        }
        let accepted = tokio::select! {
            changed = alive.changed() => {
                if changed.is_err() || !*alive.borrow() {
                    break;
                }
                continue;
            }
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, _)) = accepted else {
            break;
        };
        match choose(&endpoint, tcp, &mut alive).await {
            Choice::Direct {
                connection,
                mut send,
                recv,
                tcp,
                prelude,
            } => {
                log_line("picture direct: carrying the picture");
                if !prelude.is_empty() && send.write_all(&prelude).await.is_err() {
                    log_line("picture direct: direct send failed, using the room");
                    drop(send);
                    drop(recv);
                    drop(connection);
                    carry_room(&participant, tcp, prelude).await;
                } else {
                    let _connection = connection;
                    bridge(tcp, send, recv).await;
                    log_line("picture direct: picture stopped");
                }
            }
            Choice::Room { tcp, prelude } => {
                let _ = announce.send(false);
                log_line("picture room: no direct path, sending through the room");
                let sending = carry_room(&participant, tcp, prelude);
                tokio::pin!(sending);
                loop {
                    tokio::select! {
                        _ = &mut sending => break,
                        incoming = endpoint.accept() => {
                            // The watcher may still be punching. Refuse it so
                            // that attempt does not take the local player.
                            drop(incoming);
                        }
                        changed = alive.changed() => {
                            if changed.is_err() || !*alive.borrow() {
                                return;
                            }
                        }
                    }
                }
            }
            Choice::Stop => return,
        }
    }
}

async fn choose(
    endpoint: &quinn::Endpoint,
    mut tcp: tokio::net::TcpStream,
    alive: &mut tokio::sync::watch::Receiver<bool>,
) -> Choice {
    tcp.set_nodelay(true).ok();
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel(1);
    let mut hold = Hold::default();
    let mut buf = vec![0u8; 64 * 1024];
    let deadline = tokio::time::sleep(DIRECT_WAIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            changed = alive.changed() => {
                if changed.is_err() || !*alive.borrow() {
                    return Choice::Stop;
                }
            }
            _ = &mut deadline => break,
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    return Choice::Stop;
                };
                let tx = ready_tx.clone();
                tokio::spawn(async move {
                    let Ok(connection) = tokio::time::timeout(Duration::from_secs(3), incoming).await else {
                        return;
                    };
                    let Ok(connection) = connection else {
                        return;
                    };
                    let _ = tx.try_send(connection);
                });
            }
            ready = ready_rx.recv() => {
                let Some(connection) = ready else {
                    continue;
                };
                match open_picture(connection).await {
                    Some(opened) => {
                        return Choice::Direct {
                            connection: opened.0,
                            send: opened.1,
                            recv: opened.2,
                            tcp,
                            prelude: hold.take_for_send(),
                        };
                    }
                    None => {}
                }
            }
            read = tcp.read(&mut buf) => match read {
                Ok(0) | Err(_) => return Choice::Stop,
                Ok(size) => hold.push(&buf[..size]),
            },
        }
    }
    if let Ok(connection) = ready_rx.try_recv() {
        if let Some(opened) = open_picture(connection).await {
            return Choice::Direct {
                connection: opened.0,
                send: opened.1,
                recv: opened.2,
                tcp,
                prelude: hold.take_for_send(),
            };
        }
    }
    Choice::Room {
        tcp,
        prelude: hold.take_for_send(),
    }
}

async fn open_picture(
    connection: quinn::Connection,
) -> Option<(quinn::Connection, quinn::SendStream, quinn::RecvStream)> {
    match tokio::time::timeout(Duration::from_secs(2), connection.open_bi()).await {
        Ok(Ok((send, recv))) => Some((connection, send, recv)),
        _ => None,
    }
}

async fn carry_room(participant: &LocalParticipant, mut tcp: tokio::net::TcpStream, pending: Vec<u8>) {
    let mut started = false;
    if !pending.is_empty() && !send_chunks(participant, &pending, &mut started).await {
        let _ = publish_room(participant, vec![ROOM_END]).await;
        return;
    }
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let size = match tcp.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(size) => size,
        };
        if !send_chunks(participant, &buf[..size], &mut started).await {
            let _ = publish_room(participant, vec![ROOM_END]).await;
            return;
        }
    }
    if started {
        let _ = publish_room(participant, vec![ROOM_END]).await;
        log_line("picture room: share ended");
    }
}

async fn send_chunks(participant: &LocalParticipant, bytes: &[u8], started: &mut bool) -> bool {
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + ROOM_CHUNK).min(bytes.len());
        let mut packet = Vec::with_capacity(1 + end - offset);
        packet.push(if *started { ROOM_MORE } else { ROOM_START });
        if !*started {
            log_line("picture room: sending picture bytes");
        }
        *started = true;
        packet.extend_from_slice(&bytes[offset..end]);
        offset = end;
        if !publish_room(participant, packet).await {
            return false;
        }
    }
    true
}

async fn publish_room(participant: &LocalParticipant, payload: Vec<u8>) -> bool {
    for _ in 0..3 {
        let sent = participant
            .publish_data(DataPacket {
                payload: payload.clone(),
                topic: Some(ROOM_TOPIC.to_string()),
                reliable: true,
                destination_identities: Vec::new(),
            })
            .await;
        if sent.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    log_line("picture room: the room did not take the picture");
    false
}

/// Play picture bytes that arrived through the room. Direct playback is a
/// different connection, used only when that path opens a stream.
pub async fn play_room(
    mut incoming: UnboundedReceiver<(String, Vec<u8>)>,
    playback: SocketAddr,
    stop: Arc<AtomicBool>,
) {
    let mut writer: Option<tokio::net::tcp::OwnedWriteHalf> = None;
    let mut active: Option<String> = None;
    let mut idle = tokio::time::Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let packet = tokio::select! {
            packet = incoming.recv() => packet,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if writer.is_some() && idle.elapsed() > Duration::from_secs(3) {
                    log_line("picture room: sender stopped");
                    writer = None;
                    active = None;
                }
                continue;
            }
        };
        let Some((from, packet)) = packet else {
            break;
        };
        if packet.is_empty() {
            continue;
        }
        if active.as_ref().is_some_and(|current| current != &from) && packet[0] != ROOM_START {
            continue;
        }
        idle = tokio::time::Instant::now();
        match packet[0] {
            ROOM_START => {
                active = Some(from);
                writer = None;
                match tokio::net::TcpStream::connect(playback).await {
                    Ok(stream) => {
                        stream.set_nodelay(true).ok();
                        log_line("picture room: connected");
                        let (_read, mut write) = stream.into_split();
                        if packet.len() > 1 && write.write_all(&packet[1..]).await.is_err() {
                            log_line("picture room: playback write stopped");
                            continue;
                        }
                        writer = Some(write);
                    }
                    Err(err) => log_line(&format!("picture room: playback did not open ({err})")),
                }
            }
            ROOM_MORE => {
                if let Some(write) = writer.as_mut() {
                    if write.write_all(&packet[1..]).await.is_err() {
                        log_line("picture room: playback write stopped");
                        writer = None;
                    }
                }
            }
            ROOM_END => {
                writer = None;
                log_line("picture room: share ended");
            }
            _ => {}
        }
    }
}

/// Bytes already read from the sender, kept aligned to picture frames.
/// A keyframe is the first byte a watcher can decode, so anything older than
/// the latest keyframe is dropped once the buffer grows.
#[derive(Default)]
struct Hold {
    buf: Vec<u8>,
}

impl Hold {
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        self.shrink();
    }

    fn shrink(&mut self) {
        let mut pos = 0usize;
        let mut key_at = None;
        let mut complete = 0usize;
        while pos + FRAME_HEADER <= self.buf.len() {
            let magic = u32::from_le_bytes(self.buf[pos..pos + 4].try_into().unwrap());
            if magic != FRAME_MAGIC {
                pos += 1;
                continue;
            }
            let flags = self.buf[pos + 5];
            let len = u32::from_le_bytes(self.buf[pos + 12..pos + 16].try_into().unwrap()) as usize;
            if len > 8 * 1024 * 1024 {
                pos += 1;
                continue;
            }
            if pos + FRAME_HEADER + len > self.buf.len() {
                break;
            }
            if flags & FRAME_FLAG_KEY != 0 && flags & FRAME_FLAG_AUDIO == 0 {
                key_at = Some(pos);
            }
            pos += FRAME_HEADER + len;
            complete = pos;
        }
        if self.buf.len() > HOLD_CAP {
            if let Some(key) = key_at {
                if key > 0 {
                    self.buf.drain(..key);
                }
            } else if complete > 0 && complete < self.buf.len() {
                self.buf.drain(..complete);
            } else if complete > 0 {
                self.buf.clear();
            }
        }
        self.drop_before_key();
    }

    fn drop_before_key(&mut self) {
        let Some(key) = self.first_key() else {
            return;
        };
        if key > 0 {
            self.buf.drain(..key);
        }
    }

    fn first_key(&self) -> Option<usize> {
        let mut pos = 0usize;
        while pos + FRAME_HEADER <= self.buf.len() {
            let magic = u32::from_le_bytes(self.buf[pos..pos + 4].try_into().unwrap());
            if magic != FRAME_MAGIC {
                return None;
            }
            let flags = self.buf[pos + 5];
            let len = u32::from_le_bytes(self.buf[pos + 12..pos + 16].try_into().unwrap()) as usize;
            if len > 8 * 1024 * 1024 {
                return None;
            }
            if flags & FRAME_FLAG_KEY != 0 && flags & FRAME_FLAG_AUDIO == 0 {
                return Some(pos);
            }
            if pos + FRAME_HEADER + len > self.buf.len() {
                return None;
            }
            pos += FRAME_HEADER + len;
        }
        None
    }

    fn take_for_send(&mut self) -> Vec<u8> {
        let mut pos = 0usize;
        let mut latest = None;
        while pos + FRAME_HEADER <= self.buf.len() {
            let magic = u32::from_le_bytes(self.buf[pos..pos + 4].try_into().unwrap());
            if magic != FRAME_MAGIC {
                break;
            }
            let flags = self.buf[pos + 5];
            let len = u32::from_le_bytes(self.buf[pos + 12..pos + 16].try_into().unwrap()) as usize;
            if len > 8 * 1024 * 1024 || pos + FRAME_HEADER + len > self.buf.len() {
                break;
            }
            if flags & FRAME_FLAG_KEY != 0 && flags & FRAME_FLAG_AUDIO == 0 {
                latest = Some(pos);
            }
            pos += FRAME_HEADER + len;
        }
        if let Some(key) = latest {
            if key > 0 {
                self.buf.drain(..key);
            }
        }
        std::mem::take(&mut self.buf)
    }
}

fn log_line(line: &str) {
    println!("{line}");
    crate::audio::session_log(line);
}

async fn bridge(local: tokio::net::TcpStream, mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    local.set_nodelay(true).ok();
    let (mut reader, mut writer) = local.into_split();
    // The two directions have to stay independent. The watcher often sends
    // nothing, and treating that quiet direction as a closed connection was
    // throwing away the picture.
    let to_remote = async move {
        let mut buf = vec![0u8; 64 * 1024];
        let mut sent = 0u64;
        loop {
            let size = match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(size) => size,
            };
            if send.write_all(&buf[..size]).await.is_err() {
                eprintln!("picture direct: send stopped after {sent} bytes");
                break;
            }
            if sent == 0 {
                println!("picture direct: sending picture bytes");
            }
            sent += size as u64;
        }
        let _ = send.finish();
    };
    let to_local = async move {
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(size)) if size > 0 => {
                    if writer.write_all(&buf[..size]).await.is_err() {
                        eprintln!("picture direct: playback write stopped after {received} bytes");
                        break;
                    }
                    if received == 0 {
                        println!("picture direct: picture bytes arrived");
                    }
                    received += size as u64;
                }
                Ok(_) => break,
                Err(err) => {
                    eprintln!("picture direct: receive stopped after {received} bytes ({err})");
                    break;
                }
            }
        }
    };
    tokio::join!(to_remote, to_local);
}

fn prepare() -> Result<(UdpSocket, Vec<SocketAddr>)> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("udp bind")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .ok();
    let port = socket.local_addr().context("local port")?.port();
    let mut candidates = local_candidates(port);
    match stun_mapped(&socket) {
        Ok(mapped) if mapped.is_ipv4() && !candidates.contains(&mapped) => candidates.push(mapped),
        Ok(_) => {}
        Err(err) => eprintln!("picture direct: public address unavailable ({err:#})"),
    }
    candidates.sort_by_key(rank);
    candidates.dedup();
    if candidates.is_empty() {
        bail!("this app has no IPv4 address to share");
    }
    Ok((socket, candidates))
}

fn local_candidates(port: u16) -> Vec<SocketAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| {
            if virtual_adapter(&iface.name) {
                return None;
            }
            match iface.addr {
                if_addrs::IfAddr::V4(addr)
                    if !addr.ip.is_loopback() && !addr.ip.is_link_local() =>
                {
                    Some(SocketAddr::new(IpAddr::V4(addr.ip), port))
                }
                _ => None,
            }
        })
        .collect()
}

fn virtual_adapter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "virtualbox",
        "vmware",
        "hyper-v",
        "vethernet",
        "docker",
        "bluetooth",
        "teredo",
        "wsl",
        "awdl",
        "llw",
        "bridge",
        "gif",
        "stf",
        "pktap",
        "ipsec",
        "anpi",
    ]
    .iter()
    .any(|label| name.contains(label))
}

fn rank(addr: &SocketAddr) -> u8 {
    let IpAddr::V4(ip) = addr.ip() else {
        return 3;
    };
    let [first, second, third, _] = ip.octets();
    // VirtualBox's host-only network, and the iPhone USB/hotspot range. Both
    // are private addresses on this computer and are not the other machine's path.
    if (first == 192 && second == 168 && third == 56) || (first == 172 && second == 20 && third == 10) {
        return 5;
    }
    if ip.is_private() {
        return 0;
    }
    if first == 100 && (second & 0xc0) == 64 {
        return 1;
    }
    2
}

fn join_addrs(addrs: &[SocketAddr]) -> String {
    addrs
        .iter()
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn stun_mapped(socket: &UdpSocket) -> Result<SocketAddr> {
    let mut last = anyhow::anyhow!("no STUN server answered");
    for server in ["stun.l.google.com:19302", "stun.cloudflare.com:3478"] {
        match stun_query(socket, server) {
            Ok(mapped) if mapped.is_ipv4() => return Ok(mapped),
            Ok(_) => {}
            Err(err) => last = err,
        }
    }
    Err(last)
}

fn stun_query(socket: &UdpSocket, server: &str) -> Result<SocketAddr> {
    use std::net::ToSocketAddrs;
    let addresses: Vec<SocketAddr> = server
        .to_socket_addrs()
        .with_context(|| format!("resolve {server}"))?
        .filter(|addr| addr.is_ipv4())
        .collect();
    if addresses.is_empty() {
        bail!("{server} has no IPv4 address");
    }
    let mut last = anyhow::anyhow!("{server} did not answer on IPv4");
    for address in addresses {
        match stun_exchange(socket, address) {
            Ok(mapped) if mapped.is_ipv4() => return Ok(mapped),
            Ok(mapped) => last = anyhow::anyhow!("stun {address} returned {mapped}"),
            Err(err) => last = err,
        }
    }
    Err(last)
}

fn stun_exchange(socket: &UdpSocket, server: SocketAddr) -> Result<SocketAddr> {
    let txid = transaction_id();
    socket
        .send_to(&binding_request(&txid), server)
        .with_context(|| format!("stun {server}"))?;
    let mut buf = [0u8; 1500];
    let (size, _) = socket
        .recv_from(&mut buf)
        .with_context(|| format!("stun {server} timed out"))?;
    let mapped = parse_xor_mapped(&buf[..size])?;
    if buf[8..20] != txid {
        bail!("stun response was for a different request");
    }
    Ok(mapped)
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
    // Tailscale's tunnel is 1280 bytes. Letting QUIC grow past that drops the
    // picture while the handshake, which stays small, still succeeds.
    transport.initial_mtu(1200);
    transport.mtu_discovery_config(None);
    // One megabyte is about a tenth of a second at 80 Mbps. The old 8 MB
    // window held the pictures the viewer had not reached yet, so the share
    // played that backlog instead of the live screen.
    let window = quinn::VarInt::from_u32(1024 * 1024);
    transport.receive_window(window);
    transport.stream_receive_window(window);
    transport.send_window(1024 * 1024);
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
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            send.write_all(b"frame").await.unwrap();
            let mut buf = [0u8; 5];
            recv.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"audio");
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        let mut endpoint = client_endpoint(client_socket, expected).unwrap();
        let connection = connect_with_retry(&mut endpoint, server_addr, Duration::from_secs(3))
            .await
            .unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        let mut buf = [0u8; 5];
        recv.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"frame");
        send.write_all(b"audio").await.unwrap();
        server.await.unwrap();
    }

    fn picture_frame(key: bool, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        bytes.push(2);
        bytes.push(if key { FRAME_FLAG_KEY } else { 0 });
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&1i64.to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn held_picture_starts_at_a_keyframe() {
        let mut hold = Hold::default();
        hold.push(&picture_frame(false, b"old"));
        let key = picture_frame(true, b"key");
        assert_eq!(key.len(), FRAME_HEADER + 3);
        hold.push(&key[..10]);
        hold.push(&key[10..]);
        hold.push(&picture_frame(false, b"next"));
        let newer = picture_frame(true, b"new");
        hold.push(&newer);
        let bytes = hold.take_for_send();
        assert!(bytes.starts_with(&newer));
        assert!(!bytes.windows(3).any(|win| win == b"old"));
        assert!(!bytes.windows(4).any(|win| win == b"next"));
    }
}
