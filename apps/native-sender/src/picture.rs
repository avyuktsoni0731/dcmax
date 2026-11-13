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
use std::collections::{BTreeMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAGIC: u32 = 0x4443_4D58;
const VERSION: u8 = 1;
const HEADER: usize = 32;
const SHARD: usize = 1100;
const HOLD_FRAMES: u32 = 16;
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
        let (tx, rx) = sync_channel(4);
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

struct Wave {
    frame_id: u32,
    packets: Vec<Vec<u8>>,
    payload_len: usize,
    next: usize,
}

struct Saved {
    frame_id: u32,
    packets: Vec<Vec<u8>>,
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
    let mut wave: VecDeque<Wave> = VecDeque::new();
    let mut saved: VecDeque<Saved> = VecDeque::new();
    let mut next_at = Instant::now();
    let mut sent_packets: u32 = 0;
    let gap = Duration::from_micros(250);
    loop {
        if wave.is_empty() {
            let Ok(frame) = rx.recv() else { break };
            push_wave(&mut wave, &mut codec, frame_id, frame, &shared);
            frame_id = frame_id.wrapping_add(1);
        }
        while wave.len() < 3 {
            match rx.try_recv() {
                Ok(frame) => {
                    push_wave(&mut wave, &mut codec, frame_id, frame, &shared);
                    frame_id = frame_id.wrapping_add(1);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        let Some(mut item) = wave.pop_front() else {
            continue;
        };
        let now = Instant::now();
        if next_at > now {
            while Instant::now() < next_at {
                std::hint::spin_loop();
            }
        }
        next_at = Instant::now() + gap;
        if socket.send_to(&item.packets[item.next], addr).is_err() {
            shared.skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        item.next += 1;
        sent_packets = sent_packets.wrapping_add(1);
        if sent_packets.is_multiple_of(8) {
            poll_feedback(&socket, addr, &shared, &saved);
        }
        if item.next == item.packets.len() {
            saved.push_back(Saved {
                frame_id: item.frame_id,
                packets: item.packets.clone(),
            });
            while saved.len() > 16 {
                saved.pop_front();
            }
            shared.sent.fetch_add(1, Ordering::Relaxed);
            if !announced {
                announced = true;
                println!(
                    "picture udp: first frame {} bytes in {} datagrams",
                    item.payload_len,
                    item.packets.len()
                );
            }
            poll_feedback(&socket, addr, &shared, &saved);
        } else {
            wave.push_back(item);
        }
    }
}

fn push_wave(
    wave: &mut VecDeque<Wave>,
    codec: &mut Option<(usize, usize, ReedSolomon)>,
    frame_id: u32,
    frame: Outgoing,
    shared: &Shared,
) {
    match packets_for(
        codec,
        frame_id,
        &frame.payload,
        frame.keyframe,
        frame.timestamp_us,
        frame.cursor,
    ) {
        Some(packets) => {
            wave.push_back(Wave {
                frame_id,
                payload_len: frame.payload.len(),
                next: 0,
                packets,
            });
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
            || self.expected.is_some_and(|expected| header.frame_id < expected)
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
                if self.expected.is_some_and(|expected| oldest < expected) {
                    continue;
                }
                if let Some(picture) = self.recover(oldest, frame) {
                    self.note_gap_before(oldest);
                    self.expected = Some(oldest.wrapping_add(1));
                    return Some(picture);
                }
                self.note_lost(oldest);
                continue;
            }
            if let Some(key_id) = self.ready_keyframe_after(oldest) {
                if key_id > oldest.saturating_add(4) {
                    while self
                        .frames
                        .keys()
                        .next()
                        .copied()
                        .is_some_and(|id| id < key_id)
                    {
                        let id = self.frames.keys().next().copied().unwrap();
                        self.frames.remove(&id);
                        self.note_lost(id);
                    }
                    continue;
                }
            }
            if newest > oldest.saturating_add(HOLD_FRAMES) {
                self.frames.remove(&oldest);
                self.note_lost(oldest);
                continue;
            }
            return None;
        }
    }

    fn ready_keyframe_after(&self, oldest: u32) -> Option<u32> {
        self.frames.range(oldest.wrapping_add(1)..).find_map(|(&id, frame)| {
            (frame.keyframe && frame.have >= frame.data_shards).then_some(id)
        })
    }

    pub fn missing(&self) -> Option<Missing> {
        let newest = self.newest?;
        let (&id, frame) = self.frames.iter().find(|(_, frame)| frame.have < frame.data_shards)?;
        // The sender still has this frame on the wire for a few frame times.
        // Asking before that just makes it send the whole frame twice.
        if newest < id.saturating_add(5) {
            return None;
        }
        let total = frame.data_shards + frame.parity_shards;
        let mut bitmap = vec![0u8; total.div_ceil(8)];
        for (index, shard) in frame.shards.iter().enumerate() {
            if shard.is_none() {
                bitmap[index / 8] |= 1 << (index % 8);
            }
        }
        Some(Missing {
            frame_id: id,
            bitmap,
        })
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
    // Half the datagrams can vanish and the frame still rebuilds. The copies
    // are mixed with the picture data so a drop at the end of a burst does not
    // take every recovery shard with it.
    let mut parity = (data / 2).max(4);
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
    Some(mix_recovery(packets))
}

fn mix_recovery(packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let half = packets.len().div_ceil(2);
    let mut head = packets;
    let mut tail = head.split_off(half);
    let mut mixed = Vec::with_capacity(head.len() + tail.len());
    for packet in head {
        mixed.push(packet);
        if let Some(packet) = tail.pop() {
            mixed.push(packet);
        }
    }
    mixed.extend(tail);
    mixed
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

pub struct Missing {
    pub frame_id: u32,
    pub bitmap: Vec<u8>,
}

impl Missing {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut packet = Vec::with_capacity(10 + self.bitmap.len());
        packet.extend_from_slice(b"NAK");
        packet.push(1);
        packet.extend_from_slice(&self.frame_id.to_le_bytes());
        packet.extend_from_slice(&self.bitmap);
        packet
    }
}

fn poll_feedback(socket: &UdpSocket, addr: SocketAddr, shared: &Shared, saved: &VecDeque<Saved>) {
    let _ = socket.set_nonblocking(true);
    let mut buf = [0u8; 64];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((size, _)) if size >= 3 && &buf[..3] == b"IDR" => {
                shared.want_idr.store(true, Ordering::Release);
            }
            Ok((size, _)) if size >= 8 && &buf[..3] == b"NAK" => {
                resend(socket, addr, saved, &buf[..size]);
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let _ = socket.set_nonblocking(false);
}

fn resend(socket: &UdpSocket, addr: SocketAddr, saved: &VecDeque<Saved>, packet: &[u8]) {
    let Ok(frame_id) = packet[4..8].try_into().map(u32::from_le_bytes) else {
        return;
    };
    let Some(frame) = saved.iter().find(|frame| frame.frame_id == frame_id) else {
        return;
    };
    let bitmap = &packet[8..];
    for shard in &frame.packets {
        let Some(&index) = shard.get(12) else {
            continue;
        };
        let index = index as usize;
        let missing = bitmap
            .get(index / 8)
            .is_some_and(|bits| bits & (1 << (index % 8)) != 0);
        if missing {
            let _ = socket.send_to(shard, addr);
        }
    }
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

    #[test]
    fn a_short_frame_asks_for_the_shards_it_still_needs() {
        let payload = sample(12_000);
        let cursor = CursorSample {
            visible: false,
            x: 0,
            y: 0,
        };
        let mut cache = None;
        let first = packets_for(&mut cache, 4, &payload, false, 10, cursor).unwrap();
        let second = packets_for(&mut cache, 9, &payload, false, 20, cursor).unwrap();
        let mut assembler = Assembler::new();
        for packet in first.iter().take(2) {
            assembler.push(packet);
        }
        assembler.push(&second[0]);
        assembler.push(&second[1]);
        let gap = assembler.missing().expect("the short frame should be reported");
        assert_eq!(gap.frame_id, 4);
        assert!(gap.bitmap.iter().any(|bits| *bits != 0));
        let nak = gap.to_bytes();
        assert_eq!(&nak[..3], b"NAK");
        assert_eq!(u32::from_le_bytes(nak[4..8].try_into().unwrap()), 4);
    }
}
