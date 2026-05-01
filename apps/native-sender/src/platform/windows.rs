use anyhow::Result;

use super::CaptureBackend;

pub struct WindowsCaptureBackend;

impl CaptureBackend for WindowsCaptureBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn bootstrap_capture_pipeline(&self, dry_run: bool) -> Result<()> {
        if dry_run {
            println!("[windows] dry-run capture bootstrap: DXGI + WASAPI pipeline placeholder");
            return Ok(());
        }

        println!("[windows] capture pipeline bootstrap placeholder (implement DXGI/WASAPI next)");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Use latest GPU drivers, keep display refresh >= 60Hz, and disable Windows power saver for stable capture pacing."
    }
}

