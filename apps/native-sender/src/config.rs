use anyhow::{bail, Result};
use clap::Parser;
use crate::capture::{CaptureBackend, EncoderBackend};

#[derive(Debug, Clone, Parser)]
#[command(name = "native-sender")]
#[command(about = "Native screen/audio sender bootstrap")]
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
    #[arg(long, default_value_t = 5)]
    pub probe_seconds: u64,
    #[arg(long, default_value_t = 3)]
    pub heartbeat_seconds: u64,
    #[arg(long, default_value = "fast")]
    pub encoder: String,
    #[arg(long, default_value = "scrap")]
    pub capture: String,
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
            _ => bail!("invalid --platform value '{}'; expected auto|windows|macos", raw),
        }
    }
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
        let encoder_backend = match args.encoder.to_ascii_lowercase().as_str() {
            "fast" => EncoderBackend::Fast,
            "ffmpeg-libx264" => EncoderBackend::FfmpegLibx264,
            "ffmpeg-h264-nvenc" => EncoderBackend::FfmpegH264Nvenc,
            other => bail!(
                "invalid --encoder value '{}'; expected fast|ffmpeg-libx264|ffmpeg-h264-nvenc",
                other
            ),
        };
        let capture_backend = match args.capture.to_ascii_lowercase().as_str() {
            "auto" => CaptureBackend::Auto,
            "scrap" => CaptureBackend::Scrap,
            "ffmpeg-ddagrab" => CaptureBackend::FfmpegDdagrab,
            other => bail!(
                "invalid --capture value '{}'; expected auto|scrap|ffmpeg-ddagrab",
                other
            ),
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
        })
    }
}

