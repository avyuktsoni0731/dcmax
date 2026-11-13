use crate::api;
use crate::config::AppConfig;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use livekit::prelude::*;
use livekit::webrtc::prelude::VideoBuffer;
use livekit::webrtc::video_stream::native::{NativeVideoStream, NativeVideoStreamOptions};
use reqwest::Client;
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::interval;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

const FRAME_GAP: Duration = Duration::from_millis(34);
const PLAYBACK_QUEUE: usize = 2;
const DEFAULT_REFRESH: Duration = Duration::from_nanos(16_666_667);

enum WatchEvent {
    Frame,
    Quit,
}

#[derive(Clone)]
struct Presented {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

struct WatchApp {
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    queue: Arc<Mutex<VecDeque<Arc<Presented>>>>,
    stop: Arc<AtomicBool>,
    drawn_width: u32,
    drawn_height: u32,
    next_present: Instant,
    refresh: Duration,
}

pub fn run(config: &AppConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the watch runtime")?;
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let event_loop = EventLoop::<WatchEvent>::with_user_event()
        .build()
        .context("failed to create the watch window")?;
    let proxy = event_loop.create_proxy();

    let task_config = config.clone();
    let task_queue = Arc::clone(&queue);
    let task_stop = Arc::clone(&stop);
    runtime.spawn(async move {
        if let Err(err) = session(task_config, task_queue, proxy, task_stop).await {
            eprintln!("watch failed: {err:#}");
        }
    });

    let mut app = WatchApp {
        window: None,
        context: None,
        surface: None,
        queue,
        stop,
        drawn_width: 0,
        drawn_height: 0,
        next_present: Instant::now(),
        refresh: DEFAULT_REFRESH,
    };
    event_loop
        .run_app(&mut app)
        .context("watch window stopped")?;
    Ok(())
}

async fn session(
    config: AppConfig,
    queue: Arc<Mutex<VecDeque<Arc<Presented>>>>,
    proxy: EventLoopProxy<WatchEvent>,
    stop: Arc<AtomicBool>,
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
                println!(
                    "watch: fps={fps:.0} gaps_this_second={window_gaps} frames={received} gaps={gaps} uptime={:.0}s",
                    started.elapsed().as_secs_f64()
                );
                window_received = 0;
                window_gaps = 0;
            }
            event = events.recv() => {
                if let Some(RoomEvent::Disconnected { reason }) = event {
                    eprintln!("livekit disconnected: {reason:?}");
                    let _ = proxy.send_event(WatchEvent::Quit);
                    break;
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
                    let queued = queue.lock().map(|mut slot| {
                        slot.push_back(Arc::new(image));
                        while slot.len() > PLAYBACK_QUEUE {
                            slot.pop_front();
                        }
                    });
                    if queued.is_ok() {
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

fn presentable(frame: &livekit::webrtc::video_frame::BoxVideoFrame) -> Option<Presented> {
    let buffer = frame.buffer.as_ref();
    let i420 = buffer.as_i420()?;
    let (stride_y, stride_u, stride_v) = i420.strides();
    let (y, u, v) = i420.data();
    let width = i420.width();
    let height = i420.height();
    if width == 0 || height == 0 {
        return None;
    }
    Some(Presented {
        width,
        height,
        pixels: i420_to_xrgb(y, u, v, width, height, stride_y, stride_u, stride_v),
    })
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
            let (r, g, b) = yuv_to_rgb(luma, chroma_u, chroma_v);
            pixels[(y * width + x) as usize] =
                (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
    }
    pixels
}

fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let c = i32::from(y) - 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let r = (298 * c + 409 * e + 128) >> 8;
    let g = (298 * c - 100 * d - 208 * e + 128) >> 8;
    let b = (298 * c + 516 * d + 128) >> 8;
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
        let context = match softbuffer::Context::new(Arc::clone(&window)) {
            Ok(context) => context,
            Err(err) => {
                eprintln!("failed to create the watch surface: {err}");
                event_loop.exit();
                return;
            }
        };
        match softbuffer::Surface::new(&context, Arc::clone(&window)) {
            Ok(surface) => self.surface = Some(surface),
            Err(err) => {
                eprintln!("failed to attach the watch surface: {err}");
                event_loop.exit();
                return;
            }
        }
        self.refresh = refresh_interval(&window);
        self.context = Some(context);
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WatchEvent) {
        match event {
            WatchEvent::Quit => event_loop.exit(),
            WatchEvent::Frame => self.schedule(event_loop),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.schedule(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.stop.store(true, Ordering::Release);
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if Instant::now() < self.next_present {
                    self.schedule(event_loop);
                    return;
                }
                match self.draw() {
                    Ok(true) => self.next_present = Instant::now() + self.refresh,
                    Ok(false) => {}
                    Err(err) => eprintln!("watch draw failed: {err}"),
                }
                self.schedule(event_loop);
            }
            _ => {}
        }
    }
}

impl WatchApp {
    fn schedule(&mut self, event_loop: &ActiveEventLoop) {
        let pending = self
            .queue
            .lock()
            .map(|slot| !slot.is_empty())
            .unwrap_or(false);
        if !pending {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let now = Instant::now();
        if now >= self.next_present {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_present));
    }

    fn draw(&mut self) -> Result<bool> {
        let Some(window) = self.window.as_ref() else {
            return Ok(false);
        };
        let Some(surface) = self.surface.as_mut() else {
            return Ok(false);
        };
        let image = self
            .queue
            .lock()
            .map_err(|_| anyhow!("watch frame lock poisoned"))?
            .pop_front();
        let Some(image) = image else {
            return Ok(false);
        };
        if image.width != self.drawn_width || image.height != self.drawn_height {
            let _ =
                window.request_inner_size(winit::dpi::PhysicalSize::new(image.width, image.height));
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

fn refresh_interval(window: &Window) -> Duration {
    let millihertz = window
        .current_monitor()
        .and_then(|monitor| monitor.refresh_rate_millihertz())
        .filter(|rate| *rate > 0)
        .unwrap_or(60_000);
    Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz))
}

#[cfg(test)]
mod tests {
    use super::i420_to_xrgb;

    #[test]
    fn limited_range_gray_maps_near_white_and_black() {
        let white = i420_to_xrgb(&[235, 235, 235, 235], &[128], &[128], 2, 2, 2, 1, 1);
        let red = (white[0] >> 16) & 0xff;
        assert!(red > 240, "{red}");
        let black = i420_to_xrgb(&[16, 16, 16, 16], &[128], &[128], 2, 2, 2, 1, 1);
        let dark = (black[0] >> 16) & 0xff;
        assert!(dark < 8, "{dark}");
    }
}
