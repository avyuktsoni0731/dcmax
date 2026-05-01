# Native Direct WebRTC Private-Beta Milestone

Goal: replace the current ingest workaround path with direct native WebRTC publish, then reintroduce native capture behind a private-beta flag.

## Scope

- Keep public production web-first (`NEXT_PUBLIC_ENABLE_NATIVE_EXPERIMENTAL=false`).
- Add private-beta flag gate for native controls/UI.
- Implement direct native publish to LiveKit SFU (no RTMP/WHIP bridge for main path).

## Milestone Breakdown

1. **Native transport layer**
   - Add direct WebRTC publish transport in `apps/native-sender`.
   - Reuse API token issuance with `clientType=native_sender`.
   - Publish H264 with stable timing and periodic keyframes.

2. **Reliability + observability**
   - Extend publisher lifecycle events with reconnect counters and last fatal reason.
   - Add heartbeat-based session health events to API.
   - Surface a private-beta diagnostics panel in web app.

3. **Safety gates**
   - Add `NATIVE_PRIVATE_BETA_IDENTITIES` allowlist in API.
   - Reject runtime start for non-allowlisted identities in production.
   - Keep secret header auth mandatory.

4. **Validation**
   - Two-device quality validation at 1080p60.
   - Long-run stability test (30+ min).
   - Packet-loss simulation pass and reconnect behavior verification.

## Exit Criteria

- Native path shows clear smoothness gain over web capture in private-beta tests.
- No frozen-frame regressions across 30-minute sessions.
- Runtime control endpoints remain unauthorized for public traffic.
