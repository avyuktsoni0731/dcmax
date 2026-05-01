use crate::api::TokenResponse;
use anyhow::{Context, Result};
use futures_util::SinkExt;
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

