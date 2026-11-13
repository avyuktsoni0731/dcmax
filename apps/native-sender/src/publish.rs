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
use tokio::time::{interval, Duration, MissedTickBehavior};

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
        width: encoder.width,
        height: encoder.height,
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
    let mut rate_tick = interval(Duration::from_secs(1));
    rate_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    rate_tick.tick().await;
    let started = Instant::now();
    let mut window_started = Instant::now();
    let mut produced: u64 = 0;
    let mut dropped: u64 = 0;
    let mut window_frames: u64 = 0;
    let mut window_bytes: usize = 0;
    let mut window_latency_ms: f64 = 0.0;
    let mut keyframe_requests: u64 = 0;
    let mut governor = RateGovernor::new(u64::from(config.bitrate_kbps) * 1000, config.target_fps);
    let mut rate_frames: u64 = 0;
    let mut rate_send_drops: u64 = 0;
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
                if let Some(message) = encoder.fatal_error()? {
                    anyhow::bail!(message);
                }
                let elapsed = window_started.elapsed().as_secs_f64().max(0.001);
                let achieved_fps = window_frames as f64 / elapsed;
                let avg_payload = if window_frames > 0 { window_bytes / window_frames as usize } else { 0 };
                let avg_latency = if window_frames > 0 { window_latency_ms / window_frames as f64 } else { 0.0 };
                println!(
                    "heartbeat: fps={achieved_fps:.1} frames={produced} dropped={dropped} queue_drops={} keyframe_requests={keyframe_requests} rate_target_bps={} uptime={:.0}s",
                    encoder.queue_drops(),
                    governor.applied_bps(),
                    started.elapsed().as_secs_f64()
                );
                // The frame loop has to keep reading while this post is in flight.
                // Awaiting it here fills the encode queue and capture stalls.
                let report_http = http.clone();
                let api_base_url = config.api_base_url.clone();
                let room_name = config.room_name.clone();
                let identity = config.identity.clone();
                let backend = encoder.label();
                tokio::spawn(async move {
                    if let Err(err) = api::report_native_session(
                        &report_http,
                        &api_base_url,
                        &room_name,
                        &identity,
                        &backend,
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
                });
                window_started = Instant::now();
                window_frames = 0;
                window_bytes = 0;
                window_latency_ms = 0.0;
            }
            _ = rate_tick.tick() => {
                let decision = governor.decide(rate_frames, rate_send_drops);
                rate_frames = 0;
                rate_send_drops = 0;
                if decision.changed {
                    encoder.set_bitrate_bps(decision.bps);
                    eprintln!(
                        "encoder bitrate -> {} bps (estimate {}, {})",
                        decision.bps, decision.estimate, decision.reason
                    );
                }
            }
            maybe_unit = encoder.units.recv() => {
                let Some(unit) = maybe_unit else {
                    let tail = encoder.stderr_tail();
                    anyhow::bail!("ffmpeg stopped producing video. {}", tail.trim());
                };
                let timestamp_us = match unit.timestamp_us {
                    Some(timestamp) => timestamp,
                    None => {
                        let timestamp = frame_index.saturating_mul(frame_duration_us);
                        frame_index += 1;
                        timestamp
                    }
                };
                let frame = EncodedVideoFrame {
                    codec: EncodedVideoCodec::H264,
                    payload: &unit.payload,
                    timestamp_us,
                    frame_type: if unit.keyframe { EncodedFrameType::Key } else { EncodedFrameType::Delta },
                    resolution: resolution.clone(),
                    frame_metadata: None,
                };
                rate_frames += 1;
                if source.capture_encoded_frame(&frame) {
                    produced += 1;
                    window_frames += 1;
                    window_bytes += unit.payload.len();
                    window_latency_ms += unit.read_at.elapsed().as_secs_f64() * 1000.0;
                } else {
                    dropped += 1;
                    rate_send_drops += 1;
                }
                if source.take_keyframe_request() {
                    keyframe_requests += 1;
                    encoder.request_keyframe();
                }
                if let Some(target) = source.take_rate_control_request() {
                    // Frame rate stays where it was set. Chasing it down after
                    // a stall makes the stall permanent.
                    governor.note_estimate(target.target_bitrate_bps);
                }
            }
        }
    }

    let _ = room.close().await;
    Ok(())
}

const HOLD_BEFORE_CUT: u32 = 3;

struct RateDecision {
    bps: u64,
    estimate: u64,
    changed: bool,
    reason: &'static str,
}

/// Keeps the encoder on the configured bitrate unless the link itself is the limit.
///
/// LiveKit's estimate falls when our own capture stalls, because a one-frame
/// second looks like a path that cannot carry the stream. Following that sample
/// is what leaves a fast game blocky for half a minute. A cut happens only after
/// several seconds of full frame rate with the estimate still below the rate we
/// are already sending, and only when that estimate is flat or still falling.
/// An estimate that is climbing is the link opening up, so the encoder stays
/// on the target instead of waiting for the controller to catch it. A stall
/// puts the target back immediately, so the frames after the hitch are already
/// at full rate.
struct RateGovernor {
    configured: u64,
    applied: u64,
    estimate: u64,
    estimate_at_last_decide: u64,
    target_fps: u32,
    constrained_seconds: u32,
}

impl RateGovernor {
    fn new(configured: u64, target_fps: u32) -> Self {
        let configured = configured.max(200_000);
        Self {
            configured,
            applied: configured,
            estimate: configured,
            estimate_at_last_decide: configured,
            target_fps: target_fps.max(1),
            constrained_seconds: 0,
        }
    }

    fn applied_bps(&self) -> u64 {
        self.applied
    }

    fn note_estimate(&mut self, estimate: u64) {
        self.estimate = estimate.clamp(200_000, self.configured);
    }

    fn decide(&mut self, frames: u64, send_drops: u64) -> RateDecision {
        let previous = self.applied;
        let target_frames = u64::from(self.target_fps);
        let stalled = frames.saturating_mul(2) < target_frames;
        let healthy = frames >= target_frames * 3 / 4;
        let rose = self.estimate > self.estimate_at_last_decide.saturating_add(100_000);
        let fell = self.estimate + 100_000 < self.estimate_at_last_decide;
        if stalled {
            // The missing frames never left this machine. Restore the target
            // now so the next second is not still encoded at the collapsed rate.
            self.constrained_seconds = 0;
            self.applied = self.configured;
        } else if !healthy {
            self.constrained_seconds = 0;
        } else if send_drops > 0 {
            self.constrained_seconds = self.constrained_seconds.saturating_add(2);
            if self.constrained_seconds > HOLD_BEFORE_CUT {
                self.applied = ease_down(self.applied, self.estimate);
            }
        } else if rose || self.estimate + 500_000 >= self.applied {
            self.constrained_seconds = 0;
            if self.estimate > self.applied.saturating_add(500_000) {
                self.applied = climb(self.applied, self.configured);
            }
        } else {
            self.constrained_seconds =
                self.constrained_seconds
                    .saturating_add(if fell { 2 } else { 1 });
            if self.constrained_seconds > HOLD_BEFORE_CUT {
                self.applied = ease_down(self.applied, self.estimate);
            }
        }
        self.estimate_at_last_decide = self.estimate;
        let reason = if self.applied > previous {
            "climbing"
        } else if self.applied < previous {
            "easing"
        } else {
            "holding"
        };
        RateDecision {
            bps: self.applied,
            estimate: self.estimate,
            changed: self.applied != previous,
            reason,
        }
    }
}

fn climb(applied: u64, configured: u64) -> u64 {
    if applied >= configured {
        return configured;
    }
    let gap = configured - applied;
    let step = (gap * 2 / 3).max(2_000_000);
    applied.saturating_add(step).min(configured)
}

fn ease_down(applied: u64, estimate: u64) -> u64 {
    if estimate >= applied {
        return applied;
    }
    let step = (applied / 6).max(500_000);
    applied.saturating_sub(step).max(estimate)
}

#[cfg(test)]
mod tests {
    use super::RateGovernor;

    #[test]
    fn a_startup_stall_does_not_cut_the_target() {
        let mut governor = RateGovernor::new(20_000_000, 60);
        governor.note_estimate(3_800_000);
        for frames in [7, 1, 1] {
            let decision = governor.decide(frames, 0);
            assert_eq!(decision.bps, 20_000_000, "frames={frames}");
            assert!(!decision.changed);
        }
    }

    #[test]
    fn a_flat_low_estimate_is_ignored_until_full_rate_frames_persist() {
        let mut governor = RateGovernor::new(20_000_000, 60);
        governor.note_estimate(4_000_000);
        for _ in 0..2 {
            let decision = governor.decide(57, 0);
            assert_eq!(decision.bps, 20_000_000);
        }
        let eased = governor.decide(57, 0);
        assert!(eased.changed);
        assert!(eased.bps < 20_000_000);
        assert!(eased.bps > 15_000_000, "{}", eased.bps);
    }

    #[test]
    fn a_rising_estimate_keeps_the_target() {
        let mut governor = RateGovernor::new(20_000_000, 60);
        let mut estimate = 4_000_000;
        for _ in 0..10 {
            governor.note_estimate(estimate);
            let decision = governor.decide(57, 0);
            assert_eq!(decision.bps, 20_000_000, "estimate={estimate}");
            estimate += 600_000;
        }
    }

    #[test]
    fn a_stall_restores_the_target_immediately() {
        let mut governor = RateGovernor::new(20_000_000, 60);
        governor.note_estimate(4_000_000);
        for _ in 0..8 {
            governor.decide(57, 0);
        }
        assert!(governor.applied_bps() < 12_000_000);
        let restored = governor.decide(0, 0);
        assert_eq!(restored.bps, 20_000_000);
        assert_eq!(restored.reason, "climbing");
    }

    #[test]
    fn an_offered_increase_climbs_most_of_the_gap() {
        let mut governor = RateGovernor::new(20_000_000, 60);
        governor.note_estimate(4_000_000);
        for _ in 0..8 {
            governor.decide(57, 0);
        }
        let reduced = governor.applied_bps();
        governor.note_estimate(reduced + 2_000_000);
        let climbed = governor.decide(57, 0);
        assert!(
            climbed.bps > reduced + (20_000_000 - reduced) / 2,
            "{}",
            climbed.bps
        );
    }

    #[test]
    fn a_matched_estimate_does_not_climb_past_the_link() {
        let mut governor = RateGovernor::new(20_000_000, 60);
        governor.note_estimate(8_000_000);
        for _ in 0..12 {
            governor.decide(57, 0);
        }
        let settled = governor.applied_bps();
        governor.note_estimate(settled);
        for _ in 0..4 {
            let decision = governor.decide(57, 0);
            assert_eq!(decision.bps, settled);
        }
    }
}
