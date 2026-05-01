mod api;
mod capture;
mod config;
mod platform;
mod publisher;

use anyhow::Result;
use clap::Parser;
use capture::CaptureTuning;
use config::{AppConfig, CliArgs, TargetPlatform};
use platform::CaptureBackend;
use reqwest::Client;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = CliArgs::parse();
    let config = AppConfig::from_env(&args)?;
    let client = Client::new();

    println!(
        "native-sender starting: room='{}' identity='{}' platform='{:?}' dry_run={} target_fps={} probe_seconds={}",
        config.room_name,
        config.identity,
        config.platform,
        config.dry_run,
        config.target_fps,
        config.probe_seconds
    );
    let tuning = CaptureTuning {
        target_fps: config.target_fps,
        probe_seconds: config.probe_seconds,
    };

    api::health_check(&client, &config.api_base_url).await?;
    println!("api health check: ok");

    let token = api::fetch_token(
        &client,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        &config.client_type,
    )
    .await?;
    println!("token fetch: ok");

    #[cfg(target_os = "windows")]
    {
        let backend = platform::windows::WindowsCaptureBackend;
        if config.platform == TargetPlatform::MacOs {
            anyhow::bail!("requested macos backend on windows host");
        }
        println!("selected backend: {}", backend.name());
        println!("hint: {}", backend.diagnostics_hint());
        let report = backend.bootstrap_capture_pipeline(config.dry_run, tuning)?;
        if let Some(report) = report {
            api::report_native_session(
                &client,
                &config.api_base_url,
                &config.room_name,
                &config.identity,
                &report.backend,
                report.achieved_fps,
                report.produced_frames,
                report.dropped_frames,
                report.avg_ingest_latency_ms,
                report.avg_payload_bytes,
            )
            .await?;
            println!("native session report: posted");
        }
        publisher::publish_bootstrap(backend.name(), &token, config.dry_run).await;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let backend = platform::macos::MacOsCaptureBackend;
        if config.platform == TargetPlatform::Windows {
            anyhow::bail!("requested windows backend on macos host");
        }
        println!("selected backend: {}", backend.name());
        println!("hint: {}", backend.diagnostics_hint());
        let report = backend.bootstrap_capture_pipeline(config.dry_run, tuning)?;
        if let Some(report) = report {
            api::report_native_session(
                &client,
                &config.api_base_url,
                &config.room_name,
                &config.identity,
                &report.backend,
                report.achieved_fps,
                report.produced_frames,
                report.dropped_frames,
                report.avg_ingest_latency_ms,
                report.avg_payload_bytes,
            )
            .await?;
            println!("native session report: posted");
        }
        publisher::publish_bootstrap(backend.name(), &token, config.dry_run).await;
        return Ok(());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = config;
        anyhow::bail!("native-sender currently supports windows and macos only");
    }
}

