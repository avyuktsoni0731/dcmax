import cors from "cors";
import dotenv from "dotenv";
import express from "express";
import helmet from "helmet";
import rateLimit from "express-rate-limit";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { AccessToken, IngressClient, IngressInput } from "livekit-server-sdk";
import { z } from "zod";

dotenv.config();

const envSchema = z.object({
  NODE_ENV: z.enum(["development", "test", "production"]).default("development"),
  PORT: z.coerce.number().default(4000),
  WEB_ORIGIN: z.string().optional(),
  WEB_ORIGINS: z.string().optional(),
  NATIVE_CONTROL_SECRET: z.string().optional(),
  NATIVE_SENDER_BIN: z.string().default("cargo"),
  NATIVE_SENDER_WORKDIR: z.string().optional(),
  LIVEKIT_URL: z.string().default("ws://localhost:7880"),
  LIVEKIT_API_KEY: z.string().min(1),
  LIVEKIT_API_SECRET: z.string().min(1)
});

const env = envSchema.parse(process.env);
if (env.NODE_ENV === "production" && !env.NATIVE_CONTROL_SECRET) {
  throw new Error("NATIVE_CONTROL_SECRET is required in production.");
}
if (env.NODE_ENV === "production" && !(env.WEB_ORIGINS ?? env.WEB_ORIGIN)) {
  throw new Error("WEB_ORIGINS or WEB_ORIGIN must be set in production.");
}
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
type NativeRuntimeStatus = "idle" | "starting" | "running" | "stopping" | "error";
type NativeRuntimeRecord = {
  roomName: string;
  identity: string;
  status: NativeRuntimeStatus;
  pid?: number;
  command: string[];
  startedAt?: string;
  stoppedAt?: string;
  lastError?: string;
  updatedAt: string;
};
type NativeRuntimeLogEntry = {
  ts: string;
  stream: "stdout" | "stderr" | "system";
  message: string;
};
const nativeSessions = new Map<string, NativeSessionRecord>();
const nativePublishers = new Map<string, NativePublisherRecord>();
const nativeRuntimes = new Map<string, NativeRuntimeRecord>();
const nativeRuntimeProcs = new Map<string, ReturnType<typeof spawn>>();
const nativeRuntimeLogs = new Map<string, NativeRuntimeLogEntry[]>();
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
app.use(
  helmet({
    crossOriginEmbedderPolicy: false
  })
);
app.use(
  rateLimit({
    windowMs: 60_000,
    max: 300,
    standardHeaders: true,
    legacyHeaders: false
  })
);
app.use(express.json());

function resolveNativeSenderWorkdir() {
  if (env.NATIVE_SENDER_WORKDIR) return env.NATIVE_SENDER_WORKDIR;
  const candidates = [
    path.resolve(process.cwd(), "../native-sender"),
    path.resolve(process.cwd(), "apps/native-sender"),
    path.resolve(process.cwd(), "../../apps/native-sender")
  ];
  const existing = candidates.find((candidate) => fs.existsSync(candidate));
  return existing ?? candidates[1];
}

function requireNativeControl(req: express.Request, res: express.Response) {
  if (!env.NATIVE_CONTROL_SECRET) return true;
  const provided = req.header("x-native-control-secret");
  if (provided === env.NATIVE_CONTROL_SECRET) return true;
  res.status(401).json({ error: "Unauthorized native control request" });
  return false;
}

function resolveLiveKitHttpUrl() {
  return env.LIVEKIT_URL.replace(/^wss:\/\//, "https://").replace(/^ws:\/\//, "http://");
}

function appendRuntimeLog(roomName: string, stream: NativeRuntimeLogEntry["stream"], message: string) {
  if (!message.trim()) return;
  const prev = nativeRuntimeLogs.get(roomName) ?? [];
  const next: NativeRuntimeLogEntry[] = [
    ...prev,
    {
      ts: new Date().toISOString(),
      stream,
      message: message.slice(0, 1000)
    }
  ].slice(-200);
  nativeRuntimeLogs.set(roomName, next);
}

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
  appendRuntimeLog(payload.roomName, "system", `[publisher:${payload.state}] ${payload.backend} ${payload.captureBackend}/${payload.encoderBackend}${payload.message ? ` - ${payload.message}` : ""}`);

  const runtime = nativeRuntimes.get(payload.roomName);
  if (runtime && runtime.identity === payload.identity) {
    const mappedStatus: NativeRuntimeStatus =
      payload.state === "running"
        ? "running"
        : payload.state === "starting"
          ? "starting"
          : payload.state === "stopped"
            ? "idle"
            : "error";
    nativeRuntimes.set(payload.roomName, {
      ...runtime,
      status: mappedStatus,
      lastError: payload.state === "error" ? payload.message ?? runtime.lastError : runtime.lastError,
      updatedAt: new Date().toISOString()
    });
  }

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

const nativeRuntimeStartSchema = z.object({
  roomName: z.string().min(2).max(64).regex(/^[a-zA-Z0-9_-]+$/),
  identity: z.string().min(2).max(64).default("native-sender"),
  dryRun: z.boolean().optional().default(false),
  targetFps: z.number().int().min(24).max(240).optional().default(60),
  probeSeconds: z.number().int().min(1).max(60).optional().default(3),
  heartbeatSeconds: z.number().int().min(1).max(60).optional().default(1),
  capture: z.enum(["auto", "scrap", "ffmpeg-ddagrab"]).optional().default("scrap"),
  encoder: z.enum(["fast", "ffmpeg-libx264", "ffmpeg-h264-nvenc"]).optional().default("ffmpeg-h264-nvenc"),
  autoEnsureIngress: z.boolean().optional().default(true)
});

const nativeRuntimeStopSchema = z.object({
  roomName: z.string().min(2).max(64).regex(/^[a-zA-Z0-9_-]+$/)
});

app.post("/native/runtime/start", async (req, res) => {
  if (!requireNativeControl(req, res)) return;
  const parsed = nativeRuntimeStartSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid native runtime start payload", details: parsed.error.flatten() });
    return;
  }

  const payload = parsed.data;
  const existing = nativeRuntimeProcs.get(payload.roomName);
  if (existing && !existing.killed) {
    const running = nativeRuntimes.get(payload.roomName);
    res.status(409).json({ error: "Native runtime already running for room", runtime: running ?? null });
    return;
  }

  const workdir = resolveNativeSenderWorkdir();
  if (!fs.existsSync(workdir)) {
    res.status(500).json({ error: `Native sender workdir does not exist: ${workdir}` });
    return;
  }
  const args = [
    "run",
    "--",
    "--room",
    payload.roomName,
    "--identity",
    payload.identity,
    "--target-fps",
    String(payload.targetFps),
    "--probe-seconds",
    String(payload.probeSeconds),
    "--heartbeat-seconds",
    String(payload.heartbeatSeconds),
    "--capture",
    payload.capture,
    "--encoder",
    payload.encoder
  ];
  if (payload.dryRun) args.push("--dry-run");

  let livekitWhipUrlFromIngress: string | undefined;
  let livekitRtmpUrlFromIngress: string | undefined;
  if (payload.autoEnsureIngress && !payload.dryRun) {
    try {
      const ingressClient = new IngressClient(resolveLiveKitHttpUrl(), env.LIVEKIT_API_KEY, env.LIVEKIT_API_SECRET);
      const existing = await ingressClient.listIngress({
        roomName: payload.roomName
      });
      const byIdentity = existing.find(
        (ingress) => ingress.inputType === IngressInput.RTMP_INPUT && ingress.participantIdentity === payload.identity
      );
      const ingress =
        byIdentity ??
        (await ingressClient.createIngress(IngressInput.RTMP_INPUT, {
          name: `native-rtmp-${payload.roomName}`,
          roomName: payload.roomName,
          participantIdentity: payload.identity,
          participantName: payload.identity,
          enableTranscoding: true,
          bypassTranscoding: false
        }));
      if (ingress.url && ingress.streamKey) {
        livekitRtmpUrlFromIngress = `${ingress.url.replace(/\/$/, "")}/${ingress.streamKey}`;
      }
    } catch (err) {
      res.status(500).json({
        error: "Failed to ensure WHIP ingress before runtime start",
        message: err instanceof Error ? err.message : "unknown error"
      });
      return;
    }
  }

  const child = spawn(env.NATIVE_SENDER_BIN, args, {
    cwd: workdir,
    env: {
      ...process.env,
      API_BASE_URL: `http://localhost:${env.PORT}`,
      ROOM_NAME: payload.roomName,
      IDENTITY: payload.identity,
      CLIENT_TYPE: "native_sender",
      ...(livekitWhipUrlFromIngress ? { LIVEKIT_WHIP_URL: livekitWhipUrlFromIngress } : {}),
      ...(livekitRtmpUrlFromIngress ? { LIVEKIT_RTMP_URL: livekitRtmpUrlFromIngress } : {})
    },
    stdio: ["ignore", "pipe", "pipe"]
  });

  const command = [env.NATIVE_SENDER_BIN, ...args];
  const now = new Date().toISOString();
  const record: NativeRuntimeRecord = {
    roomName: payload.roomName,
    identity: payload.identity,
    status: "starting",
    pid: child.pid,
    command,
    startedAt: now,
    updatedAt: now
  };
  nativeRuntimeProcs.set(payload.roomName, child);
  nativeRuntimes.set(payload.roomName, record);
  nativeRuntimeLogs.set(payload.roomName, []);
  appendRuntimeLog(payload.roomName, "system", `spawning native runtime: ${command.join(" ")}`);

  child.stdout.on("data", (chunk) => {
    const text = chunk.toString("utf8");
    appendRuntimeLog(payload.roomName, "stdout", text);
    if (text.includes("native session heartbeat started")) {
      const prev = nativeRuntimes.get(payload.roomName);
      if (!prev) return;
      nativeRuntimes.set(payload.roomName, {
        ...prev,
        status: "running",
        updatedAt: new Date().toISOString()
      });
    }
  });

  child.stderr.on("data", (chunk) => {
    const text = chunk.toString("utf8").trim();
    appendRuntimeLog(payload.roomName, "stderr", text);
    if (!text) return;
    const prev = nativeRuntimes.get(payload.roomName);
    if (!prev) return;
    nativeRuntimes.set(payload.roomName, {
      ...prev,
      status: "error",
      lastError: text.slice(0, 500),
      updatedAt: new Date().toISOString()
    });
  });

  child.on("exit", (code, signal) => {
    nativeRuntimeProcs.delete(payload.roomName);
    const prev = nativeRuntimes.get(payload.roomName);
    if (!prev) return;
    const isExpectedStop = prev.status === "stopping" || (code === 0 && signal == null);
    nativeRuntimes.set(payload.roomName, {
      ...prev,
      status: isExpectedStop ? "idle" : "error",
      pid: undefined,
      stoppedAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      lastError: isExpectedStop ? prev.lastError : `native sender exited unexpectedly (code=${code}, signal=${signal})`
    });
  });

  res.json({
    ok: true,
    runtime: record,
    whipUrl: livekitWhipUrlFromIngress ?? null,
    rtmpUrl: livekitRtmpUrlFromIngress ?? null
  });
});

app.post("/native/runtime/stop", (req, res) => {
  if (!requireNativeControl(req, res)) return;
  const parsed = nativeRuntimeStopSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid native runtime stop payload", details: parsed.error.flatten() });
    return;
  }
  const payload = parsed.data;
  const child = nativeRuntimeProcs.get(payload.roomName);
  if (!child) {
    res.status(404).json({ error: "No native runtime process found for room" });
    return;
  }

  const prev = nativeRuntimes.get(payload.roomName);
  if (prev) {
    nativeRuntimes.set(payload.roomName, {
      ...prev,
      status: "stopping",
      updatedAt: new Date().toISOString()
    });
  }
  const killed = child.kill("SIGINT");
  res.json({ ok: true, roomName: payload.roomName, killed });
});

app.get("/native/runtime/:roomName", (req, res) => {
  if (!requireNativeControl(req, res)) return;
  const roomName = req.params.roomName;
  const runtime = nativeRuntimes.get(roomName) ?? null;
  res.json({ roomName, runtime });
});

app.get("/native/runtime/:roomName/logs", (req, res) => {
  if (!requireNativeControl(req, res)) return;
  const roomName = req.params.roomName;
  const logs = nativeRuntimeLogs.get(roomName) ?? [];
  res.json({ roomName, count: logs.length, logs });
});

const nativeIngressEnsureSchema = z.object({
  roomName: z.string().min(2).max(64).regex(/^[a-zA-Z0-9_-]+$/),
  identity: z.string().min(2).max(64).default("native-sender"),
  name: z.string().min(2).max(128).optional().default("native-whip"),
  enableTranscoding: z.boolean().optional().default(true)
});

app.post("/native/ingress/ensure", async (req, res) => {
  if (!requireNativeControl(req, res)) return;
  const parsed = nativeIngressEnsureSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid native ingress payload", details: parsed.error.flatten() });
    return;
  }
  const payload = parsed.data;
  try {
    const ingressClient = new IngressClient(resolveLiveKitHttpUrl(), env.LIVEKIT_API_KEY, env.LIVEKIT_API_SECRET);
    const existing = await ingressClient.listIngress({
      roomName: payload.roomName
    });
    const byIdentity = existing.find(
      (ingress) => ingress.inputType === IngressInput.RTMP_INPUT && ingress.participantIdentity === payload.identity
    );

    const ingress =
      byIdentity ??
      (await ingressClient.createIngress(IngressInput.RTMP_INPUT, {
        name: `${payload.name}-rtmp-${payload.roomName}`,
        roomName: payload.roomName,
        participantIdentity: payload.identity,
        participantName: payload.identity,
        enableTranscoding: payload.enableTranscoding,
        bypassTranscoding: !payload.enableTranscoding
      }));

    const base = ingress.url ?? "";
    const streamKey = ingress.streamKey ?? "";
    const ingestUrl = base && streamKey ? `${base.replace(/\/$/, "")}/${streamKey}` : base;
    res.json({
      ok: true,
      roomName: payload.roomName,
      identity: payload.identity,
      ingressId: ingress.ingressId,
      inputType: ingress.inputType,
      ingestBaseUrl: ingress.url,
      streamKey: ingress.streamKey,
      ingestUrl,
      ingestType: "rtmp",
      note: "Set native-sender LIVEKIT_RTMP_URL to ingestUrl for reliable FFmpeg publishing."
    });
  } catch (err) {
    res.status(500).json({
      error: "Failed to ensure WHIP ingress",
      message: err instanceof Error ? err.message : "unknown error"
    });
  }
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
