use anyhow::Result;

use super::CaptureBackend;

pub struct MacOsCaptureBackend;

impl CaptureBackend for MacOsCaptureBackend {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn bootstrap_capture_pipeline(&self, dry_run: bool) -> Result<()> {
        if dry_run {
            println!("[macos] dry-run capture bootstrap: ScreenCaptureKit + CoreAudio placeholder");
            return Ok(());
        }

        println!("[macos] capture pipeline bootstrap placeholder (implement ScreenCaptureKit next)");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Grant Screen Recording permission, keep app in foreground for first capture grant, and prefer wired/strong Wi-Fi for 1080p60."
    }
}

