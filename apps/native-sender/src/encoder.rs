#[cfg(target_os = "windows")]
#[path = "encoder_ffmpeg.rs"]
mod ffmpeg;
#[cfg(target_os = "macos")]
#[path = "encoder_macos.rs"]
mod macos;
#[cfg(target_os = "macos")]
#[path = "vt.rs"]
mod vt;
#[cfg(target_os = "windows")]
#[path = "encoder_windows.rs"]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::ScreenEncoder;
#[cfg(target_os = "windows")]
pub use windows::ScreenEncoder;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("native-sender supports macOS and Windows");
