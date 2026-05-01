use anyhow::Result;

use super::CaptureBackend;
use crate::capture::{run_frame_pacing_probe, CaptureTuning};

pub struct WindowsCaptureBackend;

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
            "[windows] running capture pacing probe at {} fps for {}s",
            tuning.target_fps, tuning.probe_seconds
        );
        let stats = run_frame_pacing_probe(tuning)?;
        println!(
            "[windows] probe done: target_fps={} frames={} elapsed={}ms achieved_fps={:.2} avg_interval={:.2}ms",
            stats.target_fps,
            stats.produced_frames,
            stats.elapsed_ms,
            stats.achieved_fps,
            stats.avg_frame_interval_ms
        );
        println!("[windows] next milestone: replace probe with DXGI Desktop Duplication frame source");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Use latest GPU drivers, keep display refresh >= 60Hz, and disable Windows power saver for stable capture pacing."
    }
}

