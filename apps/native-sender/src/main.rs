mod api;
mod config;
mod platform;
mod publisher;

use anyhow::Result;
use clap::Parser;
use config::{AppConfig, CliArgs};
use reqwest::Client;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = CliArgs::parse();
    let config = AppConfig::from_env(&args);
    let client = Client::new();

    let token = api::fetch_token(
        &client,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        &config.client_type,
    )
    .await?;

    let requested = args.platform.to_lowercase();

    #[cfg(target_os = "windows")]
    {
        let backend = platform::windows::WindowsCaptureBackend;
        if requested != "auto" && requested != "windows" {
            anyhow::bail!("requested platform '{}' does not match current OS windows", requested);
        }
        backend.bootstrap_capture_pipeline(args.dry_run)?;
        publisher::publish_bootstrap(backend.name(), &token, args.dry_run).await;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let backend = platform::macos::MacOsCaptureBackend;
        if requested != "auto" && requested != "macos" {
            anyhow::bail!("requested platform '{}' does not match current OS macos", requested);
        }
        backend.bootstrap_capture_pipeline(args.dry_run)?;
        publisher::publish_bootstrap(backend.name(), &token, args.dry_run).await;
        return Ok(());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = requested;
        anyhow::bail!("native-sender currently supports windows and macos only");
    }
}

