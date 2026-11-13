use crate::config::{AppConfig, EncoderBackend};
use crate::h264::{AccessUnit, AnnexBSplitter};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodecKind {
    Libx264,
    Nvenc,
    VideoToolbox,
}

impl CodecKind {
    fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Libx264 => "libx264",
            Self::Nvenc => "h264_nvenc",
            Self::VideoToolbox => "h264_videotoolbox",
        }
    }
}

struct Control {
    stop: AtomicBool,
    restart: AtomicBool,
    bitrate_bps: AtomicU64,
    /// Wall-clock millis when the current process was spawned. 0 if none.
    born_at_ms: AtomicU64,
    fatal: Mutex<Option<String>>,
    child: Mutex<Option<Child>>,
}

pub struct ScreenEncoder {
    control: Arc<Control>,
    worker: Option<JoinHandle<()>>,
    pub units: mpsc::Receiver<AccessUnit>,
    stderr_tail: Arc<Mutex<String>>,
    pub capture_backend: String,
    pub encoder_backend: String,
    pub width: u32,
    pub height: u32,
}

impl ScreenEncoder {
    pub fn start(config: &AppConfig) -> Result<Self> {
        ensure_ffmpeg()?;
        let encoders = ffmpeg_encoder_list()?;
        let codec = select_codec(config, &encoders);
        let capture_backend = screen_input_name();
        println!(
            "screen encoder: capture={capture_backend} codec={} {}x{} @ {}fps {} kbps gop={} (cli capture {:?} is not a second pipeline)",
            codec.ffmpeg_name(),
            config.width,
            config.height,
            config.target_fps,
            config.bitrate_kbps,
            config.gop_frames(),
            config.capture_backend
        );
        spawn_encoder(config, codec, &capture_backend)
    }

    pub fn label(&self) -> String {
        format!("{}+{}", self.capture_backend, self.encoder_backend)
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn stop(&mut self) {
        self.control.stop.store(true, Ordering::Release);
        self.kill_child();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    pub fn fatal_error(&mut self) -> Result<Option<String>> {
        Ok(self
            .control
            .fatal
            .lock()
            .ok()
            .and_then(|guard| guard.clone()))
    }

    /// A new ffmpeg process does start on an IDR, which is what a keyframe
    /// request wants. Killing the capture to get that IDR also drops the
    /// picture for the whole startup, so this stand-in does not do it.
    /// In-process NVENC has to answer the request without a new process.
    pub fn request_keyframe(&self) {}

    pub fn set_bitrate_bps(&self, bps: u64) {
        self.control
            .bitrate_bps
            .store(bps.clamp(800_000, 50_000_000), Ordering::Release);
    }

    fn kill_child(&self) {
        if let Ok(mut slot) = self.control.child.lock() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        }
    }

    pub fn queue_drops(&self) -> u64 {
        0
    }

    pub fn stale_skips(&self) -> u64 {
        0
    }
}

impl Drop for ScreenEncoder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn select_codec(config: &AppConfig, encoders: &str) -> CodecKind {
    let preferred = match config.encoder_backend {
        EncoderBackend::Fast | EncoderBackend::FfmpegLibx264 => CodecKind::Libx264,
        EncoderBackend::FfmpegH264Nvenc => {
            if cfg!(target_os = "macos") {
                CodecKind::VideoToolbox
            } else if cfg!(target_os = "windows") {
                CodecKind::Nvenc
            } else {
                CodecKind::Libx264
            }
        }
    };
    if encoder_available(encoders, preferred.ffmpeg_name()) {
        return preferred;
    }
    if preferred != CodecKind::Libx264
        && encoder_available(encoders, CodecKind::Libx264.ffmpeg_name())
    {
        eprintln!(
            "ffmpeg has no {} encoder; using libx264",
            preferred.ffmpeg_name()
        );
        return CodecKind::Libx264;
    }
    preferred
}

struct EncoderPlan {
    codec: CodecKind,
    input_args: Vec<String>,
    scale: String,
    gop: String,
    fps: String,
}

fn spawn_encoder(
    config: &AppConfig,
    codec: CodecKind,
    capture_backend: &str,
) -> Result<ScreenEncoder> {
    if !encoder_available(
        &ffmpeg_encoder_list().unwrap_or_default(),
        codec.ffmpeg_name(),
    ) {
        bail!(
            "ffmpeg is missing the {} encoder. Install a build that includes it, or pass --encoder ffmpeg-libx264.",
            codec.ffmpeg_name()
        );
    }

    let plan = EncoderPlan {
        codec,
        input_args: screen_input_args(config)?,
        scale: format!(
            "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,fps={},format=yuv420p",
            config.width, config.height, config.width, config.height, config.target_fps
        ),
        gop: config.gop_frames().to_string(),
        fps: config.target_fps.to_string(),
    };
    let bitrate_bps = u64::from(config.bitrate_kbps) * 1000;
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        restart: AtomicBool::new(false),
        bitrate_bps: AtomicU64::new(bitrate_bps),
        born_at_ms: AtomicU64::new(0),
        fatal: Mutex::new(None),
        child: Mutex::new(None),
    });
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = mpsc::channel(2);
    let worker_control = Arc::clone(&control);
    let worker_tail = Arc::clone(&stderr_tail);
    let worker = thread::spawn(move || run_ffmpeg(plan, worker_control, worker_tail, tx));

    Ok(ScreenEncoder {
        control,
        worker: Some(worker),
        units: rx,
        stderr_tail,
        capture_backend: capture_backend.to_string(),
        encoder_backend: codec.ffmpeg_name().to_string(),
        width: config.width,
        height: config.height,
    })
}

fn run_ffmpeg(
    plan: EncoderPlan,
    control: Arc<Control>,
    stderr_tail: Arc<Mutex<String>>,
    tx: mpsc::Sender<AccessUnit>,
) {
    loop {
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        let bitrate_bps = control.bitrate_bps.load(Ordering::Acquire).max(800_000);
        let mut child = match ffmpeg_command(&plan, bitrate_bps).spawn() {
            Ok(child) => child,
            Err(err) => {
                remember_fatal(&control, format!("failed to start ffmpeg: {err}"));
                break;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            remember_fatal(&control, "ffmpeg stdout was not piped".to_string());
            break;
        };
        let Some(stderr) = child.stderr.take() else {
            remember_fatal(&control, "ffmpeg stderr was not piped".to_string());
            break;
        };
        eprintln!(
            "ffmpeg started: {} at {} kbps",
            plan.codec.ffmpeg_name(),
            bitrate_bps / 1000
        );
        control.born_at_ms.store(wall_ms(), Ordering::Release);
        // This process's opening keyframe answers whatever was pending.
        control.restart.store(false, Ordering::Release);
        {
            let Ok(mut slot) = control.child.lock() else {
                break;
            };
            *slot = Some(child);
        }
        let tail = Arc::clone(&stderr_tail);
        thread::spawn(move || collect_stderr(stderr, tail));
        if !pump_access_units(stdout, &tx) {
            break;
        }
        if let Ok(mut slot) = control.child.lock() {
            if let Some(mut child) = slot.take() {
                let _ = child.wait();
            }
        }
        control.born_at_ms.store(0, Ordering::Release);
        if control.stop.load(Ordering::Acquire) || tx.is_closed() {
            break;
        }
        if !control.restart.swap(false, Ordering::AcqRel) {
            let tail = stderr_tail
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            remember_fatal(&control, format!("ffmpeg exited. {}", tail.trim()));
            break;
        }
        eprintln!("ffmpeg restarting for a keyframe or a new bitrate");
    }
}

fn pump_access_units(stdout: impl Read, tx: &mpsc::Sender<AccessUnit>) -> bool {
    let mut reader = stdout;
    let mut splitter = AnnexBSplitter::default();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(size) => {
                for unit in splitter.push(&chunk[..size]) {
                    if tx.blocking_send(unit).is_err() {
                        return false;
                    }
                }
            }
            Err(_) => break,
        }
    }
    for unit in splitter.finish() {
        if tx.blocking_send(unit).is_err() {
            return false;
        }
    }
    true
}

fn collect_stderr(stderr: impl Read, tail: Arc<Mutex<String>>) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        eprintln!("[ffmpeg] {line}");
        if let Ok(mut guard) = tail.lock() {
            if !guard.is_empty() {
                guard.push('\n');
            }
            guard.push_str(&line);
            if guard.len() > 4000 {
                let keep_from = guard.len().saturating_sub(3000);
                *guard = guard[keep_from..].to_string();
            }
        }
    }
}

fn remember_fatal(control: &Control, message: String) {
    eprintln!("{message}");
    if let Ok(mut slot) = control.fatal.lock() {
        *slot = Some(message);
    }
}

fn ffmpeg_command(plan: &EncoderPlan, bitrate_bps: u64) -> Command {
    let bitrate = format!("{}k", bitrate_bps / 1000);
    let maxrate = format!("{}k", bitrate_bps.saturating_mul(5) / 4 / 1000);
    let mut command = Command::new("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .args(&plan.input_args)
        .arg("-vf")
        .arg(&plan.scale)
        .arg("-an")
        .arg("-c:v")
        .arg(plan.codec.ffmpeg_name());
    match plan.codec {
        CodecKind::Libx264 => {
            command
                .arg("-preset")
                .arg("veryfast")
                .arg("-tune")
                .arg("zerolatency")
                .arg("-profile:v")
                .arg("high")
                .arg("-g")
                .arg(&plan.gop)
                .arg("-keyint_min")
                .arg(&plan.gop)
                .arg("-sc_threshold")
                .arg("0")
                .arg("-bf")
                .arg("0")
                .arg("-b:v")
                .arg(&bitrate)
                .arg("-maxrate")
                .arg(&maxrate)
                .arg("-bufsize")
                .arg(&bitrate)
                .arg("-x264-params")
                .arg("aud=1:repeat-headers=1");
        }
        CodecKind::Nvenc => {
            command
                .arg("-preset")
                .arg("p4")
                .arg("-tune")
                .arg("ll")
                .arg("-rc")
                .arg("cbr")
                .arg("-profile:v")
                .arg("high")
                .arg("-g")
                .arg(&plan.gop)
                .arg("-bf")
                .arg("0")
                .arg("-b:v")
                .arg(&bitrate)
                .arg("-maxrate")
                .arg(&bitrate)
                .arg("-bufsize")
                .arg(&bitrate);
        }
        CodecKind::VideoToolbox => {
            command
                .arg("-realtime")
                .arg("1")
                .arg("-allow_sw")
                .arg("1")
                .arg("-profile:v")
                .arg("high")
                .arg("-g")
                .arg(&plan.gop)
                .arg("-bf")
                .arg("0")
                .arg("-b:v")
                .arg(&bitrate);
        }
    }
    command
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-r")
        .arg(&plan.fps)
        .arg("-fps_mode")
        .arg("cfr")
        .arg("-f")
        .arg("h264")
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn screen_input_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "avfoundation"
    } else if cfg!(target_os = "windows") {
        "gdigrab"
    } else {
        "unsupported"
    }
}

fn screen_input_args(config: &AppConfig) -> Result<Vec<String>> {
    if cfg!(target_os = "macos") {
        let index = macos_screen_index()?;
        let fps = config.target_fps.to_string();
        return Ok(vec![
            "-f".to_string(),
            "avfoundation".to_string(),
            "-pixel_format".to_string(),
            "nv12".to_string(),
            "-framerate".to_string(),
            fps,
            "-use_wallclock_as_timestamps".to_string(),
            "1".to_string(),
            "-capture_cursor".to_string(),
            "1".to_string(),
            "-i".to_string(),
            format!("{index}:none"),
        ]);
    }
    if cfg!(target_os = "windows") {
        let fps = config.target_fps.to_string();
        return Ok(vec![
            "-f".to_string(),
            "gdigrab".to_string(),
            "-framerate".to_string(),
            fps,
            "-draw_mouse".to_string(),
            "1".to_string(),
            "-i".to_string(),
            "desktop".to_string(),
        ]);
    }
    bail!("screen publish currently supports macOS and Windows");
}

fn macos_screen_index() -> Result<String> {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .output()
        .context("failed to list avfoundation devices")?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for line in text.lines() {
        if let Some(index) = capture_screen_index(line) {
            println!("avfoundation screen device: [{index}]");
            return Ok(index);
        }
    }
    bail!("ffmpeg did not list a Capture screen device. Grant Screen Recording permission to the terminal running native-sender.")
}

fn capture_screen_index(line: &str) -> Option<String> {
    if !line.to_ascii_lowercase().contains("capture screen") {
        return None;
    }
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        let Some(end) = after.find(']') else { break };
        let inside = &after[..end];
        if !inside.is_empty() && inside.chars().all(|ch| ch.is_ascii_digit()) {
            return Some(inside.to_string());
        }
        rest = &after[end + 1..];
    }
    None
}

fn ensure_ffmpeg() -> Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("ffmpeg is not installed or not on PATH")?;
    if !status.success() {
        bail!("ffmpeg -version failed");
    }
    Ok(())
}

fn ffmpeg_encoder_list() -> Result<String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .context("failed to query ffmpeg encoders")?;
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn encoder_available(list: &str, name: &str) -> bool {
    list.lines()
        .any(|line| line.split_whitespace().any(|token| token == name))
}

#[cfg(test)]
mod tests {
    use super::capture_screen_index;

    #[test]
    fn reads_the_numeric_device_index_not_the_log_prefix() {
        let line = "[AVFoundation indev @ 0x10] [2] Capture screen 0";
        assert_eq!(capture_screen_index(line).as_deref(), Some("2"));
        assert_eq!(capture_screen_index("[0] FaceTime HD Camera"), None);
    }
}
