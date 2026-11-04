// Mailstream's push relay. Gmail (through a Cloud Pub/Sub push subscription) and Microsoft Graph
// (change notifications) POST here; every app connected to /socket hears about each change and
// syncs. Nothing is stored: notifications carry ids only and are dropped when no app listens.
//
// Secrets (wrangler secret put): APP_TOKEN authenticates the app's socket, GRAPH_CLIENT_STATE
// is the clientState of the app's Graph subscriptions, PUBSUB_TOKEN is the query token of the
// Pub/Sub push endpoint.

import { DurableObject } from "cloudflare:workers";

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    switch (url.pathname) {
      case "/socket":
        if (request.headers.get("Upgrade") !== "websocket") return new Response("Expected a WebSocket", { status: 426 });
        if (!equal(request.headers.get("Authorization"), `Bearer ${env.APP_TOKEN}`)) return new Response(null, { status: 401 });
        return hub(env).fetch(request);
      case "/graph":
        return graph(request, url, env, ctx);
      case "/gmail":
        return gmail(request, url, env, ctx);
      default:
        return new Response(null, { status: 404 });
    }
  },
};

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

/// Holds the apps' sockets. Hibernation keeps idle sockets open without the object running;
/// the app's "ping" is answered without waking it.
export class Hub extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request) {
    if (request.headers.get("Upgrade") === "websocket") {
      const [client, server] = Object.values(new WebSocketPair());
      this.ctx.acceptWebSocket(server);
      return new Response(null, { status: 101, webSocket: client });
    }
    const messages = (await request.json()).map((event) => JSON.stringify(event));
    for (const socket of this.ctx.getWebSockets()) {
      for (const message of messages) {
        try {
          socket.send(message);
        } catch {
          // A socket closing mid-broadcast misses this event; the app syncs when it reconnects.
        }
      }
    }
    return new Response(null, { status: 204 });
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
