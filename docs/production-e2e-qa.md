# Production E2E QA Checklist

Use this checklist after API and web are deployed.

## Preconditions

- API URL is HTTPS and `/health` returns `{ "ok": true }`.
- Web URL is HTTPS and points to the production API URL.
- `NEXT_PUBLIC_ENABLE_NATIVE_EXPERIMENTAL=false` in web deployment.

## Two-device smoke test

1. Open the deployed web URL on device A and B.
2. Join the same room with different identities.
3. Confirm:
   - both participants appear connected,
   - voice works in both directions,
   - mute/unmute behaves correctly,
   - web screen share starts/stops without freezing.

## Reconnect and browser matrix

- Refresh one participant tab and confirm automatic recovery.
- Run one pass each on:
  - Chrome
  - Edge
  - Safari

## LiveKit Cloud validation

- Verify room/session activity appears for each smoke test.
- Verify no recurring runtime/ingress error spam.
- Verify join/leave metrics are coherent with test timeline.

## Pass criteria

- All tests above pass without blocked joins, frozen video, or repeated disconnect loops.
