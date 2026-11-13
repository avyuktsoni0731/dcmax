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
const VERSION: u8 = 2;
const HEADER: usize = 28;
const FLAG_KEY: u8 = 0x01;
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

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
            self.shared.need_key.store(true, Ordering::Release);
            self.shared.want_idr.store(true, Ordering::Release);
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
    let mut stream = Some(connect_picture(addr, Arc::clone(&shared)));
    let mut frame_id = 0u32;
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
        if shared.need_key.load(Ordering::Acquire) && !frame.keyframe {
            continue;
        }
        if stream.is_none() {
            stream = Some(connect_picture(addr, Arc::clone(&shared)));
            frame_id = 0;
            announced = false;
        }
        let socket = stream.as_mut().unwrap();
        if write_frame(socket, frame_id, &frame).is_err() {
            shared.need_key.store(true, Ordering::Release);
            shared.want_idr.store(true, Ordering::Release);
            let _ = socket.shutdown(Shutdown::Both);
            stream = None;
            if frame.keyframe {
                retry = Some(frame);
            }
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if frame.keyframe {
            shared.need_key.store(false, Ordering::Release);
        }
        shared.sent.fetch_add(1, Ordering::Relaxed);
        if !announced {
            announced = true;
            println!(
                "picture tcp: first frame {} bytes",
                frame.payload.len()
            );
        }
        frame_id = frame_id.wrapping_add(1);
    }
}

fn connect_picture(addr: SocketAddr, shared: Arc<Shared>) -> TcpStream {
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => {
                if let Err(err) = prepare(&stream) {
                    eprintln!("picture tcp: {err}");
                    thread::sleep(Duration::from_millis(300));
                    continue;
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
                let want = shared;
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
                println!("picture tcp: connected to {addr}");
                return stream;
            }
            Err(err) => {
                eprintln!("picture tcp: waiting for the watcher at {addr} ({err})");
                thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

fn prepare(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let reference = socket2::SockRef::from(stream);
    let _ = reference.set_send_buffer_size(4 * 1024 * 1024);
    let _ = reference.set_recv_buffer_size(4 * 1024 * 1024);
    Ok(())
}

fn write_frame(stream: &mut TcpStream, frame_id: u32, frame: &Outgoing) -> std::io::Result<()> {
    let mut bytes = Vec::with_capacity(HEADER + frame.payload.len());
    bytes.extend_from_slice(&MAGIC.to_le_bytes());
    bytes.push(VERSION);
    bytes.push(if frame.keyframe { FLAG_KEY } else { 0 });
    bytes.push(u8::from(frame.cursor.visible));
    bytes.push(0);
    bytes.extend_from_slice(&frame.cursor.x.to_le_bytes());
    bytes.extend_from_slice(&frame.cursor.y.to_le_bytes());
    bytes.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&frame_id.to_le_bytes());
    bytes.extend_from_slice(&frame.timestamp_us.to_le_bytes());
    bytes.extend_from_slice(&frame.payload);
    stream.write_all(&bytes)?;
    Ok(())
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
        assert_eq!(second.payload, vec![9, 9, 9]);
        assert!(!second.keyframe);
        assert_eq!(second.timestamp_us, 2000);
        assert_eq!(second.frame_id, 1);
    }
}
