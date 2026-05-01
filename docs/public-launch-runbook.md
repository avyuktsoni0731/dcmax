# Public Web-First Launch Runbook

This runbook deploys MyCord in web-first mode with native controls disabled for public users.

## 1) API deploy (Railway/Render)

Build command:

```bash
npm run build --workspace api
```

Start command:

```bash
npm run start --workspace api
```

Required environment variables:

- `NODE_ENV=production`
- `PORT=4000` (or provider port)
- `WEB_ORIGINS=https://your-web-domain.vercel.app`
- `LIVEKIT_URL=wss://your-livekit-cloud-url`
- `LIVEKIT_API_KEY=...`
- `LIVEKIT_API_SECRET=...`
- `NATIVE_CONTROL_SECRET=long-random-secret`

Post-deploy checks:

```bash
curl https://YOUR_API_DOMAIN/health
```

Expected: `{"ok":true}`

Validate native auth is enforced:

```bash
curl -X POST https://YOUR_API_DOMAIN/native/runtime/start -H "Content-Type: application/json" -d "{\"roomName\":\"mycord-room\",\"identity\":\"native-sender\"}"
```

Expected: `401 Unauthorized`

## 2) Web deploy (Vercel)

Set Vercel project root directory to `apps/web`.

Required environment variables:

- `NEXT_PUBLIC_API_BASE_URL=https://YOUR_API_DOMAIN`
- `NEXT_PUBLIC_ENABLE_NATIVE_EXPERIMENTAL=false`
- `NEXT_PUBLIC_NATIVE_CONTROL_SECRET` unset or empty

## 3) End-to-end production QA

Use two devices and the deployed HTTPS URL.

Checklist:

- Both devices can join same room.
- Voice is bidirectional.
- Web screen share starts/stops cleanly.
- Rejoin/reconnect works after tab refresh.
- Chrome + Edge pass, and one Safari pass.
- LiveKit Cloud dashboard shows normal room/session activity and no native ingest spam.

## 4) Rollout note

Public release is web-only. Native sender paths stay in codebase for private/internal testing only.
