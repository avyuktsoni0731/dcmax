use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub struct CaptureTuning {
    pub target_fps: u32,
    pub probe_seconds: u64,
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct CaptureProbeStats {
    pub target_fps: u32,
    pub elapsed_ms: u128,
    pub produced_frames: u64,
    pub achieved_fps: f64,
    pub avg_frame_interval_ms: f64,
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn run_frame_pacing_probe(tuning: CaptureTuning) -> Result<CaptureProbeStats> {
    let frame_interval = Duration::from_micros(1_000_000 / tuning.target_fps as u64);
    let start = Instant::now();
    let mut next_tick = start;
    let mut frames: u64 = 0;

    while start.elapsed() < Duration::from_secs(tuning.probe_seconds) {
        next_tick += frame_interval;
        let now = Instant::now();
        if next_tick > now {
            thread::sleep(next_tick - now);
        } else {
            // If we are late, resync the scheduler to avoid accumulating lag.
            next_tick = Instant::now();
        }
        frames += 1;
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let achieved_fps = frames as f64 / elapsed_secs;
    let avg_frame_interval_ms = 1000.0 / achieved_fps.max(0.0001);

    Ok(CaptureProbeStats {
        target_fps: tuning.target_fps,
        elapsed_ms,
        produced_frames: frames,
        achieved_fps,
        avg_frame_interval_ms,
    })
}

