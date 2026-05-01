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
use publisher::PublisherState;
use reqwest::Client;
use std::io::Read;
use std::process::Child;
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

async fn post_publisher_event(
    client: &Client,
    config: &AppConfig,
    state: PublisherState,
    backend: &str,
    message: Option<&str>,
) -> Result<()> {
    let capture_backend = format!("{:?}", config.capture_backend).to_ascii_lowercase();
    let encoder_backend = format!("{:?}", config.encoder_backend).to_ascii_lowercase();
    api::report_native_publisher_event(
        client,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        state.as_str(),
        backend,
        &capture_backend,
        &encoder_backend,
        message,
    )
    .await
}

fn stop_publisher_child(child: &mut Option<Child>) {
    if let Some(proc) = child.as_mut() {
        let _ = proc.kill();
        let _ = proc.wait();
    }
    *child = None;
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = CliArgs::parse();
    let config = AppConfig::from_env(&args)?;
    let client = Client::new();

    println!(
        "native-sender starting: room='{}' identity='{}' platform='{:?}' dry_run={} target_fps={} probe_seconds={} heartbeat_seconds={} capture={:?} encoder={:?}",
        config.room_name,
        config.identity,
        config.platform,
        config.dry_run,
        config.target_fps,
        config.probe_seconds,
        config.heartbeat_seconds,
        config.capture_backend,
        config.encoder_backend
    );
    let tuning = CaptureTuning {
        target_fps: config.target_fps,
        probe_seconds: config.probe_seconds,
        encoder_backend: config.encoder_backend,
        capture_backend: config.capture_backend,
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
        let _ = post_publisher_event(&client, &config, PublisherState::Starting, backend.name(), None).await;
        if let Err(err) = publisher::publish_bootstrap(backend.name(), &token, config.dry_run).await {
            let _ = post_publisher_event(
                &client,
                &config,
                PublisherState::Error,
                backend.name(),
                Some("publisher bootstrap failed"),
            )
            .await;
            anyhow::bail!("publisher bootstrap failed: {}", err);
        }
        let mut ffmpeg_publisher: Option<Child> = if config.dry_run {
            None
        } else {
            match publisher::start_ffmpeg_whip_publisher(
                backend.name(),
                &token,
                config.target_fps,
                config.capture_backend,
                config.encoder_backend,
            ) {
                Ok(child) => {
                    println!("ffmpeg WHIP publisher process started");
                    Some(child)
                }
                Err(err) => {
                    let _ = post_publisher_event(
                        &client,
                        &config,
                        PublisherState::Error,
                        backend.name(),
                        Some("ffmpeg WHIP publisher failed to start"),
                    )
                    .await;
                    anyhow::bail!("ffmpeg WHIP publisher failed to start: {}", err);
                }
            }
        };
        let report = backend.bootstrap_capture_pipeline(config.dry_run, tuning)?;
        if let Some(report) = report {
            let last_report = report;
            post_native_report(&client, &config, &last_report).await?;
            println!("native session report: posted");
            let _ = post_publisher_event(&client, &config, PublisherState::Running, backend.name(), None).await;
            if !config.dry_run {
                println!(
                    "native session heartbeat started (every {}s). Press Ctrl+C to stop.",
                    config.heartbeat_seconds
                );
                let mut ticker = interval(Duration::from_secs(config.heartbeat_seconds));
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            println!("shutdown signal received, stopping native sender heartbeat");
                            stop_publisher_child(&mut ffmpeg_publisher);
                            let _ = post_publisher_event(
                                &client,
                                &config,
                                PublisherState::Stopped,
                                backend.name(),
                                Some("shutdown signal received"),
                            )
                            .await;
                            break;
                        }
                        _ = ticker.tick() => {
                            if let Some(proc) = ffmpeg_publisher.as_mut() {
                                match proc.try_wait() {
                                    Ok(Some(status)) => {
                                        let mut msg = format!("ffmpeg WHIP publisher exited: {}", status);
                                        if let Some(stderr) = proc.stderr.as_mut() {
                                            let mut buf = String::new();
                                            let _ = stderr.read_to_string(&mut buf);
                                            if !buf.trim().is_empty() {
                                                msg = format!("{} :: {}", msg, buf.trim());
                                                if buf.contains("Invalid answer: OK") {
                                                    msg = format!(
                                                        "{} :: hint: LiveKit signaling URL is not a WHIP ingest endpoint. Set LIVEKIT_WHIP_URL (and optionally LIVEKIT_WHIP_BEARER_TOKEN) for a valid ingest target.",
                                                        msg
                                                    );
                                                }
                                            }
                                        }
                                        let _ = post_publisher_event(
                                            &client,
                                            &config,
                                            PublisherState::Error,
                                            backend.name(),
                                            Some("ffmpeg WHIP publisher exited"),
                                        )
                                        .await;
                                        anyhow::bail!("{}", msg);
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        eprintln!("ffmpeg WHIP publisher status check failed: {}", err);
                                    }
                                }
                            }

                            if let Err(err) = post_native_report(&client, &config, &last_report).await {
                                eprintln!("native session heartbeat post failed: {}", err);
                                let _ = post_publisher_event(
                                    &client,
                                    &config,
                                    PublisherState::Error,
                                    backend.name(),
                                    Some("native session heartbeat post failed"),
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }
        stop_publisher_child(&mut ffmpeg_publisher);
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
        let _ = post_publisher_event(&client, &config, PublisherState::Starting, backend.name(), None).await;
        if let Err(err) = publisher::publish_bootstrap(backend.name(), &token, config.dry_run).await {
            let _ = post_publisher_event(
                &client,
                &config,
                PublisherState::Error,
                backend.name(),
                Some("publisher bootstrap failed"),
            )
            .await;
            anyhow::bail!("publisher bootstrap failed: {}", err);
        }
        let report = backend.bootstrap_capture_pipeline(config.dry_run, tuning)?;
        if let Some(report) = report {
            post_native_report(&client, &config, &report).await?;
            println!("native session report: posted");
            let _ = post_publisher_event(&client, &config, PublisherState::Running, backend.name(), None).await;
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
                    capture_backend: config.capture_backend,
                };
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            println!("shutdown signal received, stopping native sender heartbeat");
                            let _ = post_publisher_event(
                                &client,
                                &config,
                                PublisherState::Stopped,
                                backend.name(),
                                Some("shutdown signal received"),
                            )
                            .await;
                            break;
                        }
                        _ = ticker.tick() => {
                            match backend.bootstrap_capture_pipeline(false, heartbeat_tuning) {
                                Ok(Some(live_report)) => {
                                    if let Err(err) = post_native_report(&client, &config, &live_report).await {
                                        eprintln!("native session heartbeat post failed: {}", err);
                                        let _ = post_publisher_event(
                                            &client,
                                            &config,
                                            PublisherState::Error,
                                            backend.name(),
                                            Some("native session heartbeat post failed"),
                                        )
                                        .await;
                                    }
                                }
                                Ok(None) => {
                                    // no-op
                                }
                                Err(err) => {
                                    eprintln!("native session heartbeat sampling failed: {}", err);
                                    let _ = post_publisher_event(
                                        &client,
                                        &config,
                                        PublisherState::Error,
                                        backend.name(),
                                        Some("native session heartbeat sampling failed"),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = config;
        anyhow::bail!("native-sender currently supports windows and macos only");
    }
}

