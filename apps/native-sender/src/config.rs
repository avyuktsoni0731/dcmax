use anyhow::{bail, Result};
use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "native-sender")]
#[command(about = "Publish a desktop capture into a LiveKit room")]
pub struct CliArgs {
    #[arg(long)]
    pub room: Option<String>,
    #[arg(long)]
    pub identity: Option<String>,
    #[arg(long, default_value = "auto")]
    pub platform: String,
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    #[arg(long, default_value_t = 60)]
    pub target_fps: u32,
    /// Accepted so existing API launches keep working. The capture probe is gone.
    #[arg(long, default_value_t = 5)]
    pub probe_seconds: u64,
    #[arg(long, default_value_t = 3)]
    pub heartbeat_seconds: u64,
    #[arg(long, default_value = "fast")]
    pub encoder: String,
    /// Accepted so existing API launches keep working. Capture is the OS screen input.
    #[arg(long, default_value = "scrap")]
    pub capture: String,
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
    #[arg(long, default_value_t = 8000)]
    pub bitrate_kbps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPlatform {
    Auto,
    Windows,
    MacOs,
}

impl TargetPlatform {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "windows" => Ok(Self::Windows),
            "macos" => Ok(Self::MacOs),
            _ => bail!("invalid --platform value '{raw}'; expected auto|windows|macos"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    Fast,
    FfmpegLibx264,
    FfmpegH264Nvenc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    Auto,
    Scrap,
    FfmpegDdagrab,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub api_base_url: String,
    pub room_name: String,
    pub identity: String,
    pub client_type: String,
    pub platform: TargetPlatform,
    pub dry_run: bool,
    pub target_fps: u32,
    pub probe_seconds: u64,
    pub heartbeat_seconds: u64,
    pub encoder_backend: EncoderBackend,
    pub capture_backend: CaptureBackend,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
}

impl AppConfig {
    pub fn from_env(args: &CliArgs) -> Result<Self> {
        let api_base_url =
            std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://localhost:4000".to_string());
        let room_name = args
            .room
            .clone()
            .or_else(|| std::env::var("ROOM_NAME").ok())
            .unwrap_or_else(|| "mycord-room".to_string());
        let identity = args
            .identity
            .clone()
            .or_else(|| std::env::var("IDENTITY").ok())
            .unwrap_or_else(|| "native-sender".to_string());
        let client_type =
            std::env::var("CLIENT_TYPE").unwrap_or_else(|_| "native_sender".to_string());
        let platform = TargetPlatform::parse(&args.platform)?;

        if !api_base_url.starts_with("http://") && !api_base_url.starts_with("https://") {
            bail!("API_BASE_URL must start with http:// or https://");
        }
        if room_name.trim().len() < 2 {
            bail!("room name must be at least 2 characters");
        }
        if identity.trim().len() < 2 {
            bail!("identity must be at least 2 characters");
        }
        if client_type != "native_sender" {
            bail!("CLIENT_TYPE must be native_sender for native capture publisher");
        }
        if args.target_fps < 24 || args.target_fps > 240 {
            bail!("--target-fps must be between 24 and 240");
        }
        if args.probe_seconds == 0 || args.probe_seconds > 60 {
            bail!("--probe-seconds must be between 1 and 60");
        }
        if args.heartbeat_seconds == 0 || args.heartbeat_seconds > 60 {
            bail!("--heartbeat-seconds must be between 1 and 60");
        }
        if args.width < 320 || args.height < 180 || args.width > 3840 || args.height > 2160 {
            bail!("--width/--height must stay within 320x180 and 3840x2160");
        }
        if args.width % 2 != 0 || args.height % 2 != 0 {
            bail!("--width and --height must be even");
        }
        if args.bitrate_kbps < 800 || args.bitrate_kbps > 20_000 {
            bail!("--bitrate-kbps must be between 800 and 20000");
        }
        let encoder_backend = match args.encoder.to_ascii_lowercase().as_str() {
            "fast" => EncoderBackend::Fast,
            "ffmpeg-libx264" => EncoderBackend::FfmpegLibx264,
            "ffmpeg-h264-nvenc" => EncoderBackend::FfmpegH264Nvenc,
            other => bail!(
                "invalid --encoder value '{other}'; expected fast|ffmpeg-libx264|ffmpeg-h264-nvenc"
            ),
        };
        let capture_backend = match args.capture.to_ascii_lowercase().as_str() {
            "auto" => CaptureBackend::Auto,
            "scrap" => CaptureBackend::Scrap,
            "ffmpeg-ddagrab" => CaptureBackend::FfmpegDdagrab,
            other => bail!("invalid --capture value '{other}'; expected auto|scrap|ffmpeg-ddagrab"),
        };

        Ok(Self {
            api_base_url,
            room_name,
            identity,
            client_type,
            platform,
            dry_run: args.dry_run,
            target_fps: args.target_fps,
            probe_seconds: args.probe_seconds,
            heartbeat_seconds: args.heartbeat_seconds,
            encoder_backend,
            capture_backend,
            width: args.width,
            height: args.height,
            bitrate_kbps: args.bitrate_kbps,
        })
    }

    pub fn gop_frames(&self) -> u32 {
        self.target_fps.saturating_mul(2).max(self.target_fps)
    }
}
