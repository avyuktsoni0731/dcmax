# Native Capture Blueprint (Best-Quality Path)

This project will use a hybrid architecture:

- Web app: room control, viewing, and collaboration UI
- Native sender app (Windows first): high-performance screen + system-audio capture
- LiveKit Cloud: transport and media distribution

## Why This Approach

Browser-only capture is the current quality bottleneck for smooth 60fps screen sharing. Native capture can provide more consistent frame pacing, better hardware encoder usage, and more reliable system-audio behavior.

## v1 Scope (Windows Native Sender)

- Capture desktop frames via DXGI Desktop Duplication
- Capture system audio via WASAPI loopback
- Encode video with hardware acceleration (NVENC/AMF/Quick Sync where available)
- Publish to LiveKit room as a `native_sender`

Web client remains unchanged as the receiver/controller.

## Token Flow

The API now supports:

- `clientType: "web"` (default)
- `clientType: "native_sender"` for desktop publisher tokens

Request shape:

```json
{
  "roomName": "my-room",
  "identity": "desktop-publisher",
  "clientType": "native_sender"
}
```

## Recommended Native Stack

Choose one implementation track:

1. **Rust + GStreamer/WebRTC bindings** (recommended balance)
2. C++ + libwebrtc (max control, highest complexity)

Initial target: stable 1080p60 motion at low jitter before 1440p/4K tuning.

## Milestones

1. **M1 - Token + Room Join**
   - Native app fetches token from `POST /token`
   - Joins LiveKit room as publisher
2. **M2 - Video Capture**
   - Desktop Duplication feed visible in remote web client
3. **M3 - System Audio**
   - WASAPI loopback track published and audible remotely
4. **M4 - Performance Tuning**
   - Hardware encode selection
   - Target bitrate ladder and frame pacing
5. **M5 - UX Integration**
   - Start/stop from app tray UI
   - Device/source selection and diagnostics

## Acceptance Criteria

- Remote receiver consistently sees >50 fps during active motion at 1080p
- Audio/video sync remains stable during 10+ minute session
- Reconnect works after brief network interruption

