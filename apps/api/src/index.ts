import cors from "cors";
import dotenv from "dotenv";
import express from "express";
import { AccessToken } from "livekit-server-sdk";
import { z } from "zod";

dotenv.config();

const envSchema = z.object({
  PORT: z.coerce.number().default(4000),
  WEB_ORIGIN: z.string().default("http://localhost:3000"),
  LIVEKIT_URL: z.string().default("ws://localhost:7880"),
  LIVEKIT_API_KEY: z.string().min(1),
  LIVEKIT_API_SECRET: z.string().min(1)
});

const env = envSchema.parse(process.env);
const app = express();

app.use(
  cors({
    origin: env.WEB_ORIGIN
  })
);
app.use(express.json());

app.get("/health", (_req, res) => {
  res.json({ ok: true });
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
    .regex(/^[a-zA-Z0-9_-]+$/)
});

app.post("/token", async (req, res) => {
  const parsed = tokenSchema.safeParse(req.body);
  if (!parsed.success) {
    res.status(400).json({ error: "Invalid payload", details: parsed.error.flatten() });
    return;
  }

  const { roomName, identity } = parsed.data;
  const at = new AccessToken(env.LIVEKIT_API_KEY, env.LIVEKIT_API_SECRET, {
    identity
  });

  at.addGrant({
    roomJoin: true,
    room: roomName,
    canPublish: true,
    canSubscribe: true,
    canPublishData: false
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
