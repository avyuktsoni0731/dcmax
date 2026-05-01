use anyhow::Result;

use crate::capture::CaptureTuning;

pub trait CaptureBackend {
    fn name(&self) -> &'static str;
    fn bootstrap_capture_pipeline(&self, dry_run: bool, tuning: CaptureTuning) -> Result<()>;
    fn diagnostics_hint(&self) -> &'static str;
}

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub mod windows_dxgi;

#[cfg(target_os = "macos")]
pub mod macos;

