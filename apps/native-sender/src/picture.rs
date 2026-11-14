//! Direct picture path. Each access unit is one length-prefixed record on a
//! TCP connection. The kernel retransmits a lost packet, so the decoder never
//! sees a hole and a following frame stays decodable.
//!
//! LiveKit is not involved. The publisher still sends the same frames there
//! for the browser.

use crate::h264::CursorSample;
use anyhow::{Context, Result};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAGIC: u32 = 0x4443_4D58;
const HELLO_MAGIC: u32 = 0x4443_4D52;
const VERSION: u8 = 2;
const HELLO_VERSION: u8 = 1;
const HEADER: usize = 28;
const FLAG_KEY: u8 = 0x01;
const FLAG_AUDIO: u8 = 0x02;
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
pub const ROLE_PUBLISH: u8 = 1;
pub const ROLE_WATCH: u8 = 2;

#[allow(dead_code)]
pub struct PictureFrame {
    pub payload: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: i64,
    pub cursor: CursorSample,
    pub frame_id: u32,
    pub audio: bool,
}

enum Outgoing {
    Video {
        payload: Vec<u8>,
        keyframe: bool,
        timestamp_us: i64,
        cursor: CursorSample,
    },
    Audio {
        payload: Vec<u8>,
        timestamp_us: i64,
    },
}

pub struct Hello {
    pub role: u8,
    pub room: String,
    pub invite: String,
}

struct Dial {
    addr: SocketAddr,
    relay: Option<(String, String)>,
}

struct Shared {
    sent: AtomicU64,
    skipped: AtomicU64,
    want_idr: AtomicBool,
    /// A frame was not sent. Deltas after it would be predicted from a picture
    /// the watcher never got, so they wait for the next keyframe.
    need_key: AtomicBool,
}

pub struct PictureSender {
    tx: SyncSender<Outgoing>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl PictureSender {
    pub fn connect(addr: SocketAddr) -> Result<Self> {
        Self::spawn(Dial { addr, relay: None })
    }

    /// Dial the relay. The bytes after the hello are the same picture stream
    /// a direct watcher reads.
    pub fn connect_relay(addr: SocketAddr, room: &str, invite: &str) -> Result<Self> {
        Self::spawn(Dial {
            addr,
            relay: Some((room.to_string(), invite.to_string())),
        })
    }

    fn spawn(dial: Dial) -> Result<Self> {
        let (tx, rx) = sync_channel(8);
        let shared = Arc::new(Shared {
            sent: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            want_idr: AtomicBool::new(false),
            need_key: AtomicBool::new(false),
        });
        let counters = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("picture-tcp".to_string())
            .spawn(move || send_loop(dial, rx, counters))
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
        let frame = Outgoing::Video {
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
            self.shared.need_key.store(true, Ordering::Release);
            self.shared.want_idr.store(true, Ordering::Release);
        }
    }

    pub fn offer_audio(&self, payload: &[u8], timestamp_us: i64) {
        let frame = Outgoing::Audio {
            payload: payload.to_vec(),
            timestamp_us,
        };
        let _ = self.tx.try_send(frame);
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

fn send_loop(dial: Dial, rx: Receiver<Outgoing>, shared: Arc<Shared>) {
    let mut stream = Some(connect_picture(&dial, Arc::clone(&shared)));
    let mut frame_id = 0u32;
    let mut audio_id = 0u32;
    let mut announced = false;
    let mut retry = None::<Outgoing>;
    loop {
        let frame = if let Some(frame) = retry.take() {
            frame
        } else {
            match rx.recv() {
                Ok(frame) => frame,
                Err(_) => return,
            }
        };
        let video_key = matches!(frame, Outgoing::Video { keyframe: true, .. });
        let waiting_for_key = shared.need_key.load(Ordering::Acquire)
            && matches!(frame, Outgoing::Video { keyframe: false, .. });
        if waiting_for_key {
            continue;
        }
        if stream.is_none() {
            stream = Some(connect_picture(&dial, Arc::clone(&shared)));
            frame_id = 0;
            audio_id = 0;
            announced = false;
            // Frames queued before the watcher was reading belong to a gap.
            // Sending them would show one old picture and then nothing the
            // watcher can decode until the next keyframe.
            if shared.need_key.load(Ordering::Acquire) {
                while rx.try_recv().is_ok() {}
                if !video_key {
                    continue;
                }
            }
        }
        let socket = stream.as_mut().unwrap();
        let id = if matches!(frame, Outgoing::Audio { .. }) {
            audio_id
        } else {
            frame_id
        };
        if write_frame(socket, id, &frame).is_err() {
            shared.need_key.store(true, Ordering::Release);
            shared.want_idr.store(true, Ordering::Release);
            let _ = socket.shutdown(Shutdown::Both);
            stream = None;
            if video_key {
                retry = Some(frame);
            }
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        match &frame {
            Outgoing::Video { keyframe, payload, .. } => {
                if *keyframe {
                    shared.need_key.store(false, Ordering::Release);
                }
                shared.sent.fetch_add(1, Ordering::Relaxed);
                if !announced {
                    announced = true;
                    println!("picture tcp: first frame {} bytes", payload.len());
                }
                frame_id = frame_id.wrapping_add(1);
            }
            Outgoing::Audio { .. } => {
                audio_id = audio_id.wrapping_add(1);
            }
        }
    }
}

fn connect_picture(dial: &Dial, shared: Arc<Shared>) -> TcpStream {
    loop {
        match TcpStream::connect(dial.addr) {
            Ok(stream) => {
                if let Err(err) = prepare(&stream) {
                    eprintln!("picture tcp: {err}");
                    thread::sleep(Duration::from_millis(300));
                    continue;
                }
                if let Some((room, invite)) = &dial.relay {
                    if let Err(err) = write_hello(&stream, ROLE_PUBLISH, room, invite) {
                        eprintln!("picture tcp: {err}");
                        thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                }
                let mut reader = match stream.try_clone() {
                    Ok(reader) => reader,
                    Err(err) => {
                        eprintln!("picture tcp: {err}");
                        thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                };
                // The cloned half only reads. A byte from the watcher means the
                // picture it has can no longer continue, so the next frame is a keyframe.
                let want = Arc::clone(&shared);
                thread::Builder::new()
                    .name("picture-tcp-read".to_string())
                    .spawn(move || {
                        let mut buf = [0u8; 32];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) => break,
                                Ok(size) if buf[..size].contains(&1) => {
                                    want.want_idr.store(true, Ordering::Release);
                                }
                                Ok(_) => {}
                                Err(err)
                                    if err.kind() == ErrorKind::TimedOut
                                        || err.kind() == ErrorKind::WouldBlock => {}
                                Err(_) => break,
                            }
                        }
                    })
                    .ok();
                println!("picture tcp: connected to {}", dial.addr);
                return stream;
            }
            Err(err) => {
                eprintln!(
                    "picture tcp: waiting for the watcher at {} ({err})",
                    dial.addr
                );
                thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

fn prepare(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let reference = socket2::SockRef::from(stream);
    // A multi-megabyte buffer is a queue of old pictures. A few frames of
    // room is enough for a burst; anything older is discarded by the sender.
    let _ = reference.set_send_buffer_size(256 * 1024);
    let _ = reference.set_recv_buffer_size(256 * 1024);
    Ok(())
}

fn write_frame(stream: &mut TcpStream, frame_id: u32, frame: &Outgoing) -> std::io::Result<()> {
    let (payload, flags, visible, x, y, timestamp_us) = match frame {
        Outgoing::Video {
            payload,
            keyframe,
            timestamp_us,
            cursor,
        } => (
            payload.as_slice(),
            if *keyframe { FLAG_KEY } else { 0 },
            u8::from(cursor.visible),
            cursor.x,
            cursor.y,
            *timestamp_us,
        ),
        Outgoing::Audio {
            payload,
            timestamp_us,
        } => (payload.as_slice(), FLAG_AUDIO, 0, 0, 0, *timestamp_us),
    };
    let mut bytes = Vec::with_capacity(HEADER + payload.len());
    bytes.extend_from_slice(&MAGIC.to_le_bytes());
    bytes.push(VERSION);
    bytes.push(flags);
    bytes.push(visible);
    bytes.push(0);
    bytes.extend_from_slice(&x.to_le_bytes());
    bytes.extend_from_slice(&y.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&frame_id.to_le_bytes());
    bytes.extend_from_slice(&timestamp_us.to_le_bytes());
    bytes.extend_from_slice(payload);
    stream.write_all(&bytes)?;
    Ok(())
}

pub fn write_hello(mut stream: &TcpStream, role: u8, room: &str, invite: &str) -> std::io::Result<()> {
    let room_bytes = room.as_bytes();
    let invite_bytes = invite.as_bytes();
    if room_bytes.len() > 64 || invite_bytes.len() > 64 || room_bytes.is_empty() || invite_bytes.is_empty()
    {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "room and invite must be 1 to 64 bytes",
        ));
    }
    let mut bytes = Vec::with_capacity(9 + room_bytes.len() + invite_bytes.len());
    bytes.extend_from_slice(&HELLO_MAGIC.to_le_bytes());
    bytes.push(HELLO_VERSION);
    bytes.push(role);
    bytes.push(room_bytes.len() as u8);
    bytes.extend_from_slice(&(invite_bytes.len() as u16).to_le_bytes());
    bytes.extend_from_slice(room_bytes);
    bytes.extend_from_slice(invite_bytes);
    stream.write_all(&bytes)
}

pub fn read_hello(mut stream: &TcpStream) -> std::io::Result<Hello> {
    let mut prefix = [0u8; 9];
    stream.read_exact(&mut prefix)?;
    if prefix[0..4] != HELLO_MAGIC.to_le_bytes() || prefix[4] != HELLO_VERSION {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "picture relay hello was not a join",
        ));
    }
    let room_len = prefix[6] as usize;
    let invite_len = u16::from_le_bytes(prefix[7..9].try_into().unwrap()) as usize;
    if room_len == 0 || room_len > 64 || invite_len == 0 || invite_len > 64 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "picture relay hello was the wrong size",
        ));
    }
    let mut body = vec![0u8; room_len + invite_len];
    stream.read_exact(&mut body)?;
    let room = std::str::from_utf8(&body[..room_len])
        .map_err(|err| std::io::Error::new(ErrorKind::InvalidData, err))?
        .to_string();
    let invite = std::str::from_utf8(&body[room_len..])
        .map_err(|err| std::io::Error::new(ErrorKind::InvalidData, err))?
        .to_string();
    Ok(Hello {
        role: prefix[5],
        room,
        invite,
    })
}

pub fn connect_watch(addr: SocketAddr, room: &str, invite: &str) -> std::io::Result<TcpStream> {
    let mut stream = TcpStream::connect(addr)?;
    prepare(&stream)?;
    write_hello(&stream, ROLE_WATCH, room, invite)?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    stream.write_all(&[1])?;
    Ok(stream)
}

/// Reads length-prefixed frames without dropping bytes when a read times out.
///
/// `read_exact` gives up the bytes it already took if the next wait times out.
/// The following read then treats the middle of a picture as a header, the
/// connection is thrown away, and the sender starts over.
pub struct FrameReader {
    stream: TcpStream,
    pending: Vec<u8>,
}

impl FrameReader {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            pending: Vec::with_capacity(64 * 1024),
        }
    }

    pub fn read_frame(&mut self) -> std::io::Result<Option<PictureFrame>> {
        loop {
            if let Err(err) = self.align() {
                return match err.kind() {
                    ErrorKind::UnexpectedEof if self.pending.is_empty() => Ok(None),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock if self.pending.len() < HEADER => {
                        Err(err)
                    }
                    _ => Err(err),
                };
            }
            let header: [u8; HEADER] = self.pending[..HEADER].try_into().unwrap();
            let payload_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
            if payload_len == 0 || payload_len > MAX_PAYLOAD {
                self.pending.drain(..1);
                continue;
            }
            let total = HEADER + payload_len;
            self.pull(total)?;
            let payload = self.pending[HEADER..total].to_vec();
            self.pending.drain(..total);
            return Ok(Some(PictureFrame {
                payload,
                keyframe: header[5] & FLAG_KEY != 0,
                timestamp_us: i64::from_le_bytes(header[20..28].try_into().unwrap()),
                cursor: CursorSample {
                    visible: header[6] != 0,
                    x: u16::from_le_bytes(header[8..10].try_into().unwrap()),
                    y: u16::from_le_bytes(header[10..12].try_into().unwrap()),
                },
                frame_id: u32::from_le_bytes(header[16..20].try_into().unwrap()),
                audio: header[5] & FLAG_AUDIO != 0,
            }));
        }
    }

    fn align(&mut self) -> std::io::Result<()> {
        let magic = MAGIC.to_le_bytes();
        loop {
            if self.pending.len() >= 4 {
                if let Some(at) = self.pending.windows(4).position(|word| word == magic) {
                    if at > 0 {
                        self.pending.drain(..at);
                    }
                    if self.pending.len() < HEADER {
                        self.pull(HEADER)?;
                        continue;
                    }
                    if self.pending[4] == VERSION {
                        return Ok(());
                    }
                    self.pending.drain(..1);
                    continue;
                }
                let keep = self.pending.len().min(3);
                self.pending.drain(..self.pending.len() - keep);
            }
            let have = self.pending.len();
            self.pull(have + 1)?;
        }
    }

    fn pull(&mut self, need: usize) -> std::io::Result<()> {
        let mut scratch = [0u8; 32 * 1024];
        let mut stalls = 0u32;
        while self.pending.len() < need {
            match self.stream.read(&mut scratch) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "picture tcp closed",
                    ));
                }
                Ok(size) => {
                    stalls = 0;
                    self.pending.extend_from_slice(&scratch[..size]);
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err)
                    if (err.kind() == ErrorKind::TimedOut || err.kind() == ErrorKind::WouldBlock)
                        && !self.pending.is_empty() =>
                {
                    stalls += 1;
                    if stalls > 15 {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

pub fn listen(addr: SocketAddr) -> Result<TcpListener> {
    let listener = TcpListener::bind(addr).with_context(|| format!("picture tcp listen {addr} failed"))?;
    listener.set_nonblocking(true).context("picture tcp listen")?;
    println!("picture tcp: listening on {addr}");
    Ok(listener)
}

pub fn accept_picture(listener: &TcpListener) -> std::io::Result<Option<TcpStream>> {
    match listener.accept() {
        Ok((stream, peer)) => {
            prepare(&stream)?;
            stream.set_read_timeout(Some(Duration::from_millis(200)))?;
            println!("picture tcp: watcher accepted {peer}");
            Ok(Some(stream))
        }
        Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn request_keyframe(stream: &mut TcpStream) {
    let _ = stream.write_all(&[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_delivers_every_frame_in_order() {
        let listener = listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(false).unwrap();
        let sender = PictureSender::connect(addr).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = FrameReader::new(stream);
        let cursor = CursorSample {
            visible: true,
            x: 3,
            y: 9,
        };
        sender.offer(&[1, 2, 3, 4], true, 1000, Some(cursor));
        sender.offer(&[9, 9, 9], false, 2000, None);
        let first = reader.read_frame().unwrap().unwrap();
        let second = reader.read_frame().unwrap().unwrap();
        assert_eq!(first.payload, vec![1, 2, 3, 4]);
        assert!(first.keyframe);
        assert_eq!(first.timestamp_us, 1000);
        assert_eq!(first.cursor, cursor);
        assert_eq!(first.frame_id, 0);
        assert!(!first.audio);
        assert_eq!(second.payload, vec![9, 9, 9]);
        assert!(!second.keyframe);
        assert_eq!(second.timestamp_us, 2000);
        assert_eq!(second.frame_id, 1);
    }
}
