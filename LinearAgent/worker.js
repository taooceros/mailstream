// Linear agent inbox (MS-377): receives the `linux-omp` app's Agent session webhooks, which Linear
// sends when someone delegates an issue to the app or mentions it, and keeps them until the
// Linux machine, which is often off and not reachable from outside, pulls them with
// `scripts/linear-agent.py`. Linear retries a failed delivery only 3 times (1 min, 1 h, 6 h), so
// the machine can't receive them itself.
//
//   POST   /webhook           Linear's webhook. Checks `Linear-Signature` (HMAC-SHA256 of the raw
//                             body with LINEAR_WEBHOOK_SECRET) and that the body's webhookTimestamp
//                             is within a minute, then stores AgentSessionEvent payloads under
//                             their `Linear-Delivery` id (a redelivery is stored once). On a
//                             `created` event it posts a `thought` activity right away with the
//                             token the machine left here, since Linear marks a session
//                             unresponsive without an activity within 10 seconds.
//   GET    /v1/events         {"events": [{"id", "received", "payload"}]}: what waits, oldest first.
//   DELETE /v1/events/<id>    the machine handled it, 204. Its id is kept a week so a late
//                             redelivery is not stored again.
//   PUT    /v1/token          the machine's current Linear access token (body), for the thought.
// /v1 takes `Authorization: Bearer <PULL_TOKEN>`. Both secrets are Worker secrets that
// `deploy.py` puts in place; nothing secret is in this file.
//
// One SQLite Durable Object holds everything (the Workers Free plan has them).
// Deploy: `python3 LinearAgent/deploy.py` (its header has the steps).

import { DurableObject } from "cloudflare:workers";

/// Linear's agent session payloads carry the issue description and comments.
const MAX_BODY = 1024 * 1024;
/// Events waiting at once; past this the webhook answers 503 and Linear retries.
const MAX_EVENTS = 1000;
/// Linear's recommended replay window for webhookTimestamp.
const SKEW_MS = 60 * 1000;
/// Handled ids are remembered this long; Linear's last retry comes 6 h after the first try.
const KEEP_DONE_MS = 7 * 24 * 3600 * 1000;
const ID = /^[0-9A-Za-z-]{1,64}$/;
const ACK = "[linear-agent worker] 收到。Linux 机器会拉取这个任务并交给一个 agent；机器关机时，开机后接着做。";

export default {
  async fetch(request, env, ctx) {
    const { pathname } = new URL(request.url);
    const inbox = env.INBOX.get(env.INBOX.idFromName("inbox"));
    if (pathname === "/webhook") {
      if (request.method !== "POST") return status(405);
      return webhook(request, env, ctx, inbox);
    }
    if (!pathname.startsWith("/v1/")) return status(404);
    if (!env.PULL_TOKEN) return status(503);
    if (!(await authorized(request, env.PULL_TOKEN))) return status(401);
    if (pathname === "/v1/events") {
      if (request.method !== "GET") return status(405);
      const events = (await inbox.list()).map(({ id, received, body }) => ({ id, received, payload: JSON.parse(body) }));
      return Response.json({ events });
    }
    if (pathname === "/v1/token") {
      if (request.method !== "PUT") return status(405);
      const token = (await request.text()).trim();
      if (!/^[\x21-\x7e]{1,512}$/.test(token)) return status(400);
      await inbox.setToken(token);
      return status(204);
    }
    const event = pathname.match(/^\/v1\/events\/([^/]+)$/);
    if (event && ID.test(event[1])) {
      if (request.method !== "DELETE") return status(405);
      await inbox.remove(event[1]);
      return status(204);
    }
    return status(404);
  },
};

async function webhook(request, env, ctx, inbox) {
  if (!env.LINEAR_WEBHOOK_SECRET) return status(503);
  if (Number(request.headers.get("Content-Length") ?? 0) > MAX_BODY) return status(413);
  const raw = await request.arrayBuffer();
  if (raw.byteLength > MAX_BODY) return status(413);
  const signature = request.headers.get("Linear-Signature") ?? "";
  if (!equal(await hmac(env.LINEAR_WEBHOOK_SECRET, raw), signature.toLowerCase())) return status(401);
  let payload;
  try {
    payload = JSON.parse(new TextDecoder().decode(raw));
  } catch {
    return status(400);
  }
  const sent = payload?.webhookTimestamp;
  if (typeof sent !== "number" || Math.abs(Date.now() - sent) > SKEW_MS) return status(401);
  // Other categories (Inbox notifications, Permission changes) are not used yet.
  if (payload.type !== "AgentSessionEvent") return status(200);
  const delivery = request.headers.get("Linear-Delivery") ?? "";
  const id = ID.test(delivery) ? delivery : hex(await crypto.subtle.digest("SHA-256", raw));
  const { stored, full, token } = await inbox.add(id, new TextDecoder().decode(raw), Date.now());
  if (full) return status(503);
  const session = payload.agentSession?.id;
  if (stored && payload.action === "created" && token && session) ctx.waitUntil(acknowledge(token, session));
  return status(200);
}

/// Best effort: without a token, or with a stale one, the machine acknowledges when it pulls.
async function acknowledge(token, session) {
  const query = "mutation($input: AgentActivityCreateInput!) { agentActivityCreate(input: $input) { success } }";
  const variables = { input: { agentSessionId: session, content: { type: "thought", body: ACK } } };
  try {
    const response = await fetch("https://api.linear.app/graphql", {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ query, variables }),
    });
    const out = await response.json();
    if (!out?.data?.agentActivityCreate?.success) console.log("acknowledge failed", JSON.stringify(out?.errors ?? out));
  } catch (error) {
    console.log("acknowledge failed", String(error));
  }
}

async function authorized(request, secret) {
  const presented = request.headers.get("Authorization")?.match(/^Bearer (\S+)$/)?.[1];
  if (!presented) return false;
  const [a, b] = await Promise.all([presented, secret].map((text) => crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))));
  return crypto.subtle.timingSafeEqual(a, b);
}

async function hmac(secret, data) {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return hex(await crypto.subtle.sign("HMAC", key, data));
}

function equal(a, b) {
  const [x, y] = [a, b].map((text) => new TextEncoder().encode(text));
  return x.byteLength === y.byteLength && crypto.subtle.timingSafeEqual(x, y);
}

function hex(buffer) {
  return [...new Uint8Array(buffer)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function status(code) {
  return new Response(null, { status: code });
}

export class Inbox extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec("CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, received INTEGER NOT NULL, body TEXT NOT NULL, done INTEGER)");
    this.sql.exec("CREATE TABLE IF NOT EXISTS token (id INTEGER PRIMARY KEY CHECK (id = 0), value TEXT NOT NULL)");
  }

  add(id, body, now) {
    this.sql.exec("DELETE FROM events WHERE done < ?", now - KEEP_DONE_MS);
    const token = this.sql.exec("SELECT value FROM token").toArray()[0]?.value ?? null;
    if (this.sql.exec("SELECT 1 FROM events WHERE id = ?", id).toArray().length) return { stored: false, full: false, token };
    if (this.sql.exec("SELECT COUNT(*) AS n FROM events WHERE done IS NULL").one().n >= MAX_EVENTS) return { stored: false, full: true, token };
    this.sql.exec("INSERT INTO events (id, received, body) VALUES (?, ?, ?)", id, now, body);
    return { stored: true, full: false, token };
  }

  list() {
    return this.sql.exec("SELECT id, received, body FROM events WHERE done IS NULL ORDER BY received, id").toArray();
  }

  remove(id) {
    this.sql.exec("UPDATE events SET done = ? WHERE id = ? AND done IS NULL", Date.now(), id);
  }

  setToken(value) {
    this.sql.exec("INSERT INTO token (id, value) VALUES (0, ?) ON CONFLICT (id) DO UPDATE SET value = excluded.value", value);
  }
}
