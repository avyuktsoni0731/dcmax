use crate::api::TokenResponse;
use anyhow::{Context, Result};
use crate::capture::{CaptureBackend, EncoderBackend};
use futures_util::SinkExt;
use std::process::{Child, Command, Stdio};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

#[derive(Debug, Clone, Copy)]
pub enum PublisherState {
    Starting,
    Running,
    Stopped,
    Error,
}

impl PublisherState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
        }
    }
}

pub async fn publish_bootstrap(platform_name: &str, token: &TokenResponse, dry_run: bool) -> Result<()> {
    println!(
        "publisher config: platform={} livekit_url={} token_size={}",
        platform_name,
        token.url,
        token.token.len()
    );

    if dry_run {
        println!(
            "[dry-run] platform={} livekit_url={} token_prefix={}...",
            platform_name,
            token.url,
            &token.token[..std::cmp::min(token.token.len(), 10)]
        );
        return Ok(());
    }

    let livekit_url =
        Url::parse(&token.url).context("invalid LiveKit URL in token response for publisher bootstrap")?;
    let host = livekit_url
        .host_str()
        .context("LiveKit URL host missing for publisher bootstrap")?;
    let port = livekit_url
        .port_or_known_default()
        .context("LiveKit URL port missing for publisher bootstrap")?;
    let tcp_target = format!("{}:{}", host, port);

    timeout(Duration::from_secs(4), TcpStream::connect(&tcp_target))
        .await
        .context("tcp connect timeout to LiveKit")?
        .context("tcp connect failed to LiveKit")?;

    let scheme = match livekit_url.scheme() {
        "wss" => "wss",
        "ws" => "ws",
        "https" => "wss",
        "http" => "ws",
        other => {
            anyhow::bail!("unsupported LiveKit URL scheme '{}' for publisher bootstrap", other);
        }
    };
    let authority = if let Some(configured_port) = livekit_url.port() {
        format!("{}:{}", livekit_url.host_str().unwrap_or_default(), configured_port)
    } else {
        livekit_url.host_str().unwrap_or_default().to_string()
    };
    let ws_probe_paths = ["/rtc/v1", "/rtc"];
    let mut ws_probe_ok = false;
    for probe_path in ws_probe_paths {
        let ws_url = Url::parse(&format!(
            "{}://{}{}?access_token={}",
            scheme, authority, probe_path, token.token
        ))
        .context("failed to build LiveKit signal URL for publisher bootstrap")?;

        match timeout(Duration::from_secs(6), connect_async(ws_url.as_str())).await {
            Ok(Ok((mut ws_stream, _response))) => {
                let _ = ws_stream.send(Message::Close(None)).await;
                ws_probe_ok = true;
                break;
            }
            Ok(Err(err)) => {
                eprintln!(
                    "publisher bootstrap warning: websocket probe failed on {}: {}",
                    probe_path, err
                );
            }
            Err(_) => {
                eprintln!(
                    "publisher bootstrap warning: websocket probe timeout on {}",
                    probe_path
                );
            }
        }
    }

    if !ws_probe_ok {
        eprintln!(
            "publisher bootstrap warning: all websocket probes failed; continuing because tcp reachability is healthy"
        );
    }

    println!(
        "publisher bootstrap ready for platform={} (LiveKit tcp check passed, ws_probe_ok={})",
        platform_name,
        ws_probe_ok
    );
    Ok(())
}

fn whip_base_url_from_livekit_url(raw: &str) -> Result<String> {
    let parsed = Url::parse(raw).context("invalid LiveKit URL for WHIP publisher")?;
    let scheme = match parsed.scheme() {
        "wss" | "https" => "https",
        "ws" | "http" => "http",
        other => anyhow::bail!("unsupported LiveKit URL scheme '{}' for WHIP publisher", other),
    };
    let authority = if let Some(port) = parsed.port() {
        format!("{}:{}", parsed.host_str().unwrap_or_default(), port)
    } else {
        parsed.host_str().unwrap_or_default().to_string()
    };
    Ok(format!("{}://{}/whip", scheme, authority))
}

pub fn start_ffmpeg_whip_publisher(
    platform_name: &str,
    token: &TokenResponse,
    target_fps: u32,
    capture_backend: CaptureBackend,
    encoder_backend: EncoderBackend,
) -> Result<Child> {
    if platform_name != "windows" {
        anyhow::bail!("ffmpeg WHIP publisher is currently wired for windows only");
    }

    let whip_base = whip_base_url_from_livekit_url(&token.url)?;
    let whip_url = format!("{}?access_token={}", whip_base, token.token);
    let capture_filter = match capture_backend {
        CaptureBackend::Auto | CaptureBackend::FfmpegDdagrab => format!("ddagrab=framerate={}", target_fps),
        CaptureBackend::Scrap => format!("ddagrab=framerate={}", target_fps),
    };
    let encoder_codec = match encoder_backend {
        EncoderBackend::Fast | EncoderBackend::FfmpegLibx264 => "libx264",
        EncoderBackend::FfmpegH264Nvenc => "h264_nvenc",
    };
    let mut command = Command::new("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg(capture_filter)
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:v")
        .arg(encoder_codec)
        .arg("-preset")
        .arg("veryfast")
        .arg("-tune")
        .arg("zerolatency")
        .arg("-g")
        .arg((target_fps * 2).to_string())
        .arg("-r")
        .arg(target_fps.to_string())
        .arg("-b:v")
        .arg("12M")
        .arg("-maxrate")
        .arg("16M")
        .arg("-bufsize")
        .arg("24M")
        .arg("-an")
        .arg("-f")
        .arg("whip")
        .arg(whip_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = command
        .spawn()
        .context("failed to spawn ffmpeg WHIP publisher (ensure ffmpeg is installed and in PATH)")?;
    Ok(child)
}

