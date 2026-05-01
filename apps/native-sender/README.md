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

## Current status (M1)

- API health check (`GET /health`)
- Token fetch (`POST /token` with `clientType: native_sender`)
- Platform backend selection (windows/macos)
- Backend diagnostics hints and bootstrap placeholders
- Capture pacing probe with measured achieved FPS (pre-DXGI/ScreenCaptureKit integration)

