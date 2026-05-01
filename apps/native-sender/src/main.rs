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
use tokio::time::{interval, Duration};

async fn post_native_report(client: &Client, config: &AppConfig, report: &capture::PipelineReport) -> Result<()> {
    api::report_native_session(
        client,
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
    .await
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = CliArgs::parse();
    let config = AppConfig::from_env(&args)?;
    let client = Client::new();

    println!(
        "native-sender starting: room='{}' identity='{}' platform='{:?}' dry_run={} target_fps={} probe_seconds={} heartbeat_seconds={} encoder={:?}",
        config.room_name,
        config.identity,
        config.platform,
        config.dry_run,
        config.target_fps,
        config.probe_seconds,
        config.heartbeat_seconds,
        config.encoder_backend
    );
    let tuning = CaptureTuning {
        target_fps: config.target_fps,
        probe_seconds: config.probe_seconds,
        encoder_backend: config.encoder_backend,
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
            post_native_report(&client, &config, &report).await?;
            println!("native session report: posted");
            if !config.dry_run {
                println!(
                    "native session heartbeat started (every {}s). Press Ctrl+C to stop.",
                    config.heartbeat_seconds
                );
                let mut ticker = interval(Duration::from_secs(config.heartbeat_seconds));
                let heartbeat_tuning = CaptureTuning {
                    target_fps: config.target_fps,
                    probe_seconds: 1,
                    encoder_backend: config.encoder_backend,
                };
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            println!("shutdown signal received, stopping native sender heartbeat");
                            break;
                        }
                        _ = ticker.tick() => {
                            match backend.bootstrap_capture_pipeline(false, heartbeat_tuning) {
                                Ok(Some(live_report)) => {
                                    if let Err(err) = post_native_report(&client, &config, &live_report).await {
                                        eprintln!("native session heartbeat post failed: {}", err);
                                    }
                                }
                                Ok(None) => {
                                    // no-op
                                }
                                Err(err) => {
                                    eprintln!("native session heartbeat sampling failed: {}", err);
                                }
                            }
                        }
                    }
                }
            }
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
            post_native_report(&client, &config, &report).await?;
            println!("native session report: posted");
            if !config.dry_run {
                println!(
                    "native session heartbeat started (every {}s). Press Ctrl+C to stop.",
                    config.heartbeat_seconds
                );
                let mut ticker = interval(Duration::from_secs(config.heartbeat_seconds));
                let heartbeat_tuning = CaptureTuning {
                    target_fps: config.target_fps,
                    probe_seconds: 1,
                    encoder_backend: config.encoder_backend,
                };
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            println!("shutdown signal received, stopping native sender heartbeat");
                            break;
                        }
                        _ = ticker.tick() => {
                            match backend.bootstrap_capture_pipeline(false, heartbeat_tuning) {
                                Ok(Some(live_report)) => {
                                    if let Err(err) = post_native_report(&client, &config, &live_report).await {
                                        eprintln!("native session heartbeat post failed: {}", err);
                                    }
                                }
                                Ok(None) => {
                                    // no-op
                                }
                                Err(err) => {
                                    eprintln!("native session heartbeat sampling failed: {}", err);
                                }
                            }
                        }
                    }
                }
            }
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

