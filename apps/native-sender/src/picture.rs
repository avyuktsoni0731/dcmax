//! Direct picture path. Each access unit is split into equal shards, a few
//! parity shards are added, and every shard is one UDP datagram. The receiver
//! rebuilds the access unit from any complete set, so one lost datagram does
//! not wait for the next keyframe.
//!
//! LiveKit is not involved. The publisher still sends the same frames there
//! for the browser.

use crate::h264::CursorSample;
use anyhow::{Context, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAGIC: u32 = 0x4443_4D58;
const VERSION: u8 = 1;
const HEADER: usize = 32;
const SHARD: usize = 1100;
const HOLD_FRAMES: u32 = 8;
const MAX_DATA: usize = 200;
const FLAG_KEY: u8 = 0x01;

#[allow(dead_code)]
pub struct PictureFrame {
    pub payload: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: i64,
    pub cursor: CursorSample,
    pub frame_id: u32,
}

struct Outgoing {
    payload: Vec<u8>,
    keyframe: bool,
    timestamp_us: i64,
    cursor: CursorSample,
}

struct Shared {
    sent: AtomicU64,
    skipped: AtomicU64,
    want_idr: AtomicBool,
}

pub struct PictureSender {
    tx: SyncSender<Outgoing>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl PictureSender {
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        let (tx, rx) = sync_channel(2);
        let shared = Arc::new(Shared {
            sent: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            want_idr: AtomicBool::new(false),
        });
        let counters = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("picture-udp".to_string())
            .spawn(move || send_loop(addr, rx, counters))
            .context("failed to start the picture sender")?;
        Ok(Self {
            tx,
            shared,
            join: Some(join),
        })
    }

    pub fn offer(
        &self,
        payload: &[u8],
        keyframe: bool,
        timestamp_us: i64,
        cursor: Option<CursorSample>,
    ) {
        let frame = Outgoing {
            payload: payload.to_vec(),
            keyframe,
            timestamp_us,
            cursor: cursor.unwrap_or(CursorSample {
                visible: false,
                x: 0,
                y: 0,
            }),
        };
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.tx.try_send(frame)
        {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn sent(&self) -> u64 {
        self.shared.sent.load(Ordering::Relaxed)
    }

    pub fn skipped(&self) -> u64 {
        self.shared.skipped.load(Ordering::Relaxed)
    }

    pub fn take_keyframe(&self) -> bool {
        self.shared.want_idr.swap(false, Ordering::AcqRel)
    }
}

impl Drop for PictureSender {
    fn drop(&mut self) {
        let (tx, _) = sync_channel(1);
        let _ = std::mem::replace(&mut self.tx, tx);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn send_loop(addr: SocketAddr, rx: Receiver<Outgoing>, shared: Arc<Shared>) {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => socket,
        Err(err) => {
            eprintln!("picture udp: bind failed: {err}");
            return;
        }
    };
    let reference = socket2::SockRef::from(&socket);
    let _ = reference.set_send_buffer_size(4 * 1024 * 1024);
    println!("picture udp: sending to {addr}");
    let mut codec: Option<(usize, usize, ReedSolomon)> = None;
    let mut frame_id = 0u32;
    let mut announced = false;
    while let Ok(frame) = rx.recv() {
        match packets_for(
            &mut codec,
            frame_id,
            &frame.payload,
            frame.keyframe,
            frame.timestamp_us,
            frame.cursor,
        ) {
            Some(packets) => {
                let mut failed = false;
                for packet in &packets {
                    if socket.send_to(packet, addr).is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    shared.skipped.fetch_add(1, Ordering::Relaxed);
                } else {
                    shared.sent.fetch_add(1, Ordering::Relaxed);
                    if !announced {
                        announced = true;
                        println!(
                            "picture udp: first frame {} bytes in {} datagrams",
                            frame.payload.len(),
                            packets.len()
                        );
                    }
                }
                frame_id = frame_id.wrapping_add(1);
                poll_keyframe_request(&socket, &shared);
            }
            None => {
                shared.skipped.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "picture udp: frame of {} bytes is too large to send",
                    frame.payload.len()
                );
            }
        }
    }
}

struct PartialFrame {
    data_shards: usize,
    parity_shards: usize,
    payload_len: usize,
    keyframe: bool,
    timestamp_us: i64,
    cursor: CursorSample,
    shards: Vec<Option<Vec<u8>>>,
    have: usize,
}

pub struct Assembler {
    frames: BTreeMap<u32, PartialFrame>,
    newest: Option<u32>,
    expected: Option<u32>,
    lost: u64,
    codec: Option<(usize, usize, ReedSolomon)>,
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            frames: BTreeMap::new(),
            newest: None,
            expected: None,
            lost: 0,
            codec: None,
        }
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }

    pub fn push(&mut self, packet: &[u8]) -> Vec<PictureFrame> {
        let Some(header) = parse_header(packet) else {
            return Vec::new();
        };
        if packet.len() != HEADER + header.shard_size
            || header.shard_size == 0
            || header.shard_size > 1500
            || header.data_shards == 0
            || header.data_shards + header.parity_shards > 255
            || header.payload_len == 0
            || header.payload_len > header.data_shards * header.shard_size
            || header.shard_index >= header.data_shards + header.parity_shards
            || self
                .newest
                .is_some_and(|newest| header.frame_id.saturating_add(HOLD_FRAMES) < newest)
        {
            return Vec::new();
        }
        self.newest = Some(self.newest.map_or(header.frame_id, |id| id.max(header.frame_id)));
        let entry = self.frames.entry(header.frame_id).or_insert_with(|| PartialFrame {
            data_shards: header.data_shards,
            parity_shards: header.parity_shards,
            payload_len: header.payload_len,
            keyframe: header.keyframe,
            timestamp_us: header.timestamp_us,
            cursor: header.cursor,
            shards: vec![None; header.data_shards + header.parity_shards],
            have: 0,
        });
        if entry.shards.len() != header.data_shards + header.parity_shards {
            return Vec::new();
        }
        if entry.shards[header.shard_index].is_none() {
            entry.shards[header.shard_index] = Some(packet[HEADER..].to_vec());
            entry.have += 1;
        }
        let mut ready = Vec::new();
        while let Some(frame) = self.finish_ready() {
            ready.push(frame);
        }
        ready
    }

    fn finish_ready(&mut self) -> Option<PictureFrame> {
        let newest = self.newest?;
        loop {
            let Some(oldest) = self.frames.keys().next().copied() else {
                return None;
            };
            let ready = self.frames.get(&oldest).is_some_and(|frame| frame.have >= frame.data_shards);
            if ready {
                let frame = self.frames.remove(&oldest)?;
                if let Some(picture) = self.recover(oldest, frame) {
                    self.note_gap_before(oldest);
                    self.expected = Some(oldest.wrapping_add(1));
                    return Some(picture);
                }
                self.note_lost(oldest);
                continue;
            }
            if newest > oldest.saturating_add(HOLD_FRAMES) {
                self.frames.remove(&oldest);
                self.note_lost(oldest);
                continue;
            }
            return None;
        }
    }

    fn note_gap_before(&mut self, frame_id: u32) {
        if let Some(expected) = self.expected {
            if frame_id > expected {
                self.lost += u64::from(frame_id - expected);
            }
        }
    }

    fn note_lost(&mut self, frame_id: u32) {
        match self.expected {
            Some(expected) if frame_id < expected => {}
            Some(expected) => {
                self.lost += u64::from(frame_id - expected) + 1;
                self.expected = Some(frame_id.wrapping_add(1));
            }
            None => {
                self.lost += 1;
                self.expected = Some(frame_id.wrapping_add(1));
            }
        }
    }

    fn recover(&mut self, frame_id: u32, frame: PartialFrame) -> Option<PictureFrame> {
        let mut shards = frame.shards;
        let missing_data = shards[..frame.data_shards].iter().any(|shard| shard.is_none());
        if missing_data {
            if frame.parity_shards == 0 || frame.have < frame.data_shards {
                return None;
            }
            let codec = cached_codec(&mut self.codec, frame.data_shards, frame.parity_shards)?;
            codec.reconstruct_data(&mut shards).ok()?;
        }
        let mut payload = Vec::with_capacity(frame.payload_len);
        for shard in shards.into_iter().take(frame.data_shards) {
            payload.extend_from_slice(shard?.as_slice());
            if payload.len() >= frame.payload_len {
                break;
            }
        }
        if payload.len() < frame.payload_len {
            return None;
        }
        payload.truncate(frame.payload_len);
        Some(PictureFrame {
            payload,
            keyframe: frame.keyframe,
            timestamp_us: frame.timestamp_us,
            cursor: frame.cursor,
            frame_id,
        })
    }
}

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
}

struct Header {
    frame_id: u32,
    shard_index: usize,
    data_shards: usize,
    parity_shards: usize,
    shard_size: usize,
    payload_len: usize,
    keyframe: bool,
    timestamp_us: i64,
    cursor: CursorSample,
}

fn parse_header(packet: &[u8]) -> Option<Header> {
    if packet.len() < HEADER {
        return None;
    }
    let magic = u32::from_le_bytes(packet[0..4].try_into().ok()?);
    if magic != MAGIC || packet[4] != VERSION {
        return None;
    }
    Some(Header {
        keyframe: packet[5] & FLAG_KEY != 0,
        data_shards: packet[6] as usize,
        parity_shards: packet[7] as usize,
        frame_id: u32::from_le_bytes(packet[8..12].try_into().ok()?),
        shard_index: packet[12] as usize,
        cursor: CursorSample {
            visible: packet[13] != 0,
            x: u16::from_le_bytes(packet[14..16].try_into().ok()?),
            y: u16::from_le_bytes(packet[16..18].try_into().ok()?),
        },
        shard_size: u16::from_le_bytes(packet[18..20].try_into().ok()?) as usize,
        payload_len: u32::from_le_bytes(packet[20..24].try_into().ok()?) as usize,
        timestamp_us: i64::from_le_bytes(packet[24..32].try_into().ok()?),
    })
}

fn write_header(
    packet: &mut [u8],
    frame_id: u32,
    shard_index: usize,
    data_shards: usize,
    parity_shards: usize,
    shard_size: usize,
    payload_len: usize,
    keyframe: bool,
    timestamp_us: i64,
    cursor: CursorSample,
) {
    packet[..4].copy_from_slice(&MAGIC.to_le_bytes());
    packet[4] = VERSION;
    packet[5] = if keyframe { FLAG_KEY } else { 0 };
    packet[6] = data_shards as u8;
    packet[7] = parity_shards as u8;
    packet[8..12].copy_from_slice(&frame_id.to_le_bytes());
    packet[12] = shard_index as u8;
    packet[13] = u8::from(cursor.visible);
    packet[14..16].copy_from_slice(&cursor.x.to_le_bytes());
    packet[16..18].copy_from_slice(&cursor.y.to_le_bytes());
    packet[18..20].copy_from_slice(&(shard_size as u16).to_le_bytes());
    packet[20..24].copy_from_slice(&(payload_len as u32).to_le_bytes());
    packet[24..32].copy_from_slice(&timestamp_us.to_le_bytes());
}

fn plan(len: usize) -> Option<(usize, usize)> {
    if len == 0 || len > MAX_DATA * SHARD {
        return None;
    }
    let data = len.div_ceil(SHARD).clamp(1, MAX_DATA);
    let mut parity = (data / 3).max(4);
    if data + parity > 255 {
        parity = 255 - data;
    }
    if parity == 0 {
        return None;
    }
    Some((data, parity))
}

fn cached_codec(
    cache: &mut Option<(usize, usize, ReedSolomon)>,
    data: usize,
    parity: usize,
) -> Option<&mut ReedSolomon> {
    let fresh = cache
        .as_ref()
        .is_none_or(|(have_data, have_parity, _)| *have_data != data || *have_parity != parity);
    if fresh {
        *cache = Some((data, parity, ReedSolomon::new(data, parity).ok()?));
    }
    cache.as_mut().map(|(_, _, codec)| codec)
}

fn packets_for(
    cache: &mut Option<(usize, usize, ReedSolomon)>,
    frame_id: u32,
    payload: &[u8],
    keyframe: bool,
    timestamp_us: i64,
    cursor: CursorSample,
) -> Option<Vec<Vec<u8>>> {
    let (data, parity) = plan(payload.len())?;
    let mut shards = vec![vec![0u8; SHARD]; data + parity];
    for (index, chunk) in payload.chunks(SHARD).enumerate() {
        shards[index][..chunk.len()].copy_from_slice(chunk);
    }
    cached_codec(cache, data, parity)?.encode(&mut shards).ok()?;
    let packets = shards
        .into_iter()
        .enumerate()
        .map(|(index, shard)| {
            let mut packet = vec![0u8; HEADER + shard.len()];
            write_header(
                &mut packet[..HEADER],
                frame_id,
                index,
                data,
                parity,
                SHARD,
                payload.len(),
                keyframe,
                timestamp_us,
                cursor,
            );
            packet[HEADER..].copy_from_slice(&shard);
            packet
        })
        .collect();
    Some(packets)
}

pub fn listen(addr: SocketAddr) -> Result<(UdpSocket, Assembler)> {
    let socket = UdpSocket::bind(addr).with_context(|| format!("picture udp listen {addr} failed"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .context("picture udp timeout")?;
    let reference = socket2::SockRef::from(&socket);
    if let Err(err) = reference.set_recv_buffer_size(4 * 1024 * 1024) {
        eprintln!("picture udp: could not enlarge the receive buffer ({err})");
    } else {
        let size = reference.recv_buffer_size().unwrap_or(0);
        println!("picture udp: receive buffer {size} bytes");
    }
    println!("picture udp: listening on {addr}");
    Ok((socket, Assembler::new()))
}

pub fn recv_packet(
    socket: &UdpSocket,
    buffer: &mut [u8],
) -> Result<Option<(usize, SocketAddr)>, std::io::Error> {
    match socket.recv_from(buffer) {
        Ok((size, addr)) => Ok(Some((size, addr))),
        Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut => Ok(None),
        Err(err) => Err(err),
    }
}

fn poll_keyframe_request(socket: &UdpSocket, shared: &Shared) {
    let _ = socket.set_nonblocking(true);
    let mut buf = [0u8; 8];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((size, _)) if size >= 3 && &buf[..3] == b"IDR" => {
                shared.want_idr.store(true, Ordering::Release);
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let _ = socket.set_nonblocking(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index % 251) as u8).collect()
    }

    fn assemble(packets: &[Vec<u8>], drop_at: &[usize]) -> PictureFrame {
        let mut assembler = Assembler::new();
        let mut picture = None;
        for (index, packet) in packets.iter().enumerate() {
            if drop_at.contains(&index) {
                continue;
            }
            if let Some(frame) = assembler.push(packet).pop() {
                picture = Some(frame);
            }
        }
        picture.expect("frame should rebuild")
    }

    #[test]
    fn parity_rebuilds_a_frame_after_two_lost_datagrams() {
        let payload = sample(20_000);
        let cursor = CursorSample {
            visible: true,
            x: 1000,
            y: 40000,
        };
        let mut cache = None;
        let packets = packets_for(&mut cache, 7, &payload, true, 1234, cursor).unwrap();
        assert!(packets.len() > 4);
        let picture = assemble(&packets, &[0, 3]);
        assert_eq!(picture.payload, payload);
        assert!(picture.keyframe);
        assert_eq!(picture.timestamp_us, 1234);
        assert_eq!(picture.cursor, cursor);
        assert_eq!(picture.frame_id, 7);
    }

    #[test]
    fn too_many_losses_do_not_invent_a_frame() {
        let payload = sample(8_000);
        let mut cache = None;
        let packets = packets_for(
            &mut cache,
            1,
            &payload,
            false,
            9,
            CursorSample {
                visible: false,
                x: 0,
                y: 0,
            },
        )
        .unwrap();
        let parity = packets[0][7] as usize;
        let mut assembler = Assembler::new();
        let mut picture = None;
        for packet in packets.iter().skip(packets.len() - parity) {
            picture = assembler.push(packet).pop().or(picture);
        }
        assert!(picture.is_none());
    }
}
