import cors from "cors";
import dotenv from "dotenv";
import express from "express";
import { AccessToken } from "livekit-server-sdk";
import { z } from "zod";

dotenv.config();

const envSchema = z.object({
  PORT: z.coerce.number().default(4000),
  WEB_ORIGIN: z.string().optional(),
  WEB_ORIGINS: z.string().optional(),
  LIVEKIT_URL: z.string().default("ws://localhost:7880"),
  LIVEKIT_API_KEY: z.string().min(1),
  LIVEKIT_API_SECRET: z.string().min(1)
});

const env = envSchema.parse(process.env);
const app = express();
type NativeSessionRecord = {
  roomName: string;
  identity: string;
  backend: string;
  achievedFps: number;
  producedFrames: number;
  droppedFrames: number;
  avgIngestLatencyMs: number;
  avgPayloadBytes: number;
  updatedAt: string;
};
type NativePublisherRecord = {
  roomName: string;
  identity: string;
  state: "starting" | "running" | "stopped" | "error";
  backend: string;
  captureBackend: string;
  encoderBackend: string;
  message?: string;
  updatedAt: string;
};
const nativeSessions = new Map<string, NativeSessionRecord>();
const nativePublishers = new Map<string, NativePublisherRecord>();
const allowedOrigins = (
  env.WEB_ORIGINS ??
  env.WEB_ORIGIN ??
  "http://localhost:3000"
)
  .split(",")
  .map((origin) => origin.trim())
  .filter(Boolean);

app.use(
  cors({
    origin: (origin, callback) => {
      if (!origin || allowedOrigins.includes(origin)) {
        callback(null, true);
        return;
      }
      callback(new Error("Origin not allowed by CORS"));
    }
  })
);
app.use(express.json());

app.get("/health", (_req, res) => {
  res.json({ ok: true });
});

const nativeSessionSchema = z.object({
  roomName: z.string().min(2).max(64).regex(/^[a-zA-Z0-9_-]+$/),
  identity: z.string().min(2).max(64),
  backend: z.string().min(2).max(64),
  achievedFps: z.number().min(0),
  producedFrames: z.number().int().min(0),
  droppedFrames: z.number().int().min(0),
  avgIngestLatencyMs: z.number().min(0),
  avgPayloadBytes: z.number().int().min(0)
});

app.post("/native/sessions", (req, res) => {
  const parsed = nativeSessionSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid native session payload", details: parsed.error.flatten() });
    return;
  }

  const payload = parsed.data;
  const key = `${payload.roomName}:${payload.identity}`;
  const record: NativeSessionRecord = {
    ...payload,
    updatedAt: new Date().toISOString()
  };
  nativeSessions.set(key, record);
  res.json({ ok: true, key, record });
});

app.get("/native/sessions/:roomName", (req, res) => {
  const roomName = req.params.roomName;
  const entries = Array.from(nativeSessions.values()).filter((row) => row.roomName === roomName);
  res.json({
    roomName,
    count: entries.length,
    sessions: entries
  });
});

const nativePublisherEventSchema = z.object({
  roomName: z.string().min(2).max(64).regex(/^[a-zA-Z0-9_-]+$/),
  identity: z.string().min(2).max(64),
  state: z.enum(["starting", "running", "stopped", "error"]),
  backend: z.string().min(2).max(64),
  captureBackend: z.string().min(2).max(64),
  encoderBackend: z.string().min(2).max(64),
  message: z.string().max(256).optional()
});

app.post("/native/publisher/events", (req, res) => {
  const parsed = nativePublisherEventSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid native publisher event payload", details: parsed.error.flatten() });
    return;
  }

  const payload = parsed.data;
  const key = `${payload.roomName}:${payload.identity}`;
  const record: NativePublisherRecord = {
    ...payload,
    updatedAt: new Date().toISOString()
  };
  nativePublishers.set(key, record);
  res.json({ ok: true, key, record });
});

app.get("/native/publisher/:roomName", (req, res) => {
  const roomName = req.params.roomName;
  const entries = Array.from(nativePublishers.values()).filter((row) => row.roomName === roomName);
  res.json({
    roomName,
    count: entries.length,
    publishers: entries
  });
});

const tokenSchema = z.object({
  roomName: z
    .string()
    .min(2)
    .max(64)
    .regex(/^[a-zA-Z0-9_-]+$/),
  identity: z
    .string()
    .min(2)
    .max(32)
    .regex(/^[a-zA-Z0-9_-]+$/),
  clientType: z.enum(["web", "native_sender"]).default("web")
});

app.post("/token", async (req, res) => {
  const parsed = tokenSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid payload", details: parsed.error.flatten() });
    return;
  }

  const { roomName, identity, clientType } = parsed.data;
  const at = new AccessToken(env.LIVEKIT_API_KEY, env.LIVEKIT_API_SECRET, {
    identity
  });

  const canPublishData = clientType === "web";
  const metadata =
    clientType === "native_sender"
      ? JSON.stringify({ role: "native_sender", source: "windows-desktop-duplication" })
      : JSON.stringify({ role: "web_client" });
  at.metadata = metadata;

  at.addGrant({
    roomJoin: true,
    room: roomName,
    canPublish: true,
    canSubscribe: true,
    canPublishData
  });

  const token = await at.toJwt();

  res.json({
    token,
    url: env.LIVEKIT_URL
  });
});

app.listen(env.PORT, () => {
  console.log(`API running on http://localhost:${env.PORT}`);
});
