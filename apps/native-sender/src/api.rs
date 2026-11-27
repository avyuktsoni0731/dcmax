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
    #[serde(skip_serializing_if = "Option::is_none")]
    invite: Option<&'a str>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TokenResponse {
    pub token: String,
    pub url: String,
    #[serde(default)]
    pub relay: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
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
        anyhow::bail!(
            "health request returned non-success status {}",
            res.status()
        );
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
    invite: Option<&str>,
    session: Option<&str>,
) -> Result<TokenResponse> {
    let endpoint = format!("{}/token", api_base_url.trim_end_matches('/'));
    let req = TokenRequest {
        room_name,
        identity,
        client_type,
        invite,
    };

    let res = authed(client.post(endpoint), session)
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

pub fn fetch_token_blocking(
    api_base_url: &str,
    room_name: &str,
    identity: &str,
    client_type: &str,
    invite: Option<&str>,
    session: Option<&str>,
) -> Result<TokenResponse> {
    let endpoint = format!("{}/token", api_base_url.trim_end_matches('/'));
    let req = TokenRequest {
        room_name,
        identity,
        client_type,
        invite,
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("token client")?;
    let res = authed_blocking(client.post(endpoint), session)
        .json(&req)
        .send()
        .context("token request failed")?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().unwrap_or_default();
        anyhow::bail!("token request returned {}: {}", status, body);
    }
    res.json::<TokenResponse>()
        .context("token response parse failed")
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
    let endpoint = format!(
        "{}/native/publisher/events",
        api_base_url.trim_end_matches('/')
    );
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

fn authed(builder: reqwest::RequestBuilder, session: Option<&str>) -> reqwest::RequestBuilder {
    match session.map(str::trim).filter(|value| !value.is_empty()) {
        Some(session) => builder.header("authorization", format!("Bearer {session}")),
        None => builder,
    }
}

fn authed_blocking(
    builder: reqwest::blocking::RequestBuilder,
    session: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    match session.map(str::trim).filter(|value| !value.is_empty()) {
        Some(session) => builder.header("authorization", format!("Bearer {session}")),
        None => builder,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountSession {
    pub username: String,
    pub email: String,
    pub session: String,
}

/// Register, sign in, resume, or end an account. The password is sent once
/// over HTTPS and is not kept here.
pub fn account_blocking(body: &serde_json::Value) -> Result<AccountSession> {
    let endpoint = match body.get("action").and_then(|value| value.as_str()) {
        Some("register") => "register",
        Some("login") => "login",
        Some("session") => "session",
        Some("logout") => "logout",
        Some("here") => "here",
        _ => anyhow::bail!("unknown account action"),
    };
    let api_base_url = std::env::var("API_BASE_URL")
        .unwrap_or_else(|_| "https://dcmax-token.soniavyukt.workers.dev".to_string());
    let url = format!("{}/{}", api_base_url.trim_end_matches('/'), endpoint);
    let session = body.get("session").and_then(|value| value.as_str());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("account client")?;
    let response = authed_blocking(client.post(url), session)
        .json(body)
        .send()
        .context("Could not reach the account service.")?;
    let status = response.status();
    let text = response.text().context("account response")?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|item| item.as_str())
                    .map(str::to_string)
            })
            .filter(|item| !item.trim().is_empty())
            .unwrap_or_else(|| "The account service did not accept that.".to_string());
        anyhow::bail!(message);
    }
    serde_json::from_str(&text).context("account response parse failed")
}

/// Usernames of people who are signed in right now and match the start of `query`.
pub fn people_blocking(session: &str, query: &str) -> Result<Vec<String>> {
    let api_base_url = std::env::var("API_BASE_URL")
        .unwrap_or_else(|_| "https://dcmax-token.soniavyukt.workers.dev".to_string());
    let url = format!("{}/people", api_base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("account client")?;
    let response = authed_blocking(client.post(url), Some(session))
        .json(&serde_json::json!({ "query": query, "session": session }))
        .send()
        .context("Could not reach the account service.")?;
    let status = response.status();
    let text = response.text().context("account response")?;
    if !status.is_success() {
        anyhow::bail!("The account service did not accept that.");
    }
    let value: serde_json::Value = serde_json::from_str(&text).context("account response parse failed")?;
    Ok(value
        .get("people")
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}
