# MyCord Web Rebuild (Local-First)

Web-first rebuild of MyCord focused on smooth 1:1 voice and screen sharing.

## Stack

- `apps/web`: Next.js + TypeScript + Tailwind + LiveKit client SDK
- `apps/api`: Node.js + Express + TypeScript + LiveKit server SDK
- `LiveKit server`: local Docker deployment for zero-cost development

## Local Setup

1. Install dependencies:

```bash
npm install
```

2. Start LiveKit locally:

```bash
docker compose -f docker-compose.livekit.yml up -d
```

3. Run API:

```bash
npm run dev:api
```

4. Run web app:

```bash
npm run dev:web
```

5. Open:

```text
http://localhost:3000
```

## Environment

Copy:

- `apps/api/.env.example` to `apps/api/.env`
- `apps/web/.env.example` to `apps/web/.env.local`

Defaults are configured for local LiveKit:

- URL: `ws://localhost:7880`
- API key: `devkey`
- API secret: `secret`

## Validation Matrix

See `docs/test-matrix.md` for browser/OS validation checklist and known limitations.
For the high-fidelity native-capture roadmap, see `docs/native-capture-blueprint.md`.
For the production web-first launch workflow, see `docs/public-launch-runbook.md`.
For production validation after deploy, see `docs/production-e2e-qa.md`.
For the post-launch native private-beta milestone, see `docs/native-private-beta-milestone.md`.

## Native Sender Scaffold

A cross-platform native sender scaffold now exists at `apps/native-sender` (Windows + macOS module layout).

## Cross-Device HTTPS Setup (Recommended)

For testing from another laptop/phone, keep media on LiveKit Cloud and tunnel only web/API.

1. Create a LiveKit Cloud project and copy:
   - project URL (WSS)
   - API key
   - API secret

2. Run local services:

```bash
npm run dev:api
npm run dev:web -- --hostname 0.0.0.0 --port 3000
```

3. Start ngrok for web and api:

```bash
ngrok http 3000
ngrok http 4000
```

4. Update env files:

- `apps/web/.env.local`

```env
NEXT_PUBLIC_API_BASE_URL=https://YOUR_API_NGROK_URL
```

- `apps/api/.env`

```env
PORT=4000
WEB_ORIGINS=https://YOUR_WEB_NGROK_URL,http://localhost:3000
LIVEKIT_URL=wss://YOUR_LIVEKIT_CLOUD_URL
LIVEKIT_API_KEY=YOUR_LIVEKIT_CLOUD_KEY
LIVEKIT_API_SECRET=YOUR_LIVEKIT_CLOUD_SECRET
```

5. Restart web + api processes and open `https://YOUR_WEB_NGROK_URL` on both devices.

Notes:
- Do not tunnel LiveKit media plane with `ngrok http 7880`; signaling can connect but peer/media transport will fail.
- If Next.js warns about dev origins, add your ngrok hostname to `allowedDevOrigins` in `apps/web/next.config.mjs`.

## Production Deployment

Public launch should run web-first:

- Deploy API with `NODE_ENV=production`, strict `WEB_ORIGINS`, and `NATIVE_CONTROL_SECRET` set.
- Deploy web with `NEXT_PUBLIC_ENABLE_NATIVE_EXPERIMENTAL=false`.
- Keep `NEXT_PUBLIC_NATIVE_CONTROL_SECRET` unset/empty in public web environments.

See `docs/public-launch-runbook.md` for exact commands and verification steps.
