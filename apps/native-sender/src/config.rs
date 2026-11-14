use anyhow::{bail, Context, Result};
use clap::Parser;
use std::net::SocketAddr;

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
    /// Encoder target in kilobits per second. 800 is the floor and 100000 is the ceiling.
    #[arg(long, default_value_t = 8000)]
    pub bitrate_kbps: u32,
    /// Join the room and show the shared screen. Does not capture.
    #[arg(long, default_value_t = false)]
    pub watch: bool,
    /// Also send the picture to this host:port. LiveKit publish stays on.
    #[arg(long, value_name = "HOST:PORT")]
    pub picture_to: Option<String>,
    /// Show the picture from this UDP address instead of the LiveKit video.
    #[arg(long, value_name = "HOST:PORT")]
    pub picture_listen: Option<String>,
    /// Run the picture relay. Friends dial this address; nothing listens on their machines.
    #[arg(long, default_value_t = false)]
    pub relay: bool,
    /// Address the relay binds. Used with --relay.
    #[arg(long, default_value = "0.0.0.0:46000")]
    pub relay_listen: String,
    /// Send or watch the picture through the relay at this host:port.
    #[arg(long, value_name = "HOST:PORT")]
    pub picture_relay: Option<String>,
    /// Invite code from POST /invites. Required once the API has INVITE_REQUIRED=true.
    #[arg(long)]
    pub invite: Option<String>,
    /// Send the picture straight to the other app. LiveKit is only the room and voice.
    #[arg(long, default_value_t = false)]
    pub picture_direct: bool,
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
    pub watch: bool,
    pub picture_to: Option<SocketAddr>,
    pub picture_listen: Option<SocketAddr>,
    pub picture_relay: Option<SocketAddr>,
    pub invite: Option<String>,
    pub picture_direct: bool,
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
        let identity = if args.watch {
            args.identity
                .clone()
                .unwrap_or_else(|| "native-viewer".to_string())
        } else {
            args.identity
                .clone()
                .or_else(|| std::env::var("IDENTITY").ok())
                .unwrap_or_else(|| "native-sender".to_string())
        };
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
        if args.bitrate_kbps < 800 || args.bitrate_kbps > 100_000 {
            bail!("--bitrate-kbps must be between 800 and 100000");
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
        if args.picture_to.is_some() && args.picture_relay.is_some() {
            bail!("use either --picture-to or --picture-relay");
        }
        if args.picture_relay.is_some()
            && args
                .invite
                .as_ref()
                .map(|invite| invite.trim().is_empty())
                .unwrap_or(true)
        {
            bail!("--picture-relay needs --invite");
        }
        if args.picture_direct
            && (args.picture_to.is_some()
                || args.picture_relay.is_some()
                || args.picture_listen.is_some())
        {
            bail!("--picture-direct replaces --picture-to, --picture-listen, and --picture-relay");
        }

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
            watch: args.watch,
            picture_to: parse_addr(args.picture_to.as_deref(), "--picture-to")?,
            picture_listen: parse_addr(args.picture_listen.as_deref(), "--picture-listen")?,
            picture_relay: parse_addr(args.picture_relay.as_deref(), "--picture-relay")?,
            invite: args.invite.clone().filter(|invite| !invite.trim().is_empty()),
            picture_direct: args.picture_direct,
        })
    }

    pub fn gop_frames(&self) -> u32 {
        // One keyframe a second. A missed picture frame heals on the next one
        // instead of smearing every later frame for two seconds.
        self.target_fps.max(1)
    }
}

fn parse_addr(raw: Option<&str>, flag: &str) -> Result<Option<SocketAddr>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let addr = raw
        .parse::<SocketAddr>()
        .with_context(|| format!("{flag} must be host:port, for example 100.72.237.115:46000"))?;
    Ok(Some(addr))
}
