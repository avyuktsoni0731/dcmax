use anyhow::Result;

use super::CaptureBackend;
use crate::capture::{run_frame_pacing_probe, CaptureTuning};

pub struct MacOsCaptureBackend;

impl CaptureBackend for MacOsCaptureBackend {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn bootstrap_capture_pipeline(&self, dry_run: bool, tuning: CaptureTuning) -> Result<()> {
        if dry_run {
            println!("[macos] dry-run capture bootstrap: ScreenCaptureKit + CoreAudio placeholder");
            return Ok(());
        }

        println!(
            "[macos] running capture pacing probe at {} fps for {}s",
            tuning.target_fps, tuning.probe_seconds
        );
        let stats = run_frame_pacing_probe(tuning)?;
        println!(
            "[macos] probe done: target_fps={} frames={} elapsed={}ms achieved_fps={:.2} avg_interval={:.2}ms",
            stats.target_fps,
            stats.produced_frames,
            stats.elapsed_ms,
            stats.achieved_fps,
            stats.avg_frame_interval_ms
        );
        println!("[macos] next milestone: replace probe with ScreenCaptureKit frame source");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Grant Screen Recording permission, keep app in foreground for first capture grant, and prefer wired/strong Wi-Fi for 1080p60."
    }
}

