//! Both apps dial out to here. The relay pairs one publisher with the watchers
//! in a room and copies the picture bytes, including the system-audio packets
//! that share the connection. It does not decode them.

use crate::picture::{read_hello, Hello, ROLE_PUBLISH, ROLE_WATCH};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct Slot {
    tx: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
}

struct Room {
    publisher: Option<Slot>,
    watchers: Vec<Slot>,
    pending_up: Vec<u8>,
}

pub fn run(addr: SocketAddr) -> Result<()> {
    let check: Arc<dyn Fn(&str, &str) -> bool + Send + Sync> = Arc::new(invite_allowed);
    let bound = spawn(addr, check)?;
    println!("picture relay: listening on {bound}");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("relay runtime")?;
    runtime.block_on(async { tokio::signal::ctrl_c().await })?;
    Ok(())
}

pub fn spawn(
    addr: SocketAddr,
    check: Arc<dyn Fn(&str, &str) -> bool + Send + Sync>,
) -> Result<SocketAddr> {
    let listener = TcpListener::bind(addr).with_context(|| format!("picture relay bind {addr}"))?;
    let bound = listener.local_addr().context("picture relay address")?;
    thread::Builder::new()
        .name("picture-relay".to_string())
        .spawn(move || accept_loop(listener, check))
        .context("picture relay thread")?;
    Ok(bound)
}

fn invite_allowed(invite: &str, room: &str) -> bool {
    if std::env::var("RELAY_OPEN").ok().as_deref() == Some("1") {
        return !invite.is_empty() && !room.is_empty();
    }
    let base = std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:4000".to_string());
    let endpoint = format!("{}/invites/check", base.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build();
    let Ok(client) = client else {
        return false;
    };
    let mut request = client
        .post(endpoint)
        .json(&serde_json::json!({ "invite": invite, "roomName": room }));
    if let Ok(secret) = std::env::var("NATIVE_CONTROL_SECRET") {
        request = request.header("x-native-control-secret", secret);
    }
    request
        .send()
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

fn accept_loop(listener: TcpListener, check: Arc<dyn Fn(&str, &str) -> bool + Send + Sync>) {
    let rooms = Arc::new(Mutex::new(HashMap::<String, Room>::new()));
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("picture relay: {err}");
                continue;
            }
        };
        let rooms = Arc::clone(&rooms);
        let check = Arc::clone(&check);
        thread::spawn(move || {
            if let Err(err) = handle(stream, rooms, &check) {
                eprintln!("picture relay: {err}");
            }
        });
    }
}

fn handle(
    stream: TcpStream,
    rooms: Arc<Mutex<HashMap<String, Room>>>,
    check: &Arc<dyn Fn(&str, &str) -> bool + Send + Sync>,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let Hello { role, room, invite } = read_hello(&stream)?;
    if !check(&invite, &room) {
        eprintln!("picture relay: refused {room}");
        let _ = stream.shutdown(Shutdown::Both);
        return Ok(());
    }
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let (tx, rx) = sync_channel::<Vec<u8>>(256);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = stream.try_clone()?;
    thread::spawn(move || write_loop(writer, rx));
    match role {
        ROLE_PUBLISH => {
            println!("picture relay: publisher joined {room}");
            {
                let mut rooms = rooms.lock().expect("relay rooms");
                let entry = rooms.entry(room.clone()).or_insert_with(empty_room);
                if let Some(old) = entry.publisher.replace(Slot {
                    tx: tx.clone(),
                    stop: Arc::clone(&stop),
                }) {
                    old.stop.store(true, Ordering::Release);
                }
                if !entry.pending_up.is_empty() {
                    let pending = std::mem::take(&mut entry.pending_up);
                    let _ = tx.try_send(pending);
                }
            }
            copy_down(stream, &room, &rooms, &stop);
            let mut rooms = rooms.lock().expect("relay rooms");
            if let Some(entry) = rooms.get_mut(&room) {
                if entry
                    .publisher
                    .as_ref()
                    .is_some_and(|slot| Arc::ptr_eq(&slot.stop, &stop))
                {
                    entry.publisher = None;
                }
            }
        }
        ROLE_WATCH => {
            println!("picture relay: watcher joined {room}");
            {
                let mut rooms = rooms.lock().expect("relay rooms");
                let entry = rooms.entry(room.clone()).or_insert_with(empty_room);
                entry.watchers.push(Slot {
                    tx,
                    stop: Arc::clone(&stop),
                });
            }
            copy_up(stream, &room, &rooms, &stop);
            let mut rooms = rooms.lock().expect("relay rooms");
            if let Some(entry) = rooms.get_mut(&room) {
                entry.watchers.retain(|slot| !Arc::ptr_eq(&slot.stop, &stop));
            }
        }
        other => {
            eprintln!("picture relay: unknown role {other}");
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    stop.store(true, Ordering::Release);
    Ok(())
}

fn empty_room() -> Room {
    Room {
        publisher: None,
        watchers: Vec::new(),
        pending_up: Vec::new(),
    }
}

fn copy_down(
    mut stream: TcpStream,
    room: &str,
    rooms: &Mutex<HashMap<String, Room>>,
    stop: &AtomicBool,
) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(size) => {
                let chunk = buf[..size].to_vec();
                let mut rooms = rooms.lock().expect("relay rooms");
                let Some(entry) = rooms.get_mut(room) else {
                    break;
                };
                entry.watchers.retain(|watcher| {
                    if watcher.stop.load(Ordering::Acquire) {
                        return false;
                    }
                    match watcher.tx.try_send(chunk.clone()) {
                        Ok(()) => true,
                        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                            watcher.stop.store(true, Ordering::Release);
                            false
                        }
                    }
                });
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::TimedOut
                    || err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn copy_up(
    mut stream: TcpStream,
    room: &str,
    rooms: &Mutex<HashMap<String, Room>>,
    stop: &AtomicBool,
) {
    let mut buf = [0u8; 64];
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(size) => {
                let chunk = buf[..size].to_vec();
                let mut rooms = rooms.lock().expect("relay rooms");
                let Some(entry) = rooms.get_mut(room) else {
                    break;
                };
                if let Some(publisher) = &entry.publisher {
                    if publisher.tx.try_send(chunk).is_err() {
                        publisher.stop.store(true, Ordering::Release);
                    }
                } else if entry.pending_up.len() < 64 {
                    entry.pending_up.extend_from_slice(&chunk);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::TimedOut
                    || err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn write_loop(mut stream: TcpStream, rx: Receiver<Vec<u8>>) {
    while let Ok(chunk) = rx.recv() {
        if stream.write_all(&chunk).is_err() {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::{connect_watch, FrameReader, PictureSender};
    use std::time::Duration;

    #[test]
    fn relay_delivers_picture_and_audio_in_order() {
        let addr = spawn(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(|invite, room| invite == "test" && room == "room"),
        )
        .unwrap();
        let sender = PictureSender::connect_relay(addr, "room", "test").unwrap();
        let mut watch = None;
        for _ in 0..20 {
            if let Ok(stream) = connect_watch(addr, "room", "test") {
                watch = Some(stream);
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let watch = watch.expect("watcher joined the relay");
        watch
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut asked = false;
        for _ in 0..20 {
            if sender.take_keyframe() {
                asked = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(asked, "the watcher asked for a keyframe");
        sender.offer(&[1, 2, 3, 4], true, 1000, None);
        sender.offer_audio(&[9, 8, 7], 2000);
        let mut reader = FrameReader::new(watch);
        let video = reader.read_frame().unwrap().unwrap();
        let audio = reader.read_frame().unwrap().unwrap();
        assert_eq!(video.payload, vec![1, 2, 3, 4]);
        assert!(video.keyframe);
        assert!(!video.audio);
        assert_eq!(audio.payload, vec![9, 8, 7]);
        assert!(audio.audio);
        assert_eq!(audio.timestamp_us, 2000);
    }
}
