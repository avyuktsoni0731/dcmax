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
        let body = res.text().await.unwrap_or_default();
        anyhow::bail!("token request returned {}: {}", res.status(), body);
    }

    let payload = res
        .json::<TokenResponse>()
        .await
        .context("token response parse failed")?;

    Ok(payload)
}

