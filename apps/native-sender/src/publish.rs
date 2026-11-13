use crate::api;
use crate::config::AppConfig;
use crate::encoder::ScreenEncoder;
use anyhow::{Context, Result};
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoderBackend, VideoEncoding};
use livekit::prelude::*;
use livekit::webrtc::video_frame::{EncodedFrameType, EncodedVideoCodec, EncodedVideoFrame};
use livekit::webrtc::video_source::{native::NativeVideoSource, RtcVideoSource, VideoResolution};
use reqwest::Client;
use std::time::Instant;
use tokio::time::{interval, Duration};

pub async fn run(
    http: &Client,
    config: &AppConfig,
    token: &str,
    livekit_url: &str,
    encoder: &mut ScreenEncoder,
) -> Result<()> {
    let mut room_options = RoomOptions::default();
    room_options.auto_subscribe = false;
    let (room, mut events) = Room::connect(livekit_url, token, room_options)
        .await
        .context("LiveKit room connect failed")?;

    let (disconnect_tx, mut disconnect_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let RoomEvent::Disconnected { reason } = event {
                eprintln!("livekit disconnected: {reason:?}");
                let _ = disconnect_tx.send(true);
                break;
            }
        }
    });

    let resolution = VideoResolution {
        width: config.width,
        height: config.height,
    };
    let source = NativeVideoSource::new_encoded(resolution.clone());
    let track =
        LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(source.clone()));
    let options = TrackPublishOptions {
        video_codec: VideoCodec::H264,
        video_encoder: VideoEncoderBackend::PreEncoded,
        simulcast: false,
        source: TrackSource::Screenshare,
        video_encoding: Some(VideoEncoding {
            max_bitrate: u64::from(config.bitrate_kbps) * 1000,
            max_framerate: f64::from(config.target_fps),
        }),
        ..Default::default()
    };
    room.local_participant()
        .publish_track(LocalTrack::Video(track), options)
        .await
        .context("failed to publish the screen track")?;

    println!(
        "published screen track into room '{}' as '{}'",
        config.room_name, config.identity
    );
    println!(
        "native session heartbeat started (every {}s). Press Ctrl+C to stop.",
        config.heartbeat_seconds
    );
    api::report_native_publisher_event(
        http,
        &config.api_base_url,
        &config.room_name,
        &config.identity,
        "running",
        &encoder.label(),
        &encoder.capture_backend,
        &encoder.encoder_backend,
        None,
    )
    .await
    .context("failed to report publisher running")?;

    let mut ticker = interval(Duration::from_secs(config.heartbeat_seconds));
    ticker.tick().await;
    let started = Instant::now();
    let mut window_started = Instant::now();
    let mut produced: u64 = 0;
    let mut dropped: u64 = 0;
    let mut window_frames: u64 = 0;
    let mut window_bytes: usize = 0;
    let mut window_latency_ms: f64 = 0.0;
    let mut keyframe_requests: u64 = 0;
    let mut rate_target_bps: u64 = u64::from(config.bitrate_kbps) * 1000;
    let frame_duration_us = 1_000_000i64 / i64::from(config.target_fps.max(1));
    let mut frame_index: i64 = 0;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("shutdown signal received");
                break;
            }
            changed = disconnect_rx.changed() => {
                if changed.is_err() || *disconnect_rx.borrow() {
                    anyhow::bail!("LiveKit room disconnected");
                }
            }
            _ = ticker.tick() => {
                if let Some(status) = encoder.try_wait()? {
                    let tail = encoder.stderr_tail();
                    anyhow::bail!("ffmpeg exited: {status} {}", tail.trim());
                }
                let elapsed = window_started.elapsed().as_secs_f64().max(0.001);
                let achieved_fps = window_frames as f64 / elapsed;
                let avg_payload = if window_frames > 0 { window_bytes / window_frames as usize } else { 0 };
                let avg_latency = if window_frames > 0 { window_latency_ms / window_frames as f64 } else { 0.0 };
                println!(
                    "heartbeat: fps={achieved_fps:.1} frames={produced} dropped={dropped} keyframe_requests={keyframe_requests} rate_target_bps={rate_target_bps} uptime={:.0}s",
                    started.elapsed().as_secs_f64()
                );
                if let Err(err) = api::report_native_session(
                    http,
                    &config.api_base_url,
                    &config.room_name,
                    &config.identity,
                    &encoder.label(),
                    achieved_fps,
                    produced,
                    dropped,
                    avg_latency,
                    avg_payload,
                )
                .await
                {
                    eprintln!("heartbeat post failed: {err}");
                }
                window_started = Instant::now();
                window_frames = 0;
                window_bytes = 0;
                window_latency_ms = 0.0;
            }
            maybe_unit = encoder.units.recv() => {
                let Some(unit) = maybe_unit else {
                    let tail = encoder.stderr_tail();
                    anyhow::bail!("ffmpeg stopped producing video. {}", tail.trim());
                };
                let timestamp_us = frame_index.saturating_mul(frame_duration_us);
                frame_index += 1;
                let frame = EncodedVideoFrame {
                    codec: EncodedVideoCodec::H264,
                    payload: &unit.payload,
                    timestamp_us,
                    frame_type: if unit.keyframe { EncodedFrameType::Key } else { EncodedFrameType::Delta },
                    resolution: resolution.clone(),
                    frame_metadata: None,
                };
                if source.capture_encoded_frame(&frame) {
                    produced += 1;
                    window_frames += 1;
                    window_bytes += unit.payload.len();
                    window_latency_ms += unit.read_at.elapsed().as_secs_f64() * 1000.0;
                } else {
                    dropped += 1;
                }
                if source.take_keyframe_request() {
                    keyframe_requests += 1;
                    eprintln!("keyframe requested; the stand-in encoder emits one every {} frames", config.gop_frames());
                }
                if let Some(target) = source.take_rate_control_request() {
                    rate_target_bps = target.target_bitrate_bps;
                }
            }
        }
    }

    let _ = room.close().await;
    Ok(())
}
