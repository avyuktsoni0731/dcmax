use crate::config::{AppConfig, EncoderBackend};
use crate::h264::{AccessUnit, AnnexBSplitter};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
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

pub struct ScreenEncoder {
    child: Child,
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

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn fatal_error(&mut self) -> Result<Option<String>> {
        match self.try_wait()? {
            Some(status) => Ok(Some(format!(
                "ffmpeg exited: {status} {}",
                self.stderr_tail()
            ))),
            None => Ok(None),
        }
    }

    /// The FFmpeg process cannot emit an IDR on demand. Windows still uses
    /// this stand-in until Graphics Capture and NVENC replace it.
    pub fn request_keyframe(&self) {}

    pub fn set_bitrate_bps(&self, _bps: u64) {}

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

    let gop = config.gop_frames().to_string();
    let fps = config.target_fps.to_string();
    let bitrate = format!("{}k", config.bitrate_kbps);
    let maxrate = format!("{}k", config.bitrate_kbps.saturating_mul(5) / 4);
    let bufsize = bitrate.clone();
    let scale = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,fps={},format=yuv420p",
        config.width, config.height, config.width, config.height, config.target_fps
    );

    let mut command = Command::new("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .args(screen_input_args(config)?)
        .arg("-vf")
        .arg(scale)
        .arg("-an")
        .arg("-c:v")
        .arg(codec.ffmpeg_name());

    match codec {
        CodecKind::Libx264 => {
            command
                .arg("-preset")
                .arg("veryfast")
                .arg("-tune")
                .arg("zerolatency")
                .arg("-profile:v")
                .arg("high")
                .arg("-g")
                .arg(&gop)
                .arg("-keyint_min")
                .arg(&gop)
                .arg("-sc_threshold")
                .arg("0")
                .arg("-bf")
                .arg("0")
                .arg("-b:v")
                .arg(&bitrate)
                .arg("-maxrate")
                .arg(&maxrate)
                .arg("-bufsize")
                .arg(&bufsize)
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
                .arg(&gop)
                .arg("-bf")
                .arg("0")
                .arg("-b:v")
                .arg(&bitrate)
                .arg("-maxrate")
                .arg(&bitrate)
                .arg("-bufsize")
                .arg(&bufsize);
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
                .arg(&gop)
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
        .arg(&fps)
        .arg("-fps_mode")
        .arg("cfr")
        .arg("-f")
        .arg("h264")
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().context(
        "failed to start ffmpeg. Install ffmpeg and grant Screen Recording permission on macOS.",
    )?;
    let stdout = child.stdout.take().context("ffmpeg stdout was not piped")?;
    let stderr = child.stderr.take().context("ffmpeg stderr was not piped")?;
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let tail = Arc::clone(&stderr_tail);
    thread::spawn(move || {
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
    });

    let (tx, rx) = mpsc::channel(2);
    thread::spawn(move || {
        let mut reader = stdout;
        let mut splitter = AnnexBSplitter::default();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    for unit in splitter.push(&chunk[..size]) {
                        if tx.blocking_send(unit).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        for unit in splitter.finish() {
            if tx.blocking_send(unit).is_err() {
                return;
            }
        }
    });

    Ok(ScreenEncoder {
        child,
        units: rx,
        stderr_tail,
        capture_backend: capture_backend.to_string(),
        encoder_backend: codec.ffmpeg_name().to_string(),
        width: config.width,
        height: config.height,
    })
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
