// Mints a LiveKit token. The picture does not pass through here.

const encoder = new TextEncoder();

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
    return cors(new Response("not found", { status: 404 }));
  },
};

async function issueToken(request, env) {
  let body;
  try {
    body = await request.json();
  } catch {
    return Response.json({ error: "Invalid payload" }, { status: 400 });
  }
  const roomName = String(body.roomName ?? "");
  const identity = String(body.identity ?? "");
  const invite = body.invite ? String(body.invite) : "";
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
  headers.set("access-control-allow-headers", "content-type");
  headers.set("access-control-allow-methods", "GET,POST,OPTIONS");
  return new Response(response.body, { status: response.status, headers });
}
