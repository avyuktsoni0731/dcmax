use super::vt::{Flight, VtSession};
use crate::audio::{EncodedAudio, SystemAudio};
use crate::config::AppConfig;
use crate::h264::{unix_us_now, AccessUnit, MAX_FRAME_AGE};
use anyhow::{anyhow, Context, Result};
use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::cv::CVPixelBuffer;
use screencapturekit::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Minimum time between requested keyframes. The regular group of pictures
/// stays one second; this only limits an extra keyframe after a lost frame.
const KEYFRAME_GAP: Duration = Duration::from_millis(32);
static AUDIO_SENT: AtomicBool = AtomicBool::new(false);
static AUDIO_MISS: AtomicBool = AtomicBool::new(false);

struct Controls {
    force_keyframe: Arc<AtomicBool>,
    bitrate_bps: AtomicU64,
    interval_us: AtomicU64,
}

struct Clock {
    origin_us: Option<i64>,
    last_us: i64,
}

struct PendingFrame {
    pixel: CVPixelBuffer,
    timestamp_us: i64,
    captured_at: Instant,
    livekit_us: i64,
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
            captured_at: Instant::now(),
            livekit_us: unix_us_now(),
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
    flight: Arc<Flight>,
    queue_drops: Arc<AtomicU64>,
    stale_skips: Arc<AtomicU64>,
    pub units: mpsc::Receiver<AccessUnit>,
    pub audio: mpsc::Receiver<EncodedAudio>,
    pub capture_backend: String,
    pub encoder_backend: String,
    pub width: u32,
    pub height: u32,
}

impl ScreenEncoder {
    pub fn start(config: &AppConfig) -> Result<Self> {
        let (width, height, display_width, display_height) = output_size(config)?;
        let (tx, rx) = mpsc::channel(4);
        let queue_drops = Arc::new(AtomicU64::new(0));
        let stale_skips = Arc::new(AtomicU64::new(0));
        let flight = Arc::new(Flight::new());
        let bitrate_bps = u64::from(config.bitrate_kbps) * 1000;
        let controls = Arc::new(Controls {
            force_keyframe: Arc::new(AtomicBool::new(false)),
            bitrate_bps: AtomicU64::new(bitrate_bps),
            interval_us: AtomicU64::new(frame_interval_us(config.target_fps)),
        });
        let session = VtSession::create(
            width,
            height,
            config.target_fps,
            bitrate_bps,
            config.gop_frames(),
            tx,
            Arc::clone(&queue_drops),
            Arc::clone(&controls.force_keyframe),
        )?;
        let session = Arc::new(Mutex::new(session));
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
            Arc::clone(&flight),
            Arc::clone(&stale_skips),
        );
        let (stream, audio) = match open_stream(
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
                flight.wake();
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
            flight,
            queue_drops,
            stale_skips,
            units: rx,
            audio,
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

    pub fn set_fps(&self, fps: u32) {
        self.controls
            .interval_us
            .store(frame_interval_us(fps), Ordering::Relaxed);
    }

    pub fn queue_drops(&self) -> u64 {
        self.queue_drops.load(Ordering::Relaxed)
    }

    pub fn stale_skips(&self) -> u64 {
        self.stale_skips.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            if let Err(err) = stream.stop_capture() {
                eprintln!("screen capture stop failed: {err}");
            }
        }
        self.pending.stop.store(true, Ordering::Release);
        self.pending.ready.notify_one();
        self.flight.wake();
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
    flight: Arc<Flight>,
    stale_skips: Arc<AtomicU64>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("screen-encode".to_string())
        .spawn(move || {
            set_encode_priority();
            let mut next_at = Instant::now();
            let mut last_keyframe = Instant::now() - KEYFRAME_GAP;
            let mut opened = false;
            loop {
                if !flight.wait_until_idle(&pending.stop) {
                    break;
                }
                let Some(frame) = wait_for_newest(&pending, next_at) else {
                    break;
                };
                if fatal.lock().map(|guard| guard.is_some()).unwrap_or(true) {
                    break;
                }
                // The slot kept only the newest capture. If that one is already
                // late, encoding it would publish a stale picture.
                if frame.captured_at.elapsed() > MAX_FRAME_AGE {
                    stale_skips.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let due = !opened
                    || (controls.force_keyframe.load(Ordering::Acquire)
                        && last_keyframe.elapsed() >= KEYFRAME_GAP);
                if due {
                    controls.force_keyframe.store(false, Ordering::Release);
                    last_keyframe = Instant::now();
                    opened = true;
                }
                let bitrate_bps = controls.bitrate_bps.load(Ordering::Relaxed);
                let ticket = flight.begin();
                let encoded = session.lock().map(|mut session| {
                    session.encode(
                        frame.pixel.as_ptr(),
                        frame.timestamp_us,
                        frame.livekit_us,
                        due,
                        bitrate_bps,
                        frame.captured_at,
                        &flight,
                        ticket,
                    )
                });
                if let Err(err) =
                    encoded.unwrap_or_else(|err| Err(anyhow!("encoder lock poisoned: {err}")))
                {
                    flight.finish(ticket);
                    if let Ok(mut fatal) = fatal.lock() {
                        if fatal.is_none() {
                            *fatal = Some(err.to_string());
                        }
                    }
                    break;
                }
                let interval_us = controls.interval_us.load(Ordering::Relaxed).max(1);
                next_at = Instant::now() + Duration::from_micros(interval_us);
            }
        })
        .expect("failed to start the screen encode thread")
}

/// Blocks until the frame interval has elapsed, then takes the newest capture.
/// Frames that arrive early stay in the slot and are replaced, so the opening
/// second cannot dump a burst into the encoder.
fn wait_for_newest(pending: &Pending, earliest: Instant) -> Option<PendingFrame> {
    let Ok(mut slot) = pending.slot.lock() else {
        return None;
    };
    loop {
        if pending.stop.load(Ordering::Acquire) {
            return None;
        }
        let now = Instant::now();
        if now < earliest {
            let (next, _) = pending
                .ready
                .wait_timeout(slot, earliest.saturating_duration_since(now))
                .unwrap_or_else(|err| err.into_inner());
            slot = next;
            continue;
        }
        if let Some(frame) = slot.take() {
            return Some(frame);
        }
        slot = pending
            .ready
            .wait(slot)
            .unwrap_or_else(|err| err.into_inner());
    }
}

fn frame_interval_us(fps: u32) -> u64 {
    1_000_000 / u64::from(fps.clamp(15, 240))
}

fn set_encode_priority() {
    extern "C" {
        fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
    }
    // User-interactive keeps the encoder ahead of a game that is using the
    // same media engine. 0x21 is QOS_CLASS_USER_INTERACTIVE.
    unsafe {
        let _ = pthread_set_qos_class_self_np(0x21, 0);
    }
}

pub fn screens() -> Result<Vec<super::ScreenChoice>> {
    let content = SCShareableContent::get().map_err(|err| {
        anyhow!("screen capture failed ({err}). Grant Screen Recording permission to this app, then try again.")
    })?;
    Ok(content
        .displays()
        .iter()
        .enumerate()
        .map(|(index, display)| super::ScreenChoice {
            id: display.display_id(),
            label: format!("Display {} · {}×{}", index + 1, display.width(), display.height()),
            width: display.width(),
            height: display.height(),
        })
        .collect())
}

fn choose_display<'a>(displays: &'a [SCDisplay], id: Option<u32>) -> Option<&'a SCDisplay> {
    if let Some(id) = id {
        if let Some(display) = displays.iter().find(|display| display.display_id() == id) {
            return Some(display);
        }
    }
    displays
        .iter()
        .max_by_key(|display| u64::from(display.width()) * u64::from(display.height()))
}

fn output_size(config: &AppConfig) -> Result<(u32, u32, u32, u32)> {
    let content = SCShareableContent::get().map_err(|err| {
        anyhow!("screen capture failed ({err}). Grant Screen Recording permission to this app, then try again.")
    })?;
    let displays = content.displays();
    let display = choose_display(&displays, config.display_id).context("ScreenCaptureKit did not list a display")?;
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
) -> Result<(SCStream, mpsc::Receiver<EncodedAudio>)> {
    let content = SCShareableContent::get().map_err(|err| {
        anyhow!(
            "screen capture failed ({err}). Grant Screen Recording permission to the terminal running native-sender, then start it again."
        )
    })?;
    let displays = content.displays();
    let display = choose_display(&displays, config.display_id).context("ScreenCaptureKit did not list a display")?;
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
        .with_queue_depth(3)
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(48_000)
        .with_channel_count(2);
    // The microphone already travels on the voice room. Leaving it in the
    // screen capture plays that same voice again, a little later.
    if stream_config.set_captures_microphone(false).is_err() {
        crate::audio::session_log("system audio: this macOS does not separate the microphone");
    }
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
    let (audio_tx, audio_rx) = mpsc::channel(128);
    match SystemAudio::new() {
        Ok(codec) => {
            let audio = AudioHandler {
                codec: Mutex::new(codec),
                tx: audio_tx,
            };
            if let Err(err) = stream.add_output_handler(audio, SCStreamOutputType::Audio) {
                crate::audio::session_log(&format!("system audio: {err}"));
            } else {
                crate::audio::session_log(&format!(
                    "system audio: screencapturekit 48 kHz exclude_app={}",
                    stream_config.excludes_current_process_audio()
                ));
            }
        }
        Err(err) => crate::audio::session_log(&format!("system audio: {err:#}")),
    }
    stream
        .start_capture()
        .map_err(|err| anyhow!("failed to start screen capture: {err}"))?;
    Ok((stream, audio_rx))
}

struct AudioHandler {
    codec: Mutex<SystemAudio>,
    tx: mpsc::Sender<EncodedAudio>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let Some((pcm, channels, rate)) = pcm_from_sample(&sample) else {
            if !AUDIO_MISS.swap(true, Ordering::Relaxed) {
                crate::audio::session_log("system audio: a buffer arrived but had no pcm");
            }
            return;
        };
        let Ok(mut codec) = self.codec.lock() else {
            return;
        };
        // Stamp the first sample, not the moment the packet is finished.
        // The picture uses the same wall clock at capture. The sample's own
        // presentation time is a media clock and does not match it.
        let frames = pcm.len() / channels.max(1);
        let duration_us = (frames as i64).saturating_mul(1_000_000) / i64::from(rate.max(1));
        let start_us = unix_us_now().saturating_sub(duration_us);
        for (timestamp_us, payload, pcm) in codec.push(&pcm, channels, rate, start_us) {
            if !AUDIO_SENT.swap(true, Ordering::Relaxed) {
                crate::audio::session_log("system audio: first packet captured");
            }
            let _ = self.tx.try_send(EncodedAudio {
                payload,
                timestamp_us,
                pcm,
            });
        }
    }
}

fn pcm_from_sample(sample: &CMSampleBuffer) -> Option<(Vec<f32>, usize, u32)> {
    let format = sample.format_description()?;
    let rate = format.audio_sample_rate().unwrap_or(48_000.0).round() as u32;
    let float = format.audio_is_float();
    let bits = format.audio_bits_per_channel().unwrap_or(if float { 32 } else { 16 });
    let list = sample.audio_buffer_list().ok()?;
    if list.num_buffers() == 0 {
        return None;
    }
    let channels = if list.num_buffers() == 1 {
        list.get(0)?.number_channels.max(1) as usize
    } else {
        list.num_buffers()
    };
    if list.num_buffers() == 1 {
        let data = list.get(0)?.data();
        let pcm = bytes_to_f32(data, float, bits);
        if pcm.is_empty() {
            return None;
        }
        return Some((pcm, channels, rate.max(1)));
    }
    let planes: Vec<Vec<f32>> = (0..list.num_buffers())
        .filter_map(|index| list.get(index).map(|buffer| bytes_to_f32(buffer.data(), float, bits)))
        .collect();
    let frames = planes.first().map(Vec::len).unwrap_or(0);
    if frames == 0 {
        return None;
    }
    let mut interleaved = Vec::with_capacity(frames * planes.len());
    for frame in 0..frames {
        for plane in &planes {
            interleaved.push(plane.get(frame).copied().unwrap_or(0.0));
        }
    }
    Some((interleaved, planes.len().max(1), rate.max(1)))
}

fn bytes_to_f32(data: &[u8], _float: bool, bits: u32) -> Vec<f32> {
    if bits == 32 {
        data.chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect()
    } else if bits == 16 {
        data.chunks_exact(2)
            .map(|chunk| f32::from(i16::from_le_bytes(chunk.try_into().unwrap())) / 32768.0)
            .collect()
    } else {
        Vec::new()
    }
}

fn cmtime_to_us(time: CMTime) -> Option<i64> {
    if !time.is_numeric() || time.timescale <= 0 {
        return None;
    }
    i64::try_from(i128::from(time.value) * 1_000_000 / i128::from(time.timescale)).ok()
}
