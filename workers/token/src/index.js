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
    if (request.method === "POST" && url.pathname === "/circle") {
      return cors(await circle(request, env));
    }
    if (request.method === "POST" && url.pathname === "/chat") {
      return cors(await chat(request, env));
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
  try {
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
  } finally {
    await release(database);
  }
}

async function login(request, env) {
  const body = await readJson(request);
  if (body.error) return body.error;
  const account = checkLogin(body.value);
  if (account.error) return Response.json({ error: account.error }, { status: 400 });
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
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
  } finally {
    await release(database);
  }
}

async function session(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
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
  } finally {
    await release(database);
  }
}

async function here(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
  const body = await readJson(request);
  if (body.error) return body.error;
  const user = await userForRequest(request, database, body);
  if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
  await markSeen(database, user.username);
  return Response.json({ username: user.username, email: "", session: "" });
  } finally {
    await release(database);
  }
}

async function people(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
  const body = await readJson(request);
  if (body.error) return body.error;
  const user = await userForRequest(request, database, body);
  if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
  await markSeen(database, user.username);
  const query = String(body.value.query ?? "").trim().replace(/^@/, "").slice(0, 32);
  if (query.length < 2) return Response.json({ people: [] });
  const pattern = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const since = new Date(Date.now() - 3 * 60_000);
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
  } finally {
    await release(database);
  }
}

const NAME = { locale: "en", strength: 2 };

async function circle(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
  const body = await readJson(request);
  if (body.error) return body.error;
  const user = await userForRequest(request, database, body);
  if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
  await markSeen(database, user.username);
  const action = String(body.value.action ?? "list");
  if (action === "list") return Response.json(await loadCircle(database, user.username));
  const other = await namedUser(database, body.value.username);
  if (!other) return Response.json({ error: "No one uses that username." }, { status: 404 });
  if (other.username.toLowerCase() === user.username.toLowerCase()) {
    return Response.json({ error: "You can't add yourself." }, { status: 400 });
  }
  if (action === "ask") {
    const already = await database.friendships.findOne(
      { user: user.username, friend: other.username },
      { collation: NAME, projection: { _id: 1 } },
    );
    if (!already) {
      const reverse = await database.requests.findOne(
        { from: other.username, to: user.username },
        { collation: NAME, projection: { _id: 1 } },
      );
      if (reverse) {
        const saved = await befriend(database, user.username, other.username);
        if (saved) return saved;
      } else {
        try {
          await database.requests.insertOne({ from: user.username, to: other.username, at: new Date() });
        } catch (err) {
          if (!err || err.code !== 11000) return Response.json({ error: "The request could not be saved." }, { status: 500 });
        }
      }
    }
  } else if (action === "accept") {
    const pending = await database.requests.findOne(
      { from: other.username, to: user.username },
      { collation: NAME, projection: { _id: 1 } },
    );
    if (!pending) return Response.json({ error: "That request is gone." }, { status: 404 });
    const saved = await befriend(database, user.username, other.username);
    if (saved) return saved;
  } else if (action === "reject") {
    await database.requests.deleteOne({ from: other.username, to: user.username }, { collation: NAME });
  } else if (action === "cancel") {
    await database.requests.deleteOne({ from: user.username, to: other.username }, { collation: NAME });
  } else {
    return Response.json({ error: "Unknown request." }, { status: 400 });
  }
  return Response.json(await loadCircle(database, user.username));
  } finally {
    await release(database);
  }
}

function pairKey(a, b) {
  return [a.toLowerCase(), b.toLowerCase()].sort().join("|");
}

function noteOf(row) {
  const at = row.at instanceof Date ? row.at.getTime() : Number(row.at) || 0;
  return { id: row.id, author: row.from, to: row.to, text: row.text, at };
}

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function inboxOf(database, me) {
  const friends = await database.friendships
    .find({ user: me }, { projection: { friend: 1, _id: 0 } })
    .limit(40)
    .toArray();
  const rows = [];
  for (const friend of friends) {
    const peer = friend.friend;
    if (!peer) continue;
    const pair = pairKey(me, peer);
    const last = await database.messages.find({ pair }).sort({ at: -1 }).limit(1).next();
    if (!last) continue;
    const read = await database.reads.findOne({ user: me, peer }, { projection: { at: 1, _id: 0 } });
    const since = read?.at instanceof Date ? read.at : new Date(0);
    const unread = await database.messages.countDocuments({ pair, from: peer, at: { $gt: since } });
    rows.push({ name: peer, text: last.text, at: noteOf(last).at, unread, from: last.from });
  }
  return rows;
}

async function messagesAfter(database, pair, after) {
  if (!after) {
    const rows = await database.messages.find({ pair }).sort({ at: -1 }).limit(80).toArray();
    return rows.reverse();
  }
  return database.messages.find({ pair, at: { $gt: new Date(after) } }).sort({ at: 1 }).limit(80).toArray();
}

async function incomingAfter(database, me, after) {
  if (!after) {
    const rows = await database.messages.find({ to: me }).sort({ at: -1 }).limit(1).toArray();
    return rows;
  }
  return database.messages.find({ to: me, at: { $gt: new Date(after) } }).sort({ at: 1 }).limit(40).toArray();
}

async function chat(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
    const body = await readJson(request);
    if (body.error) return body.error;
    const user = await userForRequest(request, database, body);
    if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
    await markSeen(database, user.username);
    const me = user.username;
    const action = String(body.value.action ?? "");
    if (action === "send") {
      const other = await namedUser(database, body.value.username);
      if (!other) return Response.json({ error: "No one uses that username." }, { status: 404 });
      if (other.username.toLowerCase() === me.toLowerCase()) {
        return Response.json({ error: "You can't message yourself." }, { status: 400 });
      }
      const friend = await database.friendships.findOne(
        { user: me, friend: other.username },
        { collation: NAME, projection: { _id: 1 } },
      );
      if (!friend) return Response.json({ error: "Add each other before chatting." }, { status: 403 });
      const text = String(body.value.text ?? "").trim();
      if (!text || text.length > 2000) {
        return Response.json({ error: "That message needs between 1 and 2000 characters." }, { status: 400 });
      }
      const given = String(body.value.id ?? "");
      const id = /^[A-Za-z0-9_-]{8,64}$/.test(given) ? given : newSecret();
      const pair = pairKey(me, other.username);
      let row = await database.messages.findOne({ id });
      if (!row) {
        row = { id, pair, from: me, to: other.username, text, at: new Date() };
        try {
          await database.messages.insertOne(row);
        } catch (err) {
          if (!err || err.code !== 11000) return Response.json({ error: "The message could not be saved." }, { status: 500 });
          row = await database.messages.findOne({ id });
        }
      }
      return Response.json({ messages: row ? [noteOf(row)] : [], inbox: await inboxOf(database, me), full: false });
    }
    if (action === "sync") {
      const wanted = String(body.value.username ?? "").trim();
      let other = null;
      if (wanted) {
        other = await namedUser(database, wanted);
        if (other && other.username.toLowerCase() === me.toLowerCase()) other = null;
        if (other) {
          const friend = await database.friendships.findOne(
            { user: me, friend: other.username },
            { collation: NAME, projection: { _id: 1 } },
          );
          if (!friend) other = null;
        }
      }
      const after = Number(body.value.after) || 0;
      const hold = body.value.wait === true;
      const full = Boolean(other) && !after;
      let rows = [];
      const deadline = Date.now() + (hold ? 5000 : 0);
      const pair = other ? pairKey(me, other.username) : "";
      do {
        rows = pair
          ? await messagesAfter(database, pair, after)
          : await incomingAfter(database, me, after);
        if (rows.length || Date.now() >= deadline) break;
        await wait(320);
      } while (Date.now() < deadline);
      if (other && body.value.read === true) {
        const newest = rows.length ? rows[rows.length - 1].at : new Date();
        try {
          await database.reads.updateOne(
            { user: me, peer: other.username },
            { $set: { at: newest } },
            { upsert: true },
          );
        } catch (err) {
          if (!err || err.code !== 11000) throw err;
        }
      }
      return Response.json({
        messages: rows.map(noteOf),
        inbox: await inboxOf(database, me),
        full,
      });
    }
    return Response.json({ error: "Unknown chat action." }, { status: 400 });
  } finally {
    await release(database);
  }
}

async function namedUser(database, username) {
  const name = String(username ?? "").trim().replace(/^@/, "");
  if (!/^[A-Za-z0-9_-]{2,32}$/.test(name)) return null;
  return database.users.findOne({ username: name }, { collation: NAME, projection: { username: 1 } });
}

async function loadCircle(database, username) {
  const [friends, incoming, outgoing] = await Promise.all([
    database.friendships.find({ user: username }, { collation: NAME, projection: { friend: 1, _id: 0 } }).limit(40).toArray(),
    database.requests.find({ to: username }, { collation: NAME, projection: { from: 1, _id: 0 } }).sort({ at: -1 }).limit(40).toArray(),
    database.requests.find({ from: username }, { collation: NAME, projection: { to: 1, _id: 0 } }).sort({ at: -1 }).limit(40).toArray(),
  ]);
  return {
    friends: friends.map((item) => item.friend).filter(Boolean),
    incoming: incoming.map((item) => item.from).filter(Boolean),
    outgoing: outgoing.map((item) => item.to).filter(Boolean),
  };
}

async function befriend(database, a, b) {
  await database.requests.deleteMany({ $or: [{ from: a, to: b }, { from: b, to: a }] });
  for (const [user, friend] of [[a, b], [b, a]]) {
    try {
      await database.friendships.insertOne({ user, friend, at: new Date() });
    } catch (err) {
      if (!err || err.code !== 11000) return Response.json({ error: "The friend could not be saved." }, { status: 500 });
    }
  }
  return null;
}

async function markSeen(database, username) {
  await database.users.updateOne(
    { username },
    { $set: { lastSeen: new Date() } },
    { collation: { locale: "en", strength: 2 } },
  );
}

async function logout(request, env) {
  const database = await accounts(env);
  if (database.error) return database.error;
  try {
  const token = bearer(request);
  if (token) {
    const tokenHash = await hashSecret(token);
    await database.sessions.deleteOne({ tokenHash });
  }
  return Response.json({ username: "", email: "", session: "" });
  } finally {
    await release(database);
  }
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
  await markSeen(database, user.username);
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
  const client = new MongoClient(env.MONGODB_URI, {
    serverSelectionTimeoutMS: 5000,
    connectTimeoutMS: 5000,
    socketTimeoutMS: 20000,
    maxPoolSize: 1,
    family: 4,
  });
  try {
    await client.connect();
    const database = client.db(databaseName(env.MONGODB_URI));
    if (!globalThis.__mycordReady) {
      await Promise.all([
        database.collection("users").createIndex({ email: 1 }, { unique: true }),
        database.collection("users").createIndex(
          { username: 1 },
          { unique: true, collation: { locale: "en", strength: 2 } },
        ),
        database.collection("sessions").createIndex({ tokenHash: 1 }, { unique: true }),
        database.collection("sessions").createIndex({ expiresAt: 1 }, { expireAfterSeconds: 0 }),
        database.collection("users").createIndex({ lastSeen: -1 }),
        database.collection("requests").createIndex(
          { from: 1, to: 1 },
          { unique: true, collation: NAME },
        ),
        database.collection("friendships").createIndex(
          { user: 1, friend: 1 },
          { unique: true, collation: NAME },
        ),
        database.collection("messages").createIndex({ pair: 1, at: 1 }),
        database.collection("messages").createIndex({ to: 1, at: 1 }),
        database.collection("messages").createIndex({ id: 1 }, { unique: true }),
        database.collection("reads").createIndex({ user: 1, peer: 1 }, { unique: true }),
      ]);
      globalThis.__mycordReady = true;
    }
    return {
      users: database.collection("users"),
      sessions: database.collection("sessions"),
      requests: database.collection("requests"),
      friendships: database.collection("friendships"),
      messages: database.collection("messages"),
      reads: database.collection("reads"),
      close: () => client.close(),
    };
  } catch {
    globalThis.__mycordReady = false;
    try { await client.close(); } catch { /* already failed */ }
    return { error: Response.json({ error: "The account service is unavailable." }, { status: 500 }) };
  }
}

async function release(database) {
  if (!database?.close) return;
  try {
    await Promise.race([
      database.close(),
      new Promise((resolve) => setTimeout(resolve, 400)),
    ]);
  } catch {
    // A finished answer matters more than closing the socket.
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
  let database;
  if (presented) {
    database = await accounts(env);
    if (database.error) return database.error;
    try {
      const user = await userForRequest(request, database, { value: {} });
      if (!user) return Response.json({ error: "Sign in again." }, { status: 401 });
      identity = user.username;
    } finally {
      await release(database);
    }
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
