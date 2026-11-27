// Mints a LiveKit token and keeps accounts. The picture does not pass through here.
// The email signs the person in. The username is the name on the call.

import { MongoClient } from "mongodb";
import {
  checkLogin,
  checkRegister,
  databaseName,
  hashPassword,
  hashSecret,
  newSalt,
  newSecret,
  passwordMatches,
} from "./auth.js";

const encoder = new TextEncoder();
const SESSION_DAYS = 30;

export default {
  async fetch(request, env) {
    if (request.method === "OPTIONS") {
      return cors(new Response(null, { status: 204 }));
    }
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/health") {
      return cors(Response.json({ ok: true }));
    }
    if (request.method === "POST" && url.pathname === "/token") {
      return cors(await issueToken(request, env));
    }
    if (request.method === "POST" && url.pathname === "/register") {
      return cors(await register(request, env));
    }
    if (request.method === "POST" && url.pathname === "/login") {
      return cors(await login(request, env));
    }
    if (request.method === "POST" && url.pathname === "/session") {
      return cors(await session(request, env));
    }
    if (request.method === "POST" && url.pathname === "/logout") {
      return cors(await logout(request, env));
    }
    if (request.method === "POST" && url.pathname === "/here") {
      return cors(await here(request, env));
    }
    if (request.method === "POST" && url.pathname === "/people") {
      return cors(await people(request, env));
    }
    return cors(new Response("not found", { status: 404 }));
  },
};

async function register(request, env) {
  const body = await readJson(request);
  if (body.error) return body.error;
  const account = checkRegister(body.value);
  if (account.error) return Response.json({ error: account.error }, { status: 400 });
  const database = await accounts(env);
  if (database.error) return database.error;
  const salt = newSalt();
  const passwordHash = await hashPassword(account.password, salt);
  try {
    await database.users.insertOne({
      email: account.email,
      username: account.username,
      passwordSalt: hex(salt),
      passwordHash,
      dob: account.dob,
      createdAt: new Date(),
    });
  } catch (err) {
    if (err && err.code === 11000) {
      const keys = Object.keys((err && err.keyPattern) || {});
      const message = String(err.message || "");
      const emailTaken = keys.includes("email") || message.includes("email");
      const error = emailTaken ? "That email is already registered." : "That username is taken.";
      return Response.json({ error }, { status: 409 });
    }
    return Response.json({ error: "The account could not be saved." }, { status: 500 });
  }
  const opened = await openSession(database, account);
  return Response.json(opened, { status: 201 });
}

async function login(request, env) {
  const body = await readJson(request);
  if (body.error) return body.error;
  const account = checkLogin(body.value);
  if (account.error) return Response.json({ error: account.error }, { status: 400 });
  const database = await accounts(env);
  if (database.error) return database.error;
  const user = account.email
    ? await database.users.findOne({ email: account.login })
    : await database.users.findOne({ username: account.login }, { collation: { locale: "en", strength: 2 } });
  const salt = user?.passwordSalt || "00".repeat(16);
  const expected = user?.passwordHash || "";
  const matches = await passwordMatches(account.password, salt, expected);
  if (!user || !matches) {
    return Response.json({ error: "Email, username, or password is wrong." }, { status: 401 });
  }
  const opened = await openSession(database, user);
  return Response.json(opened);
}

async function session(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  const body = await readJson(request);
  if (body.error) return body.error;
  const user = await userForRequest(request, database, body);
  if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
  await markSeen(database, user.username);
  return Response.json({
    username: user.username,
    email: user.email,
    session: bearer(request) || String(body.value.session ?? ""),
  });
}

async function here(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  const body = await readJson(request);
  if (body.error) return body.error;
  const user = await userForRequest(request, database, body);
  if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
  await markSeen(database, user.username);
  return Response.json({ username: user.username, email: "", session: "" });
}

async function people(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  const body = await readJson(request);
  if (body.error) return body.error;
  const user = await userForRequest(request, database, body);
  if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
  await markSeen(database, user.username);
  const query = String(body.value.query ?? "").trim().replace(/^@/, "").slice(0, 32);
  if (query.length < 2) return Response.json({ people: [] });
  const pattern = `^${query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`;
  const since = new Date(Date.now() - 90_000);
  const found = await database.users
    .find(
      { username: { $regex: pattern, $options: "i" }, lastSeen: { $gt: since } },
      { projection: { username: 1, _id: 0 }, limit: 8 },
    )
    .toArray();
  const names = found
    .map((item) => item.username)
    .filter((name) => name && name.toLowerCase() !== user.username.toLowerCase());
  return Response.json({ people: names });
}

async function markSeen(database, username) {
  await database.users.updateOne({ username }, { $set: { lastSeen: new Date() } });
}

async function logout(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  const token = bearer(request);
  if (token) {
    const tokenHash = await hashSecret(token);
    await database.sessions.deleteOne({ tokenHash });
  }
  return Response.json({ username: "", email: "", session: "" });
}

async function openSession(database, user) {
  const session = newSecret();
  const tokenHash = await hashSecret(session);
  const expiresAt = new Date(Date.now() + SESSION_DAYS * 24 * 60 * 60 * 1000);
  await database.sessions.insertOne({
    tokenHash,
    username: user.username,
    email: user.email,
    expiresAt,
  });
  return { username: user.username, email: user.email, session };
}

async function userForRequest(request, database, body) {
  const token = bearer(request) || String(body.value?.session ?? "").trim();
  if (!token) return null;
  const tokenHash = await hashSecret(token);
  const found = await database.sessions.findOne({ tokenHash, expiresAt: { $gt: new Date() } });
  if (!found) return null;
  return { username: found.username, email: found.email };
}

function bearer(request) {
  const header = request.headers.get("authorization") || "";
  return header.toLowerCase().startsWith("bearer ") ? header.slice(7).trim() : "";
}

async function accounts(env) {
  if (!env.MONGODB_URI) {
    return { error: Response.json({ error: "Accounts are not configured." }, { status: 500 }) };
  }
  try {
    if (!globalThis.__mycordMongo) {
      globalThis.__mycordMongo = new MongoClient(env.MONGODB_URI, {
        serverSelectionTimeoutMS: 8000,
        family: 4,
      });
    }
    await globalThis.__mycordMongo.connect();
    const database = globalThis.__mycordMongo.db(databaseName(env.MONGODB_URI));
    if (!globalThis.__mycordIndexes) {
      globalThis.__mycordIndexes = Promise.all([
        database.collection("users").createIndex({ email: 1 }, { unique: true }),
        database.collection("users").createIndex(
          { username: 1 },
          { unique: true, collation: { locale: "en", strength: 2 } },
        ),
        database.collection("sessions").createIndex({ tokenHash: 1 }, { unique: true }),
        database.collection("sessions").createIndex({ expiresAt: 1 }, { expireAfterSeconds: 0 }),
      ]);
    }
    await globalThis.__mycordIndexes;
    return {
      users: database.collection("users"),
      sessions: database.collection("sessions"),
    };
  } catch {
    return { error: Response.json({ error: "The account service is unavailable." }, { status: 500 }) };
  }
}

async function issueToken(request, env) {
  let body;
  try {
    body = await request.json();
  } catch {
    return Response.json({ error: "Invalid payload" }, { status: 400 });
  }
  const roomName = String(body.roomName ?? "");
  let identity = String(body.identity ?? "");
  const invite = body.invite ? String(body.invite) : "";
  const presented = bearer(request);
  if (presented) {
    const database = await accounts(env);
    if (database.error) return database.error;
    const user = await userForRequest(request, database, { value: {} });
    if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
    identity = user.username;
  }
  if (!/^[a-zA-Z0-9_-]{2,64}$/.test(roomName) || !/^[a-zA-Z0-9_-]{2,64}$/.test(identity)) {
    return Response.json({ error: "Invalid payload" }, { status: 400 });
  }
  if (!inviteAllowed(env, invite, roomName)) {
    return Response.json({ error: "Invite is not valid for this room" }, { status: 401 });
  }
  if (!env.LIVEKIT_API_KEY || !env.LIVEKIT_API_SECRET || !env.LIVEKIT_URL) {
    return Response.json({ error: "LiveKit is not configured" }, { status: 500 });
  }
  const now = Math.floor(Date.now() / 1000);
  const payload = {
    iss: env.LIVEKIT_API_KEY,
    sub: identity,
    nbf: now,
    exp: now + 6 * 60 * 60,
    video: {
      roomJoin: true,
      room: roomName,
      canPublish: true,
      canSubscribe: true,
      canPublishData: true,
    },
  };
  const token = await signJwt(payload, env.LIVEKIT_API_SECRET);
  return Response.json({
    token,
    url: env.LIVEKIT_URL,
    relay: null,
    roomName,
    identity,
  });
}

function inviteAllowed(env, invite, roomName) {
  const required = env.INVITE_REQUIRED === "true";
  const rules = String(env.INVITES ?? "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
  if (!invite) return !required;
  return rules.some((rule) => {
    const [code, room] = rule.split(":");
    return code === invite && room === roomName;
  });
}

async function readJson(request) {
  try {
    return { value: await request.json() };
  } catch {
    return { error: Response.json({ error: "Invalid payload" }, { status: 400 }) };
  }
}

function hex(bytes) {
  let text = "";
  for (const byte of bytes) text += byte.toString(16).padStart(2, "0");
  return text;
}

async function signJwt(payload, secret) {
  const header = base64url(JSON.stringify({ alg: "HS256", typ: "JWT" }));
  const body = base64url(JSON.stringify(payload));
  const data = encoder.encode(`${header}.${body}`);
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign("HMAC", key, data);
  const bytes = Array.from(new Uint8Array(signature));
  const signed = base64urlBytes(bytes);
  return `${header}.${body}.${signed}`;
}

function base64url(text) {
  return base64urlBytes(Array.from(encoder.encode(text)));
}

function base64urlBytes(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

function cors(response) {
  const headers = new Headers(response.headers);
  headers.set("access-control-allow-origin", "*");
  headers.set("access-control-allow-headers", "content-type, authorization");
  headers.set("access-control-allow-methods", "GET,POST,OPTIONS");
  return new Response(response.body, { status: response.status, headers });
}
