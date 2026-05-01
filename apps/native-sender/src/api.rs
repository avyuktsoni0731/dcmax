use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct TokenRequest<'a> {
    #[serde(rename = "roomName")]
    room_name: &'a str,
    identity: &'a str,
    #[serde(rename = "clientType")]
    client_type: &'a str,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TokenResponse {
    pub token: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
struct NativeSessionRequest<'a> {
    #[serde(rename = "roomName")]
    room_name: &'a str,
    identity: &'a str,
    backend: &'a str,
    #[serde(rename = "achievedFps")]
    achieved_fps: f64,
    #[serde(rename = "producedFrames")]
    produced_frames: u64,
    #[serde(rename = "droppedFrames")]
    dropped_frames: u64,
    #[serde(rename = "avgIngestLatencyMs")]
    avg_ingest_latency_ms: f64,
    #[serde(rename = "avgPayloadBytes")]
    avg_payload_bytes: usize,
}

#[derive(Debug, Serialize)]
struct NativePublisherEventRequest<'a> {
    #[serde(rename = "roomName")]
    room_name: &'a str,
    identity: &'a str,
    state: &'a str,
    backend: &'a str,
    #[serde(rename = "captureBackend")]
    capture_backend: &'a str,
    #[serde(rename = "encoderBackend")]
    encoder_backend: &'a str,
    message: Option<&'a str>,
}

pub async fn health_check(client: &Client, api_base_url: &str) -> Result<()> {
    let endpoint = format!("{}/health", api_base_url.trim_end_matches('/'));
    let res = client
        .get(endpoint)
        .send()
        .await
        .context("health request failed")?;

    if !res.status().is_success() {
        anyhow::bail!("health request returned non-success status {}", res.status());
    }

    let payload = res
        .json::<HealthResponse>()
        .await
        .context("health response parse failed")?;
    if !payload.ok {
        anyhow::bail!("health endpoint responded with ok=false");
    }
    Ok(())
}

pub async fn fetch_token(
    client: &Client,
    api_base_url: &str,
    room_name: &str,
    identity: &str,
    client_type: &str,
) -> Result<TokenResponse> {
    let endpoint = format!("{}/token", api_base_url.trim_end_matches('/'));
    let req = TokenRequest {
        room_name,
        identity,
        client_type,
    };

    let res = client
        .post(endpoint)
        .json(&req)
        .send()
        .await
        .context("token request failed")?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("token request returned {}: {}", status, body);
    }

    let payload = res
        .json::<TokenResponse>()
        .await
        .context("token response parse failed")?;

    Ok(payload)
}

pub async fn report_native_session(
    client: &Client,
    api_base_url: &str,
    room_name: &str,
    identity: &str,
    backend: &str,
    achieved_fps: f64,
    produced_frames: u64,
    dropped_frames: u64,
    avg_ingest_latency_ms: f64,
    avg_payload_bytes: usize,
) -> Result<()> {
    let endpoint = format!("{}/native/sessions", api_base_url.trim_end_matches('/'));
    let req = NativeSessionRequest {
        room_name,
        identity,
        backend,
        achieved_fps,
        produced_frames,
        dropped_frames,
        avg_ingest_latency_ms,
        avg_payload_bytes,
    };

    let res = client
        .post(endpoint)
        .json(&req)
        .send()
        .await
        .context("native session report request failed")?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("native session report returned {}: {}", status, body);
    }
    Ok(())
}

pub async fn report_native_publisher_event(
    client: &Client,
    api_base_url: &str,
    room_name: &str,
    identity: &str,
    state: &str,
    backend: &str,
    capture_backend: &str,
    encoder_backend: &str,
    message: Option<&str>,
) -> Result<()> {
    let endpoint = format!("{}/native/publisher/events", api_base_url.trim_end_matches('/'));
    let req = NativePublisherEventRequest {
        room_name,
        identity,
        state,
        backend,
        capture_backend,
        encoder_backend,
        message,
    };

    let res = client
        .post(endpoint)
        .json(&req)
        .send()
        .await
        .context("native publisher event request failed")?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("native publisher event returned {}: {}", status, body);
    }
    Ok(())
}

