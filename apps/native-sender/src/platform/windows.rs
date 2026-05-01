use anyhow::{Context, Result};
use scrap::{Capturer, Display};
use std::io::ErrorKind;
use std::thread;
use std::time::{Duration, Instant};

use super::CaptureBackend;
use crate::capture::CaptureTuning;

pub struct WindowsCaptureBackend;

#[derive(Debug, Clone, Copy)]
struct DesktopCaptureProbeStats {
    width: usize,
    height: usize,
    produced_frames: u64,
    elapsed_ms: u128,
    achieved_fps: f64,
    avg_frame_bytes: usize,
}

fn run_windows_desktop_capture_probe(tuning: CaptureTuning) -> Result<DesktopCaptureProbeStats> {
    let display = Display::primary().context("failed to resolve primary display")?;
    let mut capturer = Capturer::new(display).context("failed to initialize desktop capturer")?;
    let width = capturer.width();
    let height = capturer.height();

    let start = Instant::now();
    let deadline = Duration::from_secs(tuning.probe_seconds);
    let mut frames: u64 = 0;
    let mut total_bytes: usize = 0;
    let frame_interval = Duration::from_micros(1_000_000 / tuning.target_fps as u64);
    let mut next_tick = start;

    while start.elapsed() < deadline {
        next_tick += frame_interval;

        loop {
            match capturer.frame() {
                Ok(frame) => {
                    frames += 1;
                    total_bytes += frame.len();
                    break;
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(err) => {
                    return Err(err).context("desktop frame capture failed");
                }
            }
        }

        let now = Instant::now();
        if next_tick > now {
            thread::sleep(next_tick - now);
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

    Ok(DesktopCaptureProbeStats {
        width,
        height,
        produced_frames: frames,
        elapsed_ms,
        achieved_fps,
        avg_frame_bytes,
    })
}

impl CaptureBackend for WindowsCaptureBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn bootstrap_capture_pipeline(&self, dry_run: bool, tuning: CaptureTuning) -> Result<()> {
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
            "[windows] probe done: frames={} elapsed={}ms achieved_fps={:.2} resolution={}x{} avg_frame_bytes={}",
            stats.produced_frames,
            stats.elapsed_ms,
            stats.achieved_fps,
            stats.width,
            stats.height,
            stats.avg_frame_bytes
        );
        println!("[windows] next milestone: route captured frames into encoder/publisher pipeline");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Use latest GPU drivers, keep display refresh >= 60Hz, and disable Windows power saver for stable capture pacing."
    }
}

