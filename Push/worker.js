// Mailstream's push relay. Gmail (through a Cloud Pub/Sub push subscription) and Microsoft Graph
// (change notifications) POST here; every app connected to /socket hears about each change and
// syncs, and registered mobile devices receive a data-only high-priority FCM wake ("tickle")
// with zero message content so they can wake from Doze and fetch straight from Gmail/Graph
// (MS-443).
//
// Secrets (wrangler secret put): APP_TOKEN authenticates the app's socket and device registration
// endpoints, GRAPH_CLIENT_STATE is the clientState of the app's Graph subscriptions, PUBSUB_TOKEN
// is the query token of the Pub/Sub push endpoint, and optional FCM_SERVICE_ACCOUNT is the JSON
// key of a GCP service account with Firebase Cloud Messaging permission.

import { DurableObject } from "cloudflare:workers";
import {
  DEVICE_STALE_MS,
  FcmTokenCache,
  MAX_DEVICES,
  dispatchFcmWakes,
  normalizeDevicePayload,
  parseServiceAccount,
  validDeviceId,
} from "./fcm.js";

const DEVICE_ROUTE = /^\/devices\/([^/]+)$/;

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/socket") {
      if (request.headers.get("Upgrade") !== "websocket") return new Response("Expected a WebSocket", { status: 426 });
      if (!authorized(request, env)) return new Response(null, { status: 401 });
      return hub(env).fetch(request);
    }
    if (url.pathname === "/graph") {
      return graph(request, url, env, ctx);
    }
    if (url.pathname === "/gmail") {
      return gmail(request, url, env, ctx);
    }
    if (url.pathname === "/wake") {
      if (request.method !== "POST") return new Response(null, { status: 405 });
      if (!authorized(request, env)) return new Response(null, { status: 401 });
      const body = await request.json().catch(() => null);
      const source = typeof body?.source === "string" && body.source.trim() ? body.source.trim() : "mail";
      const account = typeof body?.account === "string" ? body.account.trim().toLowerCase() : "";
      ctx.waitUntil(broadcast(env, [{ source, account }]));
      return new Response(null, { status: 202 });
    }
    const deviceMatch = url.pathname.match(DEVICE_ROUTE);
    if (deviceMatch) {
      if (!authorized(request, env)) return new Response(null, { status: 401 });
      const deviceId = decodeURIComponent(deviceMatch[1]);
      if (!validDeviceId(deviceId)) return new Response(null, { status: 400 });
      if (request.method !== "PUT" && request.method !== "DELETE") {
        return new Response(null, { status: 405 });
      }
      return hub(env).fetch(request);
    }
    return new Response(null, { status: 404 });
  },
};

function authorized(request, env) {
  return equal(request.headers.get("Authorization"), `Bearer ${env.APP_TOKEN}`);
}

/// Graph validates a subscription's endpoint by expecting its token echoed within 10 s, and
/// retries notifications not acknowledged within 3 s, so relaying happens after the reply.
async function graph(request, url, env, ctx) {
  const token = url.searchParams.get("validationToken");
  if (token !== null) return new Response(token, { headers: { "Content-Type": "text/plain" } });
  if (request.method !== "POST") return new Response(null, { status: 405 });
  const body = await request.json().catch(() => null);
  const events = (body?.value ?? [])
    .filter((n) => equal(n.clientState, env.GRAPH_CLIENT_STATE))
    .map((n) => ({
      source: "graph",
      subscriptionId: n.subscriptionId,
      changeType: n.changeType ?? null,
      lifecycleEvent: n.lifecycleEvent ?? null,
    }));
  if (events.length) ctx.waitUntil(broadcast(env, events));
  return new Response(null, { status: 202 });
}

/// A Pub/Sub push: `message.data` is base64 JSON `{emailAddress, historyId}` from Gmail's watch.
async function gmail(request, url, env, ctx) {
  if (request.method !== "POST" || !equal(url.searchParams.get("token"), env.PUBSUB_TOKEN)) {
    return new Response(null, { status: 401 });
  }
  const body = await request.json().catch(() => null);
  let event = null;
  try {
    const data = JSON.parse(atob(body?.message?.data ?? ""));
    event = { source: "gmail", email: String(data.emailAddress).toLowerCase(), historyId: String(data.historyId) };
  } catch {
    // Malformed pushes are acknowledged so Pub/Sub stops redelivering them.
  }
  if (event) ctx.waitUntil(broadcast(env, [event]));
  return new Response(null, { status: 204 });
}

function hub(env) {
  return env.HUB.get(env.HUB.idFromName("hub"));
}

function broadcast(env, events) {
  return hub(env).fetch("https://hub/broadcast", { method: "POST", body: JSON.stringify(events) });
}

/// Constant-time comparison for secrets.
function equal(a, b) {
  if (typeof a !== "string" || typeof b !== "string" || !b) return false;
  const x = new TextEncoder().encode(a);
  const y = new TextEncoder().encode(b);
  if (x.length !== y.length) return false;
  let diff = 0;
  for (let i = 0; i < x.length; i++) diff |= x[i] ^ y[i];
  return diff === 0;
}

/// Holds the apps' WebSockets and mobile FCM device registrations. Hibernation keeps idle sockets
/// open without the object running; the app's "ping" is answered without waking it.
export class Hub extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.env = env;
    this.sql = ctx.storage.sql;
    this.sql.exec(
      "CREATE TABLE IF NOT EXISTS fcm_devices (" +
        "device_id TEXT PRIMARY KEY, " +
        "fcm_token TEXT NOT NULL, " +
        "accounts TEXT NOT NULL, " +
        "platform TEXT NOT NULL, " +
        "updated INTEGER NOT NULL" +
        ");",
    );
    this.fcmTokenCache = new FcmTokenCache();
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request) {
    if (request.headers.get("Upgrade") === "websocket") {
      const [client, server] = Object.values(new WebSocketPair());
      this.ctx.acceptWebSocket(server);
      return new Response(null, { status: 101, webSocket: client });
    }
    const url = new URL(request.url);
    const deviceMatch = url.pathname.match(DEVICE_ROUTE);
    if (deviceMatch) {
      const deviceId = decodeURIComponent(deviceMatch[1]);
      if (request.method === "DELETE") {
        this.sql.exec("DELETE FROM fcm_devices WHERE device_id = ?", deviceId);
        return new Response(null, { status: 204 });
      }
      if (request.method === "PUT") {
        const body = await request.json().catch(() => null);
        const normalized = normalizeDevicePayload(body);
        if (!normalized) return new Response(null, { status: 400 });
        const now = Date.now();
        this.sql.exec("DELETE FROM fcm_devices WHERE updated < ?", now - DEVICE_STALE_MS);
        const known = this.sql.exec("SELECT 1 FROM fcm_devices WHERE device_id = ?", deviceId).toArray().length > 0;
        const count = this.sql.exec("SELECT COUNT(*) AS count FROM fcm_devices").one().count;
        if (!known && count >= MAX_DEVICES) {
          return new Response(null, { status: 409 });
        }
        this.sql.exec(
          "INSERT INTO fcm_devices (device_id, fcm_token, accounts, platform, updated) VALUES (?, ?, ?, ?, ?) " +
            "ON CONFLICT (device_id) DO UPDATE SET fcm_token = excluded.fcm_token, accounts = excluded.accounts, " +
            "platform = excluded.platform, updated = excluded.updated",
          deviceId,
          normalized.fcmToken,
          JSON.stringify(normalized.accounts),
          normalized.platform,
          now,
        );
        return new Response(null, { status: 204 });
      }
      return new Response(null, { status: 405 });
    }

    const events = await request.json();
    const messages = events.map((event) => JSON.stringify(event));
    for (const socket of this.ctx.getWebSockets()) {
      for (const message of messages) {
        try {
          socket.send(message);
        } catch {
          // A socket closing mid-broadcast misses this event; the app syncs when it reconnects.
        }
      }
    }
    this.ctx.waitUntil(this.sendFcm(events));
    return new Response(null, { status: 204 });
  }

  async sendFcm(events) {
    const sa = parseServiceAccount(this.env?.FCM_SERVICE_ACCOUNT);
    if (!sa) return;
    const now = Date.now();
    this.sql.exec("DELETE FROM fcm_devices WHERE updated < ?", now - DEVICE_STALE_MS);
    const devices = this.sql
      .exec("SELECT device_id, fcm_token, accounts, platform FROM fcm_devices")
      .toArray()
      .map((row) => ({
        deviceId: row.device_id,
        fcmToken: row.fcm_token,
        platform: row.platform,
        accounts: parseAccounts(row.accounts),
      }));
    if (devices.length === 0) return;
    try {
      const { deadDeviceIds } = await dispatchFcmWakes(sa, this.fcmTokenCache, devices, events);
      for (const id of deadDeviceIds) {
        this.sql.exec("DELETE FROM fcm_devices WHERE device_id = ?", id);
      }
    } catch {
      // Transient FCM/OAuth failures must not affect WebSocket delivery.
    }
  }

  async webSocketClose(socket) {
    // The peer's code may be one a server can't send (1005, 1006), so close normally.
    try {
      socket.close(1000, "closing");
    } catch {
      // Already closed.
    }
  }
}

function parseAccounts(raw) {
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}
