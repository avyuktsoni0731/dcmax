mod api;
mod config;
mod encoder;
mod h264;
#[cfg(target_os = "macos")]
mod present_macos;
mod publish;
mod watch;

use anyhow::Result;
use clap::Parser;
use config::{AppConfig, CliArgs, TargetPlatform};
use reqwest::Client;

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = CliArgs::parse();
    let config = AppConfig::from_env(&args)?;
    if config.watch {
        return watch::run(&config);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_sender(config))
}

async fn run_sender(config: AppConfig) -> Result<()> {
    ensure_host_platform(&config)?;

    let client = Client::new();
    println!(
        "native-sender starting: room='{}' identity='{}' platform='{:?}' dry_run={} {}x{} @ {}fps bitrate={}kbps encoder={:?} capture_flag={:?}",
        config.room_name,
        config.identity,
        config.platform,
        config.dry_run,
        config.width,
        config.height,
        config.target_fps,
        config.bitrate_kbps,
        config.encoder_backend,
        config.capture_backend
    );
    if config.probe_seconds > 0 {
        println!("capture probe window is not used; video publishes until Ctrl+C");
    }

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
    println!("token fetch: ok ({})", token.url);
    post_state(
        &client, &config, "starting", "screen", "pending", "pending", None,
    )
    .await;

    if config.dry_run {
        println!("dry-run: token ok, skipping capture and publish");
        tokio::signal::ctrl_c().await?;
        post_state(
            &client,
            &config,
            "stopped",
            "screen",
            "pending",
            "pending",
            Some("dry-run shutdown"),
        )
        .await;
        return Ok(());
    }

    let mut screen = match encoder::ScreenEncoder::start(&config) {
        Ok(screen) => screen,
        Err(err) => {
            post_state(
                &client,
                &config,
                "error",
                "screen",
                "pending",
                "pending",
                Some("encoder failed to start"),
            )
            .await;
            return Err(err);
        }
    };

    let publish_result =
        publish::run(&client, &config, &token.token, &token.url, &mut screen).await;
    let tail = screen.stderr_tail();
    let label = screen.label();
    let capture_backend = screen.capture_backend.clone();
    let encoder_backend = screen.encoder_backend.clone();
    screen.stop();

    match publish_result {
        Ok(()) => {
            post_state(
                &client,
                &config,
                "stopped",
                &label,
                &capture_backend,
                &encoder_backend,
                Some("shutdown"),
            )
            .await;
            Ok(())
        }
        Err(err) => {
            let message = if tail.trim().is_empty() {
                "publish failed".to_string()
            } else {
                format!("publish failed: {}", tail.trim())
            };
            post_state(
                &client,
                &config,
                "error",
                &label,
                &capture_backend,
                &encoder_backend,
                Some(&message),
            )
            .await;
            Err(err)
        }
    }
}

fn ensure_host_platform(config: &AppConfig) -> Result<()> {
    match config.platform {
        TargetPlatform::Windows if !cfg!(target_os = "windows") => {
            anyhow::bail!("requested windows backend on this host");
        }
        TargetPlatform::MacOs if !cfg!(target_os = "macos") => {
            anyhow::bail!("requested macos backend on this host");
        }
        TargetPlatform::Auto | TargetPlatform::Windows | TargetPlatform::MacOs => {
            if !cfg!(any(target_os = "windows", target_os = "macos")) {
                anyhow::bail!("native-sender currently supports windows and macos only");
            }
            Ok(())
        }
    }
}

async fn post_state(
    client: &Client,
    config: &AppConfig,
    state: &str,
    backend: &str,
    capture_backend: &str,
    encoder_backend: &str,
    message: Option<&str>,
) {
    let clipped;
    let message = match message {
        Some(text) => {
            clipped = clip_message(text);
            Some(clipped.as_str())
        }
        None => None,
    };
    if let Err(err) = api::report_native_publisher_event(
        client,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        state,
        backend,
        capture_backend,
        encoder_backend,
        message,
    )
    .await
    {
        eprintln!("publisher state '{state}' was not reported: {err}");
    }
}

fn clip_message(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= 240 {
        return trimmed.to_string();
    }
    let mut end = 240;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_string()
}
