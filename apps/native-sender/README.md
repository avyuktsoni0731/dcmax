# Native Sender (Windows + macOS Scaffold)

This app is the native capture publisher companion for MyCord.

Current phase:

- Shared token bootstrap flow (`clientType: native_sender`)
- Platform module abstraction for Windows/macOS
- Dry-run startup path for both platforms

Planned next:

- Windows: DXGI Desktop Duplication + WASAPI loopback capture
- macOS: ScreenCaptureKit + CoreAudio loopback capture
- Hardware encoding path and LiveKit publishing integration

## Setup

1. Install Rust toolchain:

```bash
rustup --version
```

2. Copy env file:

```bash
cp .env.example .env
```

3. Run:

```bash
cargo run -- --dry-run
```

## CLI

- `--room <name>` override room
- `--identity <id>` override sender identity
- `--platform windows|macos|auto` choose platform backend
- `--dry-run` skip media pipeline boot
- `--target-fps <fps>` probe/pipeline target fps (default `60`)
- `--probe-seconds <sec>` pacing probe duration when not dry-run (default `5`)
- `--heartbeat-seconds <sec>` API report heartbeat interval after probe (default `3`)
- `--encoder fast|ffmpeg-libx264|ffmpeg-h264-nvenc` choose encoder stage backend
- `--capture auto|scrap|ffmpeg-ddagrab` choose capture backend (default `scrap`)

## Current status (M1)

- API health check (`GET /health`)
- Token fetch (`POST /token` with `clientType: native_sender`)
- Platform backend selection (windows/macos)
- Backend diagnostics hints and bootstrap placeholders
- Windows desktop frame capture probe via `scrap` with measured achieved FPS/resolution
- Windows DXGI adapter probe (primary GPU introspection) to prepare migration to Desktop Duplication
- Cross-platform pacing probe scaffolding remains for future macOS ScreenCaptureKit integration
- Encoder-ready frame contract (`CapturedFrame`) with capture timestamp and ingest-latency metrics
- Encoder-input adapter stage (`EncoderInputFrame`) with conversion metrics and end-to-end ingest timing
- Windows capture now attempts `ffmpeg` DXGI source (`ddagrab`) first, then falls back to `scrap`
  - For machines where ffmpeg capture is unreliable, use `--capture scrap` (now default).
- Native sender now posts session quality reports to API (`POST /native/sessions`)
- Native sender keeps posting heartbeat reports until stopped (`Ctrl+C`)
- Windows encoder stage can now run FFmpeg H.264 encoding (NVENC/libx264) for real encode telemetry

## FFmpeg Requirement (Windows Real Capture Path)

For the new `ffmpeg-ddagrab` backend, install FFmpeg and make sure `ffmpeg` is in `PATH`.

Example (winget):

```bash
winget install Gyan.FFmpeg
```

