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
