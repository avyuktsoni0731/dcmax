use anyhow::{Context, Result};
use scrap::{Capturer, Display};
use std::io::ErrorKind;
use std::sync::mpsc::{self, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use super::CaptureBackend;
use crate::capture::{CaptureTuning, CapturedFrame};
use crate::platform::windows_dxgi::probe_primary_adapter;

pub struct WindowsCaptureBackend;

#[derive(Debug, Clone, Copy)]
struct DesktopCaptureProbeStats {
    width: usize,
    height: usize,
    produced_frames: u64,
    would_block_events: u64,
    poll_attempts: u64,
    elapsed_ms: u128,
    achieved_fps: f64,
    avg_frame_bytes: usize,
    pipeline_sent_frames: u64,
    pipeline_dropped_frames: u64,
    pipeline_consumed_frames: u64,
    pipeline_consumed_bytes: usize,
    pipeline_avg_ingest_latency_ms: f64,
}

fn run_windows_desktop_capture_probe(tuning: CaptureTuning) -> Result<DesktopCaptureProbeStats> {
    let display = Display::primary().context("failed to resolve primary display")?;
    let mut capturer = Capturer::new(display).context("failed to initialize desktop capturer")?;
    let width = capturer.width();
    let height = capturer.height();

    let start = Instant::now();
    let deadline = Duration::from_secs(tuning.probe_seconds);
    let mut frames: u64 = 0;
    let mut would_block_events: u64 = 0;
    let mut poll_attempts: u64 = 0;
    let mut total_bytes: usize = 0;
    let (tx, rx) = mpsc::sync_channel::<CapturedFrame>(32);
    let consumer = thread::spawn(move || {
        let mut consumed_frames: u64 = 0;
        let mut consumed_bytes: usize = 0;
        let mut total_ingest_latency_ms: f64 = 0.0;
        while let Ok(frame) = rx.recv() {
            consumed_frames += 1;
            let _frame_dimensions = (frame.width, frame.height);
            consumed_bytes += frame.bytes.len();
            total_ingest_latency_ms += frame.capture_instant.elapsed().as_secs_f64() * 1000.0;
        }
        let avg_ingest_latency_ms = if consumed_frames > 0 {
            total_ingest_latency_ms / consumed_frames as f64
        } else {
            0.0
        };
        (consumed_frames, consumed_bytes, avg_ingest_latency_ms)
    });
    let mut pipeline_sent_frames: u64 = 0;
    let mut pipeline_dropped_frames: u64 = 0;

    while start.elapsed() < deadline {
        poll_attempts += 1;
        match capturer.frame() {
            Ok(frame) => {
                frames += 1;
                total_bytes += frame.len();
                let captured = CapturedFrame {
                    width,
                    height,
                    bytes: frame.to_vec(),
                    capture_instant: Instant::now(),
                };
                match tx.try_send(captured) {
                    Ok(()) => {
                        pipeline_sent_frames += 1;
                    }
                    Err(TrySendError::Full(_)) => {
                        pipeline_dropped_frames += 1;
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        pipeline_dropped_frames += 1;
                    }
                }
                // Target pacing after successful frame capture.
                thread::sleep(Duration::from_micros(1_000_000 / tuning.target_fps as u64));
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                would_block_events += 1;
                thread::sleep(Duration::from_millis(1));
            }
            Err(err) => {
                return Err(err).context("desktop frame capture failed");
            }
        }
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let achieved_fps = frames as f64 / elapsed_secs;
    let avg_frame_bytes = if frames > 0 {
        total_bytes / frames as usize
    } else {
        0
    };
    drop(tx);
    let (pipeline_consumed_frames, pipeline_consumed_bytes, pipeline_avg_ingest_latency_ms) = consumer
        .join()
        .map_err(|_| anyhow::anyhow!("capture pipeline consumer thread panicked"))?;

    Ok(DesktopCaptureProbeStats {
        width,
        height,
        produced_frames: frames,
        would_block_events,
        poll_attempts,
        elapsed_ms,
        achieved_fps,
        avg_frame_bytes,
        pipeline_sent_frames,
        pipeline_dropped_frames,
        pipeline_consumed_frames,
        pipeline_consumed_bytes,
        pipeline_avg_ingest_latency_ms,
    })
}

impl CaptureBackend for WindowsCaptureBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn bootstrap_capture_pipeline(&self, dry_run: bool, tuning: CaptureTuning) -> Result<()> {
        let adapter_probe = probe_primary_adapter()?;
        println!(
            "[windows] dxgi adapter: name='{}' vendor_id={} vram={}MB shared={}MB",
            adapter_probe.adapter_name,
            adapter_probe.vendor_id,
            adapter_probe.dedicated_video_memory_mb,
            adapter_probe.shared_system_memory_mb
        );

        if dry_run {
            println!("[windows] dry-run capture bootstrap: DXGI + WASAPI pipeline placeholder");
            return Ok(());
        }

        println!(
            "[windows] running desktop capture probe at {} fps for {}s",
            tuning.target_fps, tuning.probe_seconds
        );
        let stats = run_windows_desktop_capture_probe(tuning)?;
        println!(
            "[windows] probe done: frames={} elapsed={}ms achieved_fps={:.2} resolution={}x{} avg_frame_bytes={} polls={} would_block={} pipeline_sent={} pipeline_dropped={} pipeline_consumed={} pipeline_consumed_bytes={} pipeline_avg_ingest_latency_ms={:.2}",
            stats.produced_frames,
            stats.elapsed_ms,
            stats.achieved_fps,
            stats.width,
            stats.height,
            stats.avg_frame_bytes,
            stats.poll_attempts,
            stats.would_block_events,
            stats.pipeline_sent_frames,
            stats.pipeline_dropped_frames,
            stats.pipeline_consumed_frames,
            stats.pipeline_consumed_bytes,
            stats.pipeline_avg_ingest_latency_ms
        );
        if stats.achieved_fps < (tuning.target_fps as f64 * 0.5) {
            println!(
                "[windows] note: low new-frame rate can happen if the desktop is mostly static. Move windows/video during probe to measure true active-motion fps."
            );
        }
        println!("[windows] next milestone: route captured frames into encoder/publisher pipeline");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Use latest GPU drivers, keep display refresh >= 60Hz, and disable Windows power saver for stable capture pacing."
    }
}

