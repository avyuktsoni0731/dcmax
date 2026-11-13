use super::ffmpeg;
use crate::config::{AppConfig, EncoderBackend};
use crate::h264::{monotonic_unix_us, unix_us_now, AccessUnit, MAX_FRAME_AGE};
use anyhow::{anyhow, Result};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

const KEYFRAME_GAP: Duration = Duration::from_secs(1);
const OUTPUT_CAP: usize = 8 * 1024 * 1024;

#[repr(C)]
struct NvencSession {
    _private: [u8; 0],
}

extern "C" {
    fn nvenc_open(
        device: *mut c_void,
        width: i32,
        height: i32,
        fps: i32,
        bitrate_bps: i32,
        gop: i32,
    ) -> *mut NvencSession;
    fn nvenc_open_error() -> *const c_char;
    fn nvenc_last_error(session: *mut NvencSession) -> *const c_char;
    fn nvenc_blit(session: *mut NvencSession, texture: *mut c_void) -> i32;
    fn nvenc_encode(
        session: *mut NvencSession,
        force_idr: i32,
        out: *mut u8,
        out_cap: i32,
        out_len: *mut i32,
        is_key: *mut i32,
    ) -> i32;
    fn nvenc_set_bitrate(session: *mut NvencSession, bitrate_bps: i32) -> i32;
    fn nvenc_close(session: *mut NvencSession);
}

enum Ready {
    Waiting,
    Started,
    Failed(String),
}

struct Captured {
    at: Instant,
    unix_us: i64,
}

struct Shared {
    stop: AtomicBool,
    session: AtomicPtr<NvencSession>,
    pending: Mutex<Option<Captured>>,
    pending_cv: Condvar,
    ready: Mutex<Ready>,
    ready_cv: Condvar,
    fatal: Mutex<Option<String>>,
    force_keyframe: AtomicBool,
    bitrate_bps: AtomicU64,
    queue_drops: AtomicU64,
    stale_skips: AtomicU64,
    width: u32,
    height: u32,
    fps: u32,
    gop: u32,
}

struct CaptureHandler {
    shared: Arc<Shared>,
    _device: ID3D11Device,
}

impl GraphicsCaptureApiHandler for CaptureHandler {
    type Flags = Arc<Shared>;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let shared = ctx.flags;
        let device = ctx.device;
        let session = unsafe {
            nvenc_open(
                device.as_raw(),
                shared.width as i32,
                shared.height as i32,
                shared.fps as i32,
                shared.bitrate_bps.load(Ordering::Relaxed) as i32,
                shared.gop as i32,
            )
        };
        if session.is_null() {
            let message = last_error(std::ptr::null_mut());
            mark_failed(&shared, message.clone());
            return Err(anyhow!(message));
        }
        shared.session.store(session, Ordering::Release);
        if let Ok(mut ready) = shared.ready.lock() {
            *ready = Ready::Started;
        }
        shared.ready_cv.notify_all();
        Ok(Self {
            shared,
            _device: device,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let session = self.shared.session.load(Ordering::Acquire);
        let status = unsafe { nvenc_blit(session, frame.as_raw_texture().as_raw()) };
        if status != 0 {
            let message = last_error(session);
            remember_fatal(&self.shared, &message);
            return Err(anyhow!(message));
        }
        if let Ok(mut slot) = self.shared.pending.lock() {
            *slot = Some(Captured {
                at: Instant::now(),
                unix_us: unix_us_now(),
            });
            self.shared.pending_cv.notify_one();
        }
        Ok(())
    }
}

struct NvencEncoder {
    shared: Arc<Shared>,
    control: Option<CaptureControl<CaptureHandler, anyhow::Error>>,
    worker: Option<JoinHandle<()>>,
}

pub struct ScreenEncoder {
    inner: Inner,
    pub units: mpsc::Receiver<AccessUnit>,
    pub capture_backend: String,
    pub encoder_backend: String,
    pub width: u32,
    pub height: u32,
}

enum Inner {
    Nvenc(NvencEncoder),
    Ffmpeg(ffmpeg::ScreenEncoder),
}

impl ScreenEncoder {
    pub fn start(config: &AppConfig) -> Result<Self> {
        if config.encoder_backend == EncoderBackend::FfmpegLibx264 {
            let mut ffmpeg = ffmpeg::ScreenEncoder::start(config)?;
            let units = {
                let (_tx, rx) = mpsc::channel(1);
                std::mem::replace(&mut ffmpeg.units, rx)
            };
            return Ok(Self {
                capture_backend: ffmpeg.capture_backend.clone(),
                encoder_backend: ffmpeg.encoder_backend.clone(),
                width: ffmpeg.width,
                height: ffmpeg.height,
                units,
                inner: Inner::Ffmpeg(ffmpeg),
            });
        }
        start_nvenc(config)
    }

    pub fn label(&self) -> String {
        format!("{}+{}", self.capture_backend, self.encoder_backend)
    }

    pub fn stderr_tail(&self) -> String {
        match &self.inner {
            Inner::Nvenc(_) => String::new(),
            Inner::Ffmpeg(encoder) => encoder.stderr_tail(),
        }
    }

    pub fn stop(&mut self) {
        match &mut self.inner {
            Inner::Nvenc(encoder) => encoder.stop(),
            Inner::Ffmpeg(encoder) => encoder.stop(),
        }
    }

    pub fn fatal_error(&mut self) -> Result<Option<String>> {
        match &mut self.inner {
            Inner::Nvenc(encoder) => Ok(encoder
                .shared
                .fatal
                .lock()
                .ok()
                .and_then(|guard| guard.clone())),
            Inner::Ffmpeg(encoder) => encoder.fatal_error(),
        }
    }

    pub fn request_keyframe(&self) {
        if let Inner::Nvenc(encoder) = &self.inner {
            encoder.shared.force_keyframe.store(true, Ordering::Release);
        }
    }

    pub fn set_bitrate_bps(&self, bps: u64) {
        match &self.inner {
            Inner::Nvenc(encoder) => {
                encoder
                    .shared
                    .bitrate_bps
                    .store(bps.clamp(800_000, 50_000_000), Ordering::Relaxed);
            }
            Inner::Ffmpeg(encoder) => encoder.set_bitrate_bps(bps),
        }
    }

    pub fn queue_drops(&self) -> u64 {
        match &self.inner {
            Inner::Nvenc(encoder) => encoder.shared.queue_drops.load(Ordering::Relaxed),
            Inner::Ffmpeg(encoder) => encoder.queue_drops(),
        }
    }

    pub fn stale_skips(&self) -> u64 {
        match &self.inner {
            Inner::Nvenc(encoder) => encoder.shared.stale_skips.load(Ordering::Relaxed),
            Inner::Ffmpeg(encoder) => encoder.stale_skips(),
        }
    }
}

impl Drop for ScreenEncoder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start_nvenc(config: &AppConfig) -> Result<ScreenEncoder> {
    let monitor = Monitor::primary().map_err(|err| {
        anyhow!("Windows Graphics Capture could not find the primary monitor ({err}). Start native-sender in the interactive desktop session.")
    })?;
    let display_width = monitor
        .width()
        .map_err(|err| anyhow!("monitor width: {err}"))?;
    let display_height = monitor
        .height()
        .map_err(|err| anyhow!("monitor height: {err}"))?;
    let width = even_min(config.width, display_width);
    let height = even_min(config.height, display_height);
    if width < 2 || height < 2 {
        anyhow::bail!(
            "primary monitor is {display_width}x{display_height}, which is too small to encode"
        );
    }
    let adapter = monitor
        .device_string()
        .unwrap_or_else(|_| "display".to_string());
    println!(
        "screen encoder: capture=windows-graphics-capture codec=h264_nvenc {width}x{height} @ {}fps {} kbps gop={} adapter={adapter}",
        config.target_fps,
        config.bitrate_kbps,
        config.gop_frames(),
    );
    if width != config.width || height != config.height {
        println!(
            "screen encoder: requested {}x{}, encoding {width}x{height} from the {display_width}x{display_height} primary monitor",
            config.width, config.height
        );
    }

    let bitrate_bps = u64::from(config.bitrate_kbps) * 1000;
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        session: AtomicPtr::new(std::ptr::null_mut()),
        pending: Mutex::new(None),
        pending_cv: Condvar::new(),
        ready: Mutex::new(Ready::Waiting),
        ready_cv: Condvar::new(),
        fatal: Mutex::new(None),
        force_keyframe: AtomicBool::new(false),
        bitrate_bps: AtomicU64::new(bitrate_bps),
        queue_drops: AtomicU64::new(0),
        stale_skips: AtomicU64::new(0),
        width,
        height,
        fps: config.target_fps.max(1),
        gop: config.gop_frames().max(1),
    });
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        Arc::clone(&shared),
    );
    let control = CaptureHandler::start_free_threaded(settings).map_err(|err| {
        anyhow!("Windows Graphics Capture failed to start ({err}). Start native-sender in the interactive desktop session, not from an SSH shell.")
    })?;
    if let Err(err) = wait_until_started(&shared) {
        shared.stop.store(true, Ordering::Release);
        let _ = control.stop();
        return Err(err);
    }

    let (tx, rx) = mpsc::channel(4);
    let worker = spawn_encode_thread(Arc::clone(&shared), tx, frame_interval(config.target_fps));
    Ok(ScreenEncoder {
        inner: Inner::Nvenc(NvencEncoder {
            shared,
            control: Some(control),
            worker: Some(worker),
        }),
        units: rx,
        capture_backend: "windows-graphics-capture".to_string(),
        encoder_backend: "h264_nvenc".to_string(),
        width,
        height,
    })
}

impl NvencEncoder {
    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.pending_cv.notify_all();
        self.shared.ready_cv.notify_all();
        if let Some(control) = self.control.take() {
            if let Err(err) = control.stop() {
                eprintln!("screen capture stop failed: {err}");
            }
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let session = self
            .shared
            .session
            .swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !session.is_null() {
            unsafe { nvenc_close(session) };
        }
    }
}

fn spawn_encode_thread(
    shared: Arc<Shared>,
    tx: mpsc::Sender<AccessUnit>,
    interval: Duration,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("screen-encode".to_string())
        .spawn(move || {
            unsafe {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            }
            let session = shared.session.load(Ordering::Acquire);
            let mut output = vec![0u8; OUTPUT_CAP];
            let mut next_at = Instant::now();
            let mut last_keyframe = Instant::now() - KEYFRAME_GAP;
            let mut opened = false;
            let mut applied_bitrate = shared.bitrate_bps.load(Ordering::Relaxed);
            let mut last_unix_us = 0i64;
            loop {
                if shared.stop.load(Ordering::Acquire) {
                    break;
                }
                if shared
                    .fatal
                    .lock()
                    .map(|guard| guard.is_some())
                    .unwrap_or(true)
                {
                    break;
                }
                let Some(captured) = wait_for_newest(&shared, next_at) else {
                    break;
                };
                let frame_started = Instant::now();
                if captured.at.elapsed() > MAX_FRAME_AGE {
                    shared.stale_skips.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let requested = shared.bitrate_bps.load(Ordering::Relaxed);
                if requested != applied_bitrate {
                    let status = unsafe { nvenc_set_bitrate(session, requested as i32) };
                    if status != 0 {
                        eprintln!("nvenc bitrate change failed: {}", last_error(session));
                    }
                    applied_bitrate = requested;
                }
                let due = !opened
                    || (shared.force_keyframe.load(Ordering::Acquire)
                        && last_keyframe.elapsed() >= KEYFRAME_GAP);
                if due {
                    shared.force_keyframe.store(false, Ordering::Release);
                    last_keyframe = Instant::now();
                    opened = true;
                }
                let mut out_len = 0i32;
                let mut is_key = 0i32;
                let status = unsafe {
                    nvenc_encode(
                        session,
                        i32::from(due),
                        output.as_mut_ptr(),
                        output.len() as i32,
                        &mut out_len,
                        &mut is_key,
                    )
                };
                if status != 0 || out_len <= 0 {
                    remember_fatal(&shared, &last_error(session));
                    break;
                }
                let unit = AccessUnit {
                    payload: output[..out_len as usize].to_vec(),
                    keyframe: is_key != 0,
                    read_at: Instant::now(),
                    captured_at: captured.at,
                    timestamp_us: Some(monotonic_unix_us(&mut last_unix_us, captured.unix_us)),
                };
                match tx.try_send(unit) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        shared.queue_drops.fetch_add(1, Ordering::Relaxed);
                        shared.force_keyframe.store(true, Ordering::Release);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
                // The interval starts when the frame is taken. Adding it after
                // the encode stacks the encode time on top and caps the rate
                // below the target.
                next_at = frame_started + interval;
                if next_at < Instant::now() {
                    next_at = Instant::now();
                }
            }
        })
        .expect("failed to start the screen encode thread")
}

fn wait_for_newest(shared: &Shared, earliest: Instant) -> Option<Captured> {
    let Ok(mut slot) = shared.pending.lock() else {
        return None;
    };
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return None;
        }
        let now = Instant::now();
        if now < earliest {
            let (next, _) = shared
                .pending_cv
                .wait_timeout(slot, earliest.saturating_duration_since(now))
                .unwrap_or_else(|err| err.into_inner());
            slot = next;
            continue;
        }
        if let Some(captured) = slot.take() {
            return Some(captured);
        }
        slot = shared
            .pending_cv
            .wait(slot)
            .unwrap_or_else(|err| err.into_inner());
    }
}

fn wait_until_started(shared: &Shared) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut ready = shared
        .ready
        .lock()
        .map_err(|_| anyhow!("NVENC startup lock was poisoned"))?;
    loop {
        match &*ready {
            Ready::Started => return Ok(()),
            Ready::Failed(message) => {
                return Err(anyhow!("{message}"));
            }
            Ready::Waiting => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(anyhow!(
                        "Windows Graphics Capture did not start. Launch native-sender in the interactive desktop session, not from an SSH shell."
                    ));
                }
                let (next, _) = shared
                    .ready_cv
                    .wait_timeout(ready, deadline.saturating_duration_since(now))
                    .unwrap_or_else(|err| err.into_inner());
                ready = next;
            }
        }
    }
}

fn frame_interval(fps: u32) -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)))
}

fn even_min(requested: u32, display: u32) -> u32 {
    let value = requested.min(display);
    value - (value % 2)
}

fn last_error(session: *mut NvencSession) -> String {
    let ptr = unsafe {
        if session.is_null() {
            nvenc_open_error()
        } else {
            nvenc_last_error(session)
        }
    };
    if ptr.is_null() {
        return "NVENC failed".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn mark_failed(shared: &Shared, message: String) {
    if let Ok(mut ready) = shared.ready.lock() {
        *ready = Ready::Failed(message);
    }
    shared.ready_cv.notify_all();
}

fn remember_fatal(shared: &Shared, message: &str) {
    if let Ok(mut fatal) = shared.fatal.lock() {
        if fatal.is_none() {
            *fatal = Some(message.to_string());
        }
    }
}
