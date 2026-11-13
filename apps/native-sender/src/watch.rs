use crate::api;
use crate::config::AppConfig;
use crate::h264::CursorSample;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use livekit::prelude::*;
use livekit::webrtc::prelude::VideoBuffer;
use livekit::webrtc::video_stream::native::{NativeVideoStream, NativeVideoStreamOptions};
#[cfg(target_os = "macos")]
use screencapturekit::cv::CVPixelBuffer;
use reqwest::Client;
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::interval;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
#[cfg(target_os = "macos")]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

const FRAME_GAP: Duration = Duration::from_millis(34);
const PLAYBACK_LIMIT: usize = 12;
const MIRROR_DELAY: Duration = Duration::from_millis(33);
const MIRROR_LIMIT: Duration = Duration::from_millis(100);

enum WatchEvent {
    Frame,
    Vsync,
    Quit,
}

#[derive(Clone)]
struct Presented {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

#[derive(Clone)]
enum Picture {
    Cpu(Presented),
    #[cfg(target_os = "macos")]
    Gpu {
        buffer: CVPixelBuffer,
        full_range: bool,
    },
}

struct Queued {
    picture: Picture,
    due: Instant,
}

/// Plays the sender's capture clock on this machine's refresh.
///
/// A frame is shown when its own capture time comes due. The sender's refresh
/// rate is never a setting here: 60 Hz and 180 Hz both arrive as timestamps.
struct MirrorClock {
    origin_sender: i64,
    origin_local: Instant,
    delay: Duration,
    started: bool,
    /// How far frames sit behind the first frame's mapping. A constant offset
    /// here is pipeline delay, not jitter, so it must not stay in the hold.
    recent_late: Duration,
    steady: u32,
}

impl MirrorClock {
    fn new() -> Self {
        Self {
            origin_sender: 0,
            origin_local: Instant::now(),
            delay: MIRROR_DELAY,
            started: false,
            recent_late: Duration::ZERO,
            steady: 0,
        }
    }

    fn due(&mut self, sender_us: i64, arrived: Instant) -> Instant {
        if !self.started {
            self.started = true;
            self.origin_sender = sender_us;
            self.origin_local = arrived;
            return arrived + self.delay;
        }
        let elapsed = Duration::from_micros(sender_us.saturating_sub(self.origin_sender).max(0) as u64);
        let scheduled = self.origin_local + elapsed;
        let late = arrived.saturating_duration_since(scheduled);
        let early = scheduled.saturating_duration_since(arrived);
        if late > MIRROR_LIMIT + MIRROR_DELAY || early > MIRROR_LIMIT {
            self.reanchor(sender_us, arrived);
            return arrived + self.delay;
        }
        let drift = if late > self.recent_late {
            late - self.recent_late
        } else {
            self.recent_late - late
        };
        self.steady = if drift < Duration::from_millis(12) {
            self.steady.saturating_add(1)
        } else {
            0
        };
        self.recent_late = self.recent_late / 2 + late / 2;
        if self.steady >= 20 && self.recent_late > Duration::from_millis(24) {
            self.origin_local += self.recent_late;
            self.recent_late = Duration::ZERO;
            self.delay = MIRROR_DELAY;
            self.steady = 0;
            return arrived + self.delay;
        }
        let cushion = late.saturating_sub(self.recent_late) + Duration::from_millis(16);
        if cushion > self.delay {
            self.delay = cushion.min(MIRROR_LIMIT);
        } else if self.delay > MIRROR_DELAY {
            let step = Duration::from_millis(2);
            self.delay = if self.delay - MIRROR_DELAY > step {
                self.delay - step
            } else {
                MIRROR_DELAY
            };
        }
        scheduled + self.delay
    }

    fn reanchor(&mut self, sender_us: i64, arrived: Instant) {
        self.origin_sender = sender_us;
        self.origin_local = arrived;
        self.delay = MIRROR_DELAY;
        self.recent_late = Duration::ZERO;
        self.steady = 0;
    }
}

fn push_picture(queue: &Mutex<VecDeque<Queued>>, picture: Picture, due: Instant) -> bool {
    queue
        .lock()
        .map(|mut slot| {
            slot.push_back(Queued { picture, due });
            while slot.len() > PLAYBACK_LIMIT {
                slot.pop_front();
            }
        })
        .is_ok()
}

struct CursorState {
    visible: AtomicBool,
    x: AtomicU16,
    y: AtomicU16,
    seq: AtomicU64,
}

impl CursorState {
    fn new() -> Self {
        Self {
            visible: AtomicBool::new(false),
            x: AtomicU16::new(0),
            y: AtomicU16::new(0),
            seq: AtomicU64::new(0),
        }
    }

    fn set(&self, cursor: CursorSample) {
        self.visible.store(cursor.visible, Ordering::Relaxed);
        self.x.store(cursor.x, Ordering::Relaxed);
        self.y.store(cursor.y, Ordering::Relaxed);
        self.seq.fetch_add(1, Ordering::Release);
    }

    fn get(&self) -> (bool, f32, f32, u64) {
        let seq = self.seq.load(Ordering::Acquire);
        (
            self.visible.load(Ordering::Relaxed),
            f32::from(self.x.load(Ordering::Relaxed)) / 65535.0,
            f32::from(self.y.load(Ordering::Relaxed)) / 65535.0,
            seq,
        )
    }
}

struct WatchApp {
    proxy: EventLoopProxy<WatchEvent>,
    cursor: Arc<CursorState>,
    cursor_drawn: u64,
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    #[cfg(target_os = "macos")]
    presenter: Option<crate::present_macos::Presenter>,
    queue: Arc<Mutex<VecDeque<Queued>>>,
    inflight: VecDeque<Picture>,
    shown: Option<Picture>,
    stop: Arc<AtomicBool>,
    drawn_width: u32,
    drawn_height: u32,
    frame_interval: Duration,
    vsync: bool,
    #[cfg(target_os = "macos")]
    metal_misses: u32,
    showed: bool,
}

pub fn run(config: &AppConfig) -> Result<()> {
    let queue: Arc<Mutex<VecDeque<Queued>>> = Arc::new(Mutex::new(VecDeque::new()));
    let cursor = Arc::new(CursorState::new());
    let stop = Arc::new(AtomicBool::new(false));
    let event_loop = EventLoop::<WatchEvent>::with_user_event()
        .build()
        .context("failed to create the watch window")?;
    let proxy = event_loop.create_proxy();

    let task_queue = Arc::clone(&queue);
    let task_stop = Arc::clone(&stop);
    let task_proxy = proxy.clone();
    let task_cursor = Arc::clone(&cursor);
    let _runtime;
    let picture_thread;
    if let Some(addr) = config.picture_listen {
        println!("watch picture: direct tcp {addr}");
        picture_thread = Some(
            std::thread::Builder::new()
                .name("picture-tcp".to_string())
                .spawn(move || {
                    if let Err(err) = picture_watch(addr, task_queue, task_proxy, task_stop, task_cursor)
                    {
                        eprintln!("watch picture failed: {err:#}");
                    }
                })
                .context("failed to start the picture receiver")?,
        );
        _runtime = None;
    } else {
        picture_thread = None;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to start the watch runtime")?;
        let task_config = config.clone();
        runtime.spawn(async move {
            if let Err(err) = session(task_config, task_queue, task_proxy, task_stop, task_cursor).await {
                eprintln!("watch failed: {err:#}");
            }
        });
        _runtime = Some(runtime);
    }

    let mut app = WatchApp {
        proxy,
        cursor,
        cursor_drawn: 0,
        window: None,
        context: None,
        surface: None,
        #[cfg(target_os = "macos")]
        presenter: None,
        queue,
        inflight: VecDeque::new(),
        shown: None,
        stop,
        drawn_width: 0,
        drawn_height: 0,
        frame_interval: Duration::from_millis(16),
        vsync: false,
        #[cfg(target_os = "macos")]
        metal_misses: 0,
        showed: false,
    };
    let result = event_loop.run_app(&mut app);
    #[cfg(target_os = "macos")]
    crate::present_macos::stop_vsync();
    result.context("watch window stopped")?;
    if let Some(thread) = picture_thread {
        let _ = thread.join();
    }
    Ok(())
}

fn picture_watch(
    addr: std::net::SocketAddr,
    queue: Arc<Mutex<VecDeque<Queued>>>,
    proxy: EventLoopProxy<WatchEvent>,
    stop: Arc<AtomicBool>,
    cursor: Arc<CursorState>,
) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (queue, proxy, stop, cursor);
        anyhow::bail!("TCP picture playback is available on macOS ({addr})");
    }
    #[cfg(target_os = "macos")]
    {
        picture_watch_macos(addr, queue, proxy, stop, cursor)
    }
}

#[cfg(target_os = "macos")]
fn picture_watch_macos(
    addr: std::net::SocketAddr,
    queue: Arc<Mutex<VecDeque<Queued>>>,
    proxy: EventLoopProxy<WatchEvent>,
    stop: Arc<AtomicBool>,
    cursor: Arc<CursorState>,
) -> Result<()> {
    use std::io::ErrorKind;
    use std::net::TcpStream;
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};
    use std::thread;
    use std::time::Duration as Wait;

    use crate::decode_macos::Decoder;
    use crate::picture::{accept_picture, listen, request_keyframe, FrameReader, PictureFrame};

    enum Piped {
        Connected(TcpStream),
        Frame(PictureFrame, Instant),
    }

    let listener = listen(addr)?;
    let (tx, rx) = sync_channel(8);
    let stop_read = Arc::clone(&stop);
    let reader = thread::Builder::new()
        .name("picture-tcp-read".to_string())
        .spawn(move || {
            while !stop_read.load(Ordering::Acquire) {
                let stream = match accept_picture(&listener) {
                    Ok(Some(stream)) => stream,
                    Ok(None) => {
                        thread::sleep(Wait::from_millis(20));
                        continue;
                    }
                    Err(err) => {
                        eprintln!("picture tcp: {err}");
                        break;
                    }
                };
                let ask = match stream.try_clone() {
                    Ok(ask) => ask,
                    Err(err) => {
                        eprintln!("picture tcp: {err}");
                        continue;
                    }
                };
                if tx.send(Piped::Connected(ask)).is_err() {
                    break;
                }
                let mut reader = FrameReader::new(stream);
                loop {
                    if stop_read.load(Ordering::Acquire) {
                        return;
                    }
                    match reader.read_frame() {
                        Ok(Some(frame)) => {
                            let arrived = Instant::now();
                            if tx.send(Piped::Frame(frame, arrived)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(err)
                            if err.kind() == ErrorKind::TimedOut
                                || err.kind() == ErrorKind::WouldBlock =>
                        {
                            continue;
                        }
                        Err(err) => {
                            eprintln!("picture tcp: {err}");
                            break;
                        }
                    }
                }
            }
        })
        .context("failed to read the picture stream")?;

    let started = Instant::now();
    let mut received: u64 = 0;
    let mut gaps: u64 = 0;
    let mut skipped: u64 = 0;
    let mut window_received: u64 = 0;
    let mut window_gaps: u64 = 0;
    let mut last_arrival = Instant::now();
    let mut last_log = Instant::now();
    let mut ask: Option<TcpStream> = None;
    let mut decoder = Decoder::new();
    let mut next_frame: Option<u32> = None;
    let mut clock = MirrorClock::new();
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if last_log.elapsed() >= Duration::from_secs(1) {
            let held = queue.lock().map(|slot| slot.len()).unwrap_or(0);
            println!(
                "watch: fps={window_received:.0} gaps_this_second={window_gaps} held={held} frames={received} gaps={gaps} skipped={skipped} uptime={:.0}s",
                started.elapsed().as_secs_f64()
            );
            window_received = 0;
            window_gaps = 0;
            last_log = Instant::now();
        }
        let message = match rx.recv_timeout(Wait::from_millis(200)) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let (frame, arrival) = match message {
            Piped::Connected(socket) => {
                ask = Some(socket);
                decoder = Decoder::new();
                next_frame = None;
                clock = MirrorClock::new();
                continue;
            }
            Piped::Frame(frame, arrival) => (frame, arrival),
        };
        let follows = match next_frame {
            Some(expected) => frame.frame_id == expected,
            None => frame.keyframe,
        };
        if !frame.keyframe && !follows {
            skipped += 1;
            if let Some(socket) = ask.as_mut() {
                request_keyframe(socket);
            }
            continue;
        }
        cursor.set(frame.cursor);
        let Some(decoded) = decoder.decode(&frame.payload) else {
            next_frame = None;
            skipped += 1;
            if let Some(socket) = ask.as_mut() {
                request_keyframe(socket);
            }
            continue;
        };
        next_frame = Some(frame.frame_id.wrapping_add(1));
        if received > 0 && arrival.duration_since(last_arrival) > FRAME_GAP {
            gaps += 1;
            window_gaps += 1;
        }
        last_arrival = arrival;
        received += 1;
        window_received += 1;
        note_once(&format!(
            "watch picture: tcp nv12 {}x{}",
            decoded.buffer.width(),
            decoded.buffer.height()
        ));
        let picture = Picture::Gpu {
            buffer: decoded.buffer,
            full_range: decoded.full_range,
        };
        let due = clock.due(frame.timestamp_us, arrival);
        if push_picture(&queue, picture, due) {
            let _ = proxy.send_event(WatchEvent::Frame);
        }
    }
    drop(rx);
    let _ = reader.join();
    Ok(())
}

async fn session(
    config: AppConfig,
    queue: Arc<Mutex<VecDeque<Queued>>>,
    proxy: EventLoopProxy<WatchEvent>,
    stop: Arc<AtomicBool>,
    cursor: Arc<CursorState>,
) -> Result<()> {
    let http = Client::new();
    api::health_check(&http, &config.api_base_url).await?;
    let token = api::fetch_token(
        &http,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        &config.client_type,
    )
    .await?;
    println!(
        "watching room '{}' as '{}' ({})",
        config.room_name, config.identity, token.url
    );

    let mut room_options = RoomOptions::default();
    room_options.auto_subscribe = true;
    let (room, mut events) = Room::connect(&token.url, &token.token, room_options)
        .await
        .context("LiveKit room connect failed")?;
    println!("waiting for the screen track. Close the window or press Ctrl+C to stop.");

    let video = wait_for_screen(&mut events, &proxy, &stop).await?;
    println!("showing '{}'", video.name());
    let mut stream = NativeVideoStream::with_options(
        video.rtc_track(),
        NativeVideoStreamOptions {
            queue_size_frames: Some(2),
        },
    );
    let mut ticker = interval(Duration::from_secs(1));
    ticker.tick().await;
    let started = Instant::now();
    let mut received: u64 = 0;
    let mut gaps: u64 = 0;
    let mut window_received: u64 = 0;
    let mut window_gaps: u64 = 0;
    let mut last_arrival = Instant::now();
    let mut clock = MirrorClock::new();
    let clock_origin = Instant::now();

    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = proxy.send_event(WatchEvent::Quit);
                break;
            }
            _ = ticker.tick() => {
                let fps = window_received as f64;
                let held = queue.lock().map(|slot| slot.len()).unwrap_or(0);
                println!(
                    "watch: fps={fps:.0} gaps_this_second={window_gaps} held={held} frames={received} gaps={gaps} uptime={:.0}s",
                    started.elapsed().as_secs_f64()
                );
                window_received = 0;
                window_gaps = 0;
            }
            event = events.recv() => {
                match event {
                    Some(RoomEvent::DataReceived { payload, topic, .. }) if topic.as_deref() == Some("cursor") => {
                        if let Some(sample) = CursorSample::from_bytes(&payload) {
                            cursor.set(sample);
                            let _ = proxy.send_event(WatchEvent::Vsync);
                        }
                    }
                    Some(RoomEvent::Disconnected { reason }) => {
                        eprintln!("livekit disconnected: {reason:?}");
                        let _ = proxy.send_event(WatchEvent::Quit);
                        break;
                    }
                    _ => {}
                }
            }
            frame = stream.next() => {
                let Some(frame) = frame else { break };
                let arrival = Instant::now();
                if received > 0 && arrival.duration_since(last_arrival) > FRAME_GAP {
                    gaps += 1;
                    window_gaps += 1;
                }
                last_arrival = arrival;
                received += 1;
                window_received += 1;
                if let Some(image) = presentable(&frame) {
                    let sender_us = clock_origin.elapsed().as_micros().min(i64::MAX as u128) as i64;
                    let due = clock.due(sender_us, arrival);
                    if push_picture(&queue, image, due) {
                        let _ = proxy.send_event(WatchEvent::Frame);
                    }
                }
            }
        }
    }
    let _ = room.close().await;
    Ok(())
}

async fn wait_for_screen(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    proxy: &EventLoopProxy<WatchEvent>,
    stop: &AtomicBool,
) -> Result<RemoteVideoTrack> {
    loop {
        if stop.load(Ordering::Acquire) {
            anyhow::bail!("watch window closed");
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = proxy.send_event(WatchEvent::Quit);
                anyhow::bail!("stopped before a screen track arrived");
            }
            event = events.recv() => {
                let Some(event) = event else {
                    anyhow::bail!("LiveKit room closed before a screen track arrived");
                };
                match event {
                    RoomEvent::TrackSubscribed { track, .. } => {
                        let RemoteTrack::Video(video) = track else { continue };
                        println!("subscribed to video track '{}'", video.name());
                        if video.name().to_ascii_lowercase().contains("screen") {
                            return Ok(video);
                        }
                    }
                    RoomEvent::Disconnected { reason } => {
                        anyhow::bail!("livekit disconnected: {reason:?}");
                    }
                    _ => {}
                }
            }
        }
    }
}

fn presentable(frame: &livekit::webrtc::video_frame::BoxVideoFrame) -> Option<Picture> {
    let buffer = frame.buffer.as_ref();
    #[cfg(target_os = "macos")]
    if let Some(picture) = gpu_picture(buffer) {
        return Some(picture);
    }
    let image = if let Some(i420) = buffer.as_i420() {
        let (stride_y, stride_u, stride_v) = i420.strides();
        let (y, u, v) = i420.data();
        presented(
            i420.width(),
            i420.height(),
            i420_to_xrgb(y, u, v, i420.width(), i420.height(), stride_y, stride_u, stride_v),
        )
    } else if let Some(nv12) = buffer.as_nv12() {
        let (stride_y, stride_uv) = nv12.strides();
        let (y, uv) = nv12.data();
        presented(
            nv12.width(),
            nv12.height(),
            nv12_to_xrgb(
                y,
                uv,
                nv12.width(),
                nv12.height(),
                stride_y,
                stride_uv,
                ColorRange::Limited,
            ),
        )
    } else {
        presented_native(buffer)
    };
    let image = image.map(Picture::Cpu);
    match &image {
        Some(Picture::Cpu(image)) => note_once(&format!(
            "watch picture: cpu {:?} {}x{}",
            buffer.buffer_type(),
            image.width,
            image.height
        )),
        #[cfg(target_os = "macos")]
        Some(Picture::Gpu { .. }) => {}
        None => note_once(&format!(
            "watch dropped a {}x{} {:?} frame before it could be drawn",
            buffer.width(),
            buffer.height(),
            buffer.buffer_type()
        )),
    }
    image
}

#[cfg(target_os = "macos")]
fn gpu_picture(buffer: &dyn VideoBuffer) -> Option<Picture> {
    let native = buffer.as_native()?;
    let pixel = unsafe { CVPixelBuffer::from_raw_borrowed(native.get_cv_pixel_buffer()) }?;
    let format = pixel.pixel_format();
    let video = u32::from_be_bytes(*b"420v");
    let full = u32::from_be_bytes(*b"420f");
    if format != video && format != full {
        return None;
    }
    if !pixel.is_planar() || pixel.plane_count() < 2 {
        return None;
    }
    note_once(&format!(
        "watch picture: gpu nv12 {}x{}",
        pixel.width(),
        pixel.height()
    ));
    Some(Picture::Gpu {
        buffer: pixel,
        full_range: format == full,
    })
}

fn presented(width: u32, height: u32, pixels: Vec<u32>) -> Option<Presented> {
    if width == 0 || height == 0 || pixels.len() != (width as usize) * (height as usize) {
        return None;
    }
    Some(Presented {
        width,
        height,
        pixels,
    })
}

fn note_once(message: &str) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    if !SEEN.swap(true, Ordering::Relaxed) {
        eprintln!("{message}");
    }
}

#[cfg(target_os = "macos")]
fn presented_native(buffer: &dyn VideoBuffer) -> Option<Presented> {
    let native = buffer.as_native()?;
    let pixel = unsafe { CVPixelBuffer::from_raw_borrowed(native.get_cv_pixel_buffer()) }?;
    let width = u32::try_from(pixel.width()).ok()?;
    let height = u32::try_from(pixel.height()).ok()?;
    let format = pixel.pixel_format();
    let guard = match pixel.lock_read_only() {
        Ok(guard) => guard,
        Err(err) => {
            note_once(&format!("watch could not lock the decoded frame: {err}"));
            return None;
        }
    };
    let nv12_video = u32::from_be_bytes(*b"420v");
    let nv12_full = u32::from_be_bytes(*b"420f");
    let i420_planar = u32::from_be_bytes(*b"y420");
    let bgra = u32::from_be_bytes(*b"BGRA");
    if format == nv12_video || format == nv12_full {
        let y = plane_bytes(&guard, 0, height as usize)?;
        let uv = plane_bytes(&guard, 1, ((height as usize) + 1) / 2)?;
        let stride_y = u32::try_from(guard.bytes_per_row_of_plane(0)).ok()?;
        let stride_uv = u32::try_from(guard.bytes_per_row_of_plane(1)).ok()?;
        return presented(
            width,
            height,
            nv12_to_xrgb(
                y,
                uv,
                width,
                height,
                stride_y,
                stride_uv,
                if format == nv12_full {
                    ColorRange::Full
                } else {
                    ColorRange::Limited
                },
            ),
        );
    }
    if format == i420_planar {
        let y = plane_bytes(&guard, 0, height as usize)?;
        let chroma_height = ((height as usize) + 1) / 2;
        let u = plane_bytes(&guard, 1, chroma_height)?;
        let v = plane_bytes(&guard, 2, chroma_height)?;
        let stride_y = u32::try_from(guard.bytes_per_row_of_plane(0)).ok()?;
        let stride_u = u32::try_from(guard.bytes_per_row_of_plane(1)).ok()?;
        let stride_v = u32::try_from(guard.bytes_per_row_of_plane(2)).ok()?;
        return presented(
            width,
            height,
            i420_to_xrgb(y, u, v, width, height, stride_y, stride_u, stride_v),
        );
    }
    if format == bgra {
        let stride = guard.bytes_per_row();
        let ptr = guard.base_address();
        if ptr.is_null() || stride == 0 {
            return None;
        }
        let len = stride.checked_mul(height as usize)?;
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let stride = u32::try_from(stride).ok()?;
        return presented(width, height, bgra_to_xrgb(bytes, width, height, stride));
    }
    note_once(&format!(
        "watch decoded a native frame in pixel format {:08x}, which is not drawn yet",
        format
    ));
    None
}

#[cfg(not(target_os = "macos"))]
fn presented_native(_buffer: &dyn VideoBuffer) -> Option<Presented> {
    None
}

#[cfg(target_os = "macos")]
fn plane_bytes<'a>(
    guard: &'a screencapturekit::cv::CVPixelBufferLockGuard<'a>,
    plane: usize,
    rows: usize,
) -> Option<&'a [u8]> {
    let ptr = guard.base_address_of_plane(plane)?;
    if ptr.is_null() {
        return None;
    }
    let stride = guard.bytes_per_row_of_plane(plane);
    let len = stride.checked_mul(rows)?;
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn i420_to_xrgb(
    y_plane: &[u8],
    u_plane: &[u8],
    v_plane: &[u8],
    width: u32,
    height: u32,
    stride_y: u32,
    stride_u: u32,
    stride_v: u32,
) -> Vec<u32> {
    let mut pixels = vec![0u32; (width as usize) * (height as usize)];
    for y in 0..height {
        for x in 0..width {
            let luma = y_plane
                .get((y * stride_y + x) as usize)
                .copied()
                .unwrap_or(16);
            let chroma_x = x / 2;
            let chroma_y = y / 2;
            let chroma_u = u_plane
                .get((chroma_y * stride_u + chroma_x) as usize)
                .copied()
                .unwrap_or(128);
            let chroma_v = v_plane
                .get((chroma_y * stride_v + chroma_x) as usize)
                .copied()
                .unwrap_or(128);
            let (r, g, b) = yuv_to_rgb(luma, chroma_u, chroma_v, ColorRange::Limited);
            pixels[(y * width + x) as usize] =
                (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
    }
    pixels
}

fn nv12_to_xrgb(
    y_plane: &[u8],
    uv_plane: &[u8],
    width: u32,
    height: u32,
    stride_y: u32,
    stride_uv: u32,
    range: ColorRange,
) -> Vec<u32> {
    let mut pixels = vec![0u32; (width as usize) * (height as usize)];
    for y in 0..height {
        for x in 0..width {
            let luma = y_plane
                .get((y * stride_y + x) as usize)
                .copied()
                .unwrap_or(16);
            let chroma = ((y / 2) * stride_uv + (x / 2) * 2) as usize;
            let chroma_u = uv_plane.get(chroma).copied().unwrap_or(128);
            let chroma_v = uv_plane.get(chroma + 1).copied().unwrap_or(128);
            let (r, g, b) = yuv_to_rgb(luma, chroma_u, chroma_v, range);
            pixels[(y * width + x) as usize] =
                (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
    }
    pixels
}

fn bgra_to_xrgb(bytes: &[u8], width: u32, height: u32, stride: u32) -> Vec<u32> {
    let mut pixels = vec![0u32; (width as usize) * (height as usize)];
    for y in 0..height {
        for x in 0..width {
            let index = (y * stride + x * 4) as usize;
            let blue = bytes.get(index).copied().unwrap_or(0);
            let green = bytes.get(index + 1).copied().unwrap_or(0);
            let red = bytes.get(index + 2).copied().unwrap_or(0);
            pixels[(y * width + x) as usize] =
                (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue);
        }
    }
    pixels
}

#[derive(Clone, Copy)]
enum ColorRange {
    Limited,
    Full,
}

fn yuv_to_rgb(y: u8, u: u8, v: u8, range: ColorRange) -> (u8, u8, u8) {
    let (c, d, e) = match range {
        ColorRange::Limited => (i32::from(y) - 16, i32::from(u) - 128, i32::from(v) - 128),
        ColorRange::Full => (i32::from(y), i32::from(u) - 128, i32::from(v) - 128),
    };
    let (r, g, b) = match range {
        ColorRange::Limited => (
            (298 * c + 409 * e + 128) >> 8,
            (298 * c - 100 * d - 208 * e + 128) >> 8,
            (298 * c + 516 * d + 128) >> 8,
        ),
        ColorRange::Full => (
            ((c << 8) + 359 * e + 128) >> 8,
            ((c << 8) - 88 * d - 183 * e + 128) >> 8,
            ((c << 8) + 454 * d + 128) >> 8,
        ),
    };
    (clamp_u8(r), clamp_u8(g), clamp_u8(b))
}

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

impl ApplicationHandler<WatchEvent> for WatchApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = match event_loop.create_window(
            winit::window::WindowAttributes::default()
                .with_title("dcmax watch")
                .with_inner_size(winit::dpi::LogicalSize::new(1280, 720)),
        ) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                eprintln!("failed to open the watch window: {err}");
                event_loop.exit();
                return;
            }
        };
        self.frame_interval = frame_interval(&window);
        #[cfg(target_os = "macos")]
        if let Err(err) = self.attach_metal(&window) {
            eprintln!("watch metal presenter unavailable: {err:#}");
            if !self.attach_softbuffer(&window) {
                event_loop.exit();
                return;
            }
        }
        #[cfg(not(target_os = "macos"))]
        if !self.attach_softbuffer(&window) {
            event_loop.exit();
            return;
        }
        #[cfg(target_os = "macos")]
        if self.start_display_link() {
            self.vsync = true;
            eprintln!(
                "watch vsync: this display refreshes every {} ms, and each refresh shows the sender frame that is due",
                self.frame_interval.as_millis()
            );
        }
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WatchEvent) {
        match event {
            WatchEvent::Quit => event_loop.exit(),
            WatchEvent::Vsync | WatchEvent::Frame => self.present_due(),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            self.present_due();
        }
        self.schedule(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.stop.store(true, Ordering::Release);
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                self.present_due();
                self.schedule(event_loop);
            }
            _ => {}
        }
    }
}

impl WatchApp {
    fn attach_softbuffer(&mut self, window: &Arc<Window>) -> bool {
        let context = match softbuffer::Context::new(Arc::clone(window)) {
            Ok(context) => context,
            Err(err) => {
                eprintln!("failed to create the watch surface: {err}");
                return false;
            }
        };
        match softbuffer::Surface::new(&context, Arc::clone(window)) {
            Ok(surface) => self.surface = Some(surface),
            Err(err) => {
                eprintln!("failed to attach the watch surface: {err}");
                return false;
            }
        }
        self.context = Some(context);
        true
    }

    #[cfg(target_os = "macos")]
    fn attach_metal(&mut self, window: &Window) -> Result<()> {
        let handle = window
            .window_handle()
            .map_err(|err| anyhow!("failed to read the watch window: {err}"))?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            anyhow::bail!("the watch window is not an AppKit view");
        };
        self.presenter = Some(crate::present_macos::Presenter::attach(appkit.ns_view.as_ptr())?);
        eprintln!("watch present: metal");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn start_display_link(&self) -> bool {
        let leaked = Box::leak(Box::new(self.proxy.clone()));
        match crate::present_macos::start_vsync(on_vsync, leaked as *mut EventLoopProxy<WatchEvent> as *mut std::ffi::c_void)
        {
            Ok(()) => true,
            Err(err) => {
                eprintln!("watch display link unavailable: {err:#}");
                false
            }
        }
    }

    fn schedule(&mut self, event_loop: &ActiveEventLoop) {
        let due = self
            .queue
            .lock()
            .ok()
            .and_then(|slot| slot.front().map(|item| item.due));
        let Some(due) = due else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        if Instant::now() >= due {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(2)));
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(due));
    }

    fn present_due(&mut self) {
        let cursor_seq = self.cursor.get().3;
        let now = Instant::now();
        let frame = {
            let Ok(mut queue) = self.queue.lock() else {
                return;
            };
            let mut chosen = None;
            while queue.front().is_some_and(|item| item.due <= now) {
                if let Some(item) = queue.pop_front() {
                    chosen = Some(item.picture);
                }
            }
            if chosen.is_none() && queue.len() >= PLAYBACK_LIMIT {
                while queue.len() > 1 {
                    queue.pop_front();
                }
                chosen = queue.pop_front().map(|item| item.picture);
            }
            chosen
        };
        let Some(frame) = frame else {
            if cursor_seq != self.cursor_drawn {
                if let Some(frame) = self.shown.clone() {
                    if self.paint(&frame).unwrap_or(false) {
                        self.cursor_drawn = cursor_seq;
                    }
                }
            }
            return;
        };
        match self.paint(&frame) {
            Ok(true) => {
                self.cursor_drawn = cursor_seq;
                if !self.showed {
                    self.showed = true;
                    eprintln!("watch present: frame on screen");
                }
                self.shown = Some(frame.clone());
                self.hold(frame);
            }
            Ok(false) => {
                if let Ok(mut queue) = self.queue.lock() {
                    queue.push_front(Queued {
                        picture: frame,
                        due: now + Duration::from_millis(16),
                    });
                }
            }
            Err(err) => eprintln!("watch draw failed: {err}"),
        }
    }

    #[cfg(target_os = "macos")]
    fn pointer(&self) -> crate::present_macos::Pointer {
        let (visible, x, y, _) = self.cursor.get();
        crate::present_macos::Pointer { visible, x, y }
    }

    fn hold(&mut self, frame: Picture) {
        #[cfg(target_os = "macos")]
        if matches!(frame, Picture::Gpu { .. }) {
            self.inflight.push_back(frame);
            while self.inflight.len() > 3 {
                self.inflight.pop_front();
            }
            return;
        }
        let _ = frame;
    }

    fn paint(&mut self, picture: &Picture) -> Result<bool> {
        #[cfg(target_os = "macos")]
        if self.presenter.is_some() {
            return self.paint_metal(picture);
        }
        self.paint_softbuffer(picture)
    }

    #[cfg(target_os = "macos")]
    fn paint_metal(&mut self, picture: &Picture) -> Result<bool> {
        let Some(presenter) = self.presenter.as_ref() else {
            return Ok(false);
        };
        let pointer = self.pointer();
        let status = match picture {
            Picture::Gpu { buffer, full_range } => {
                presenter.draw_nv12(buffer.as_ptr(), *full_range, pointer)
            }
            Picture::Cpu(image) => {
                presenter.draw_bgra(&image.pixels, image.width, image.height, pointer)
            }
        };
        match status {
            0 => {
                self.metal_misses = 0;
                Ok(true)
            }
            -3 => {
                let Picture::Gpu { buffer, full_range } = picture else {
                    return Err(anyhow!("the hardware frame has no Metal texture"));
                };
                let image = gpu_to_cpu(buffer, *full_range)
                    .ok_or_else(|| anyhow!("could not read the hardware frame"))?;
                if presenter.draw_bgra(&image.pixels, image.width, image.height, pointer) == 0 {
                    self.metal_misses = 0;
                    return Ok(true);
                }
                self.note_metal_miss(picture)
            }
            -2 => self.note_metal_miss(picture),
            status => Err(anyhow!("failed to present the watch frame ({status})")),
        }
    }

    #[cfg(target_os = "macos")]
    fn note_metal_miss(&mut self, picture: &Picture) -> Result<bool> {
        self.metal_misses = self.metal_misses.saturating_add(1);
        if self.metal_misses < 30 {
            return Ok(false);
        }
        self.use_window_surface();
        self.paint_softbuffer(picture)
    }

    #[cfg(target_os = "macos")]
    fn use_window_surface(&mut self) {
        if self.surface.is_some() {
            self.presenter.take();
            return;
        }
        note_once("watch present: metal had no drawable, drawing on the window surface");
        self.presenter.take();
        if let Some(window) = self.window.clone() {
            self.attach_softbuffer(&window);
        }
    }

    fn paint_softbuffer(&mut self, picture: &Picture) -> Result<bool> {
        let image = match picture {
            Picture::Cpu(image) => image,
            #[cfg(target_os = "macos")]
            Picture::Gpu { buffer, full_range } => {
                let image = gpu_to_cpu(buffer, *full_range)
                    .ok_or_else(|| anyhow!("could not read the hardware frame"))?;
                return self.paint_softbuffer(&Picture::Cpu(image));
            }
        };
        let Some(window) = self.window.as_ref() else {
            return Ok(false);
        };
        let Some(surface) = self.surface.as_mut() else {
            return Ok(false);
        };
        if image.width != self.drawn_width || image.height != self.drawn_height {
            surface
                .resize(
                    NonZeroU32::new(image.width).context("frame width is zero")?,
                    NonZeroU32::new(image.height).context("frame height is zero")?,
                )
                .map_err(|err| anyhow!("failed to resize the watch surface: {err}"))?;
            self.drawn_width = image.width;
            self.drawn_height = image.height;
        }
        let mut buffer = surface
            .buffer_mut()
            .map_err(|err| anyhow!("failed to lock the watch surface: {err}"))?;
        let count = buffer.len().min(image.pixels.len());
        buffer[..count].copy_from_slice(&image.pixels[..count]);
        window.pre_present_notify();
        buffer
            .present()
            .map_err(|err| anyhow!("failed to present the watch frame: {err}"))?;
        Ok(true)
    }
}

#[cfg(target_os = "macos")]
extern "C" fn on_vsync(context: *mut std::ffi::c_void) {
    let proxy = unsafe { &*(context as *const EventLoopProxy<WatchEvent>) };
    let _ = proxy.send_event(WatchEvent::Vsync);
}

#[cfg(target_os = "macos")]
fn gpu_to_cpu(buffer: &CVPixelBuffer, full_range: bool) -> Option<Presented> {
    let width = u32::try_from(buffer.width()).ok()?;
    let height = u32::try_from(buffer.height()).ok()?;
    let guard = buffer.lock_read_only().ok()?;
    let y = plane_bytes(&guard, 0, height as usize)?;
    let uv = plane_bytes(&guard, 1, (height as usize + 1) / 2)?;
    let stride_y = u32::try_from(guard.bytes_per_row_of_plane(0)).ok()?;
    let stride_uv = u32::try_from(guard.bytes_per_row_of_plane(1)).ok()?;
    presented(
        width,
        height,
        nv12_to_xrgb(
            y,
            uv,
            width,
            height,
            stride_y,
            stride_uv,
            if full_range {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
        ),
    )
}

fn frame_interval(window: &Window) -> Duration {
    let millihertz = window
        .current_monitor()
        .and_then(|monitor| monitor.refresh_rate_millihertz())
        .filter(|rate| *rate > 0)
        .unwrap_or(60_000);
    Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz))
        .clamp(Duration::from_millis(4), Duration::from_millis(20))
}

#[cfg(test)]
mod tests {
    use super::{i420_to_xrgb, nv12_to_xrgb, ColorRange, MirrorClock};
    use std::time::{Duration, Instant};

    #[test]
    fn playback_hold_shrinks_once_frames_are_on_time() {
        let mut clock = MirrorClock::new();
        let start = Instant::now();
        let origin = 1_000_000_i64;
        clock.due(origin, start);
        let late_arrival = start + Duration::from_millis(96);
        clock.due(origin + 16_000, late_arrival);
        assert!(
            clock.delay > Duration::from_millis(40),
            "a late frame opens the hold, got {:?}",
            clock.delay
        );
        let mut sender = origin + 32_000;
        let mut arrived = late_arrival;
        for _ in 0..40 {
            arrived += Duration::from_millis(16);
            clock.due(sender, arrived);
            sender += 16_000;
        }
        assert!(
            clock.delay <= Duration::from_millis(40),
            "on-time frames bring the hold back down, got {:?}",
            clock.delay
        );
    }


    #[test]
    fn limited_range_gray_maps_near_white_and_black() {
        let white = i420_to_xrgb(&[235, 235, 235, 235], &[128], &[128], 2, 2, 2, 1, 1);
        let red = (white[0] >> 16) & 0xff;
        assert!(red > 240, "{red}");
        let black = i420_to_xrgb(&[16, 16, 16, 16], &[128], &[128], 2, 2, 2, 1, 1);
        let dark = (black[0] >> 16) & 0xff;
        assert!(dark < 8, "{dark}");
    }

    #[test]
    fn limited_range_nv12_gray_maps_near_white() {
        let white = nv12_to_xrgb(
            &[235, 235, 235, 235],
            &[128, 128],
            2,
            2,
            2,
            2,
            ColorRange::Limited,
        );
        let red = (white[0] >> 16) & 0xff;
        assert!(red > 240, "{red}");
    }
}
