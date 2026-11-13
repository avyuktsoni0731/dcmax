use super::vt::VtSession;
use crate::config::AppConfig;
use crate::h264::AccessUnit;
use anyhow::{anyhow, Context, Result};
use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::cv::CVPixelBuffer;
use screencapturekit::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const KEYFRAME_GAP: Duration = Duration::from_secs(1);

struct Controls {
    force_keyframe: AtomicBool,
    bitrate_bps: AtomicU64,
}

struct Clock {
    origin_us: Option<i64>,
    last_us: i64,
}

struct PendingFrame {
    pixel: CVPixelBuffer,
    timestamp_us: i64,
}

struct Pending {
    slot: Mutex<Option<PendingFrame>>,
    ready: Condvar,
    stop: AtomicBool,
}

struct Handler {
    pending: Arc<Pending>,
    fatal: Arc<Mutex<Option<String>>>,
    clock: Mutex<Clock>,
}

impl SCStreamOutputTrait for Handler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Screen || self.pending.stop.load(Ordering::Acquire) {
            return;
        }
        if self
            .fatal
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(true)
        {
            return;
        }
        let Some(pixel) = sample.pixel_buffer() else {
            return;
        };
        let frame = PendingFrame {
            pixel,
            timestamp_us: self.capture_timestamp(sample.presentation_timestamp()),
        };
        // Keep the newest frame only. Encoding stays on its own thread, so a
        // slow encoder cannot stall ScreenCaptureKit.
        if let Ok(mut slot) = self.pending.slot.lock() {
            *slot = Some(frame);
            self.pending.ready.notify_one();
        }
    }
}

impl Handler {
    fn capture_timestamp(&self, time: CMTime) -> i64 {
        let absolute = cmtime_to_us(time);
        let Ok(mut clock) = self.clock.lock() else {
            return 0;
        };
        let relative = match absolute {
            Some(us) => {
                let origin = *clock.origin_us.get_or_insert(us);
                us.saturating_sub(origin)
            }
            None => clock.last_us.saturating_add(1),
        };
        let timestamp = if relative <= clock.last_us {
            clock.last_us.saturating_add(1)
        } else {
            relative
        };
        clock.last_us = timestamp;
        timestamp
    }
}

pub struct ScreenEncoder {
    stream: Option<SCStream>,
    session: Arc<Mutex<VtSession>>,
    pending: Arc<Pending>,
    encode_thread: Option<JoinHandle<()>>,
    fatal: Arc<Mutex<Option<String>>>,
    controls: Arc<Controls>,
    queue_drops: Arc<AtomicU64>,
    pub units: mpsc::Receiver<AccessUnit>,
    pub capture_backend: String,
    pub encoder_backend: String,
    pub width: u32,
    pub height: u32,
}

impl ScreenEncoder {
    pub fn start(config: &AppConfig) -> Result<Self> {
        let (width, height, display_width, display_height) = output_size(config)?;
        let (tx, rx) = mpsc::channel(32);
        let queue_drops = Arc::new(AtomicU64::new(0));
        let bitrate_bps = u64::from(config.bitrate_kbps) * 1000;
        let session = VtSession::create(
            width,
            height,
            config.target_fps,
            bitrate_bps,
            config.gop_frames(),
            tx,
            Arc::clone(&queue_drops),
        )?;
        let session = Arc::new(Mutex::new(session));
        let controls = Arc::new(Controls {
            force_keyframe: AtomicBool::new(false),
            bitrate_bps: AtomicU64::new(bitrate_bps),
        });
        let fatal = Arc::new(Mutex::new(None));
        let pending = Arc::new(Pending {
            slot: Mutex::new(None),
            ready: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let encode_thread = spawn_encode_thread(
            Arc::clone(&pending),
            Arc::clone(&session),
            Arc::clone(&controls),
            Arc::clone(&fatal),
        );
        let stream = match open_stream(
            config,
            width,
            height,
            display_width,
            display_height,
            Arc::clone(&pending),
            Arc::clone(&fatal),
        ) {
            Ok(stream) => stream,
            Err(err) => {
                pending.stop.store(true, Ordering::Release);
                pending.ready.notify_one();
                let _ = encode_thread.join();
                if let Ok(mut session) = session.lock() {
                    session.finish();
                }
                return Err(err);
            }
        };
        Ok(Self {
            stream: Some(stream),
            session,
            pending,
            encode_thread: Some(encode_thread),
            fatal,
            controls,
            queue_drops,
            units: rx,
            capture_backend: "screencapturekit".to_string(),
            encoder_backend: "videotoolbox".to_string(),
            width,
            height,
        })
    }

    pub fn label(&self) -> String {
        format!("{}+{}", self.capture_backend, self.encoder_backend)
    }

    pub fn stderr_tail(&self) -> String {
        self.fatal
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn fatal_error(&mut self) -> Result<Option<String>> {
        let message = self.stderr_tail();
        if message.is_empty() {
            Ok(None)
        } else {
            Ok(Some(message))
        }
    }

    pub fn request_keyframe(&self) {
        self.controls.force_keyframe.store(true, Ordering::Release);
    }

    pub fn set_bitrate_bps(&self, bps: u64) {
        self.controls.bitrate_bps.store(bps, Ordering::Relaxed);
    }

    pub fn queue_drops(&self) -> u64 {
        self.queue_drops.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            if let Err(err) = stream.stop_capture() {
                eprintln!("screen capture stop failed: {err}");
            }
        }
        self.pending.stop.store(true, Ordering::Release);
        self.pending.ready.notify_one();
        if let Some(thread) = self.encode_thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut session) = self.session.lock() {
            session.finish();
        }
    }
}

impl Drop for ScreenEncoder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_encode_thread(
    pending: Arc<Pending>,
    session: Arc<Mutex<VtSession>>,
    controls: Arc<Controls>,
    fatal: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("screen-encode".to_string())
        .spawn(move || {
            let mut last_keyframe = Instant::now() - KEYFRAME_GAP;
            loop {
                let frame = {
                    let Ok(mut slot) = pending.slot.lock() else {
                        break;
                    };
                    while slot.is_none() && !pending.stop.load(Ordering::Acquire) {
                        slot = pending
                            .ready
                            .wait(slot)
                            .unwrap_or_else(|err| err.into_inner());
                    }
                    if slot.is_none() && pending.stop.load(Ordering::Acquire) {
                        break;
                    }
                    slot.take()
                };
                let Some(frame) = frame else { break };
                if fatal.lock().map(|guard| guard.is_some()).unwrap_or(true) {
                    break;
                }
                let due = controls.force_keyframe.load(Ordering::Acquire)
                    && last_keyframe.elapsed() >= KEYFRAME_GAP;
                if due {
                    controls.force_keyframe.store(false, Ordering::Release);
                    last_keyframe = Instant::now();
                }
                let bitrate_bps = controls.bitrate_bps.load(Ordering::Relaxed);
                let encoded = session.lock().map(|mut session| {
                    session.encode(frame.pixel.as_ptr(), frame.timestamp_us, due, bitrate_bps)
                });
                if let Err(err) =
                    encoded.unwrap_or_else(|err| Err(anyhow!("encoder lock poisoned: {err}")))
                {
                    if let Ok(mut fatal) = fatal.lock() {
                        if fatal.is_none() {
                            *fatal = Some(err.to_string());
                        }
                    }
                    break;
                }
            }
        })
        .expect("failed to start the screen encode thread")
}

fn output_size(config: &AppConfig) -> Result<(u32, u32, u32, u32)> {
    let content = SCShareableContent::get().map_err(|err| {
        anyhow!(
            "screen capture failed ({err}). Grant Screen Recording permission to the terminal running native-sender, then start it again."
        )
    })?;
    let displays = content.displays();
    let display = displays
        .iter()
        .max_by_key(|display| u64::from(display.width()) * u64::from(display.height()))
        .context("ScreenCaptureKit did not list a display")?;
    let width = even_min(config.width, display.width());
    let height = even_min(config.height, display.height());
    Ok((width, height, display.width(), display.height()))
}

fn even_min(requested: u32, display: u32) -> u32 {
    let size = requested.min(display).max(2);
    size - (size % 2)
}

fn open_stream(
    config: &AppConfig,
    width: u32,
    height: u32,
    display_width: u32,
    display_height: u32,
    pending: Arc<Pending>,
    fatal: Arc<Mutex<Option<String>>>,
) -> Result<SCStream> {
    let content = SCShareableContent::get().map_err(|err| {
        anyhow!(
            "screen capture failed ({err}). Grant Screen Recording permission to the terminal running native-sender, then start it again."
        )
    })?;
    let displays = content.displays();
    let display = displays
        .iter()
        .max_by_key(|display| u64::from(display.width()) * u64::from(display.height()))
        .context("ScreenCaptureKit did not list a display")?;
    println!(
        "screen encoder: capture=screencapturekit codec=videotoolbox {width}x{height} @ {}fps {} kbps gop={} display={display_width}x{display_height}",
        config.target_fps,
        config.bitrate_kbps,
        config.gop_frames(),
    );
    if width != config.width || height != config.height {
        println!(
            "encode size is the display ({width}x{height}); {}x{} would only upscale",
            config.width, config.height
        );
    }
    let filter = SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build()
        .map_err(|err| anyhow!("failed to build the screen filter: {err}"))?;
    let interval = CMTime::new(1, i32::try_from(config.target_fps).unwrap_or(60));
    let mut stream_config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::YCbCr_420v)
        .with_shows_cursor(true)
        .with_scales_to_fit(true)
        .with_minimum_frame_interval(&interval)
        .with_queue_depth(3);
    if let Err(err) = stream_config.set_preserves_aspect_ratio(true) {
        eprintln!("keeping the display's aspect ratio is unavailable: {err}");
    }
    let handler = Handler {
        pending,
        fatal,
        clock: Mutex::new(Clock {
            origin_us: None,
            last_us: 0,
        }),
    };
    let mut stream = SCStream::new(&filter, &stream_config)
        .map_err(|err| anyhow!("failed to create the screen stream: {err}"))?;
    stream
        .add_output_handler(handler, SCStreamOutputType::Screen)
        .map_err(|err| anyhow!("failed to attach the screen handler: {err}"))?;
    stream
        .start_capture()
        .map_err(|err| anyhow!("failed to start screen capture: {err}"))?;
    Ok(stream)
}

fn cmtime_to_us(time: CMTime) -> Option<i64> {
    if !time.is_numeric() || time.timescale <= 0 {
        return None;
    }
    i64::try_from(i128::from(time.value) * 1_000_000 / i128::from(time.timescale)).ok()
}
