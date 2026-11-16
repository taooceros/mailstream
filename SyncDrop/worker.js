// Device sync's offline drop (MS-374): for devices of one sync group that are never online at the
// same time and have no always-on node between them. Each device uploads its copy of the group's
// synced settings here, encrypted with a key derived from the group's secret, and reads the
// other devices' copies when it starts, wakes or changes something. The Worker sees only
// ciphertext under opaque ids: it can't read, and without the group's write token can't
// replace, what a group stores. The engine's side is `Engine/src/cloud_drop.rs`.
//
// One SQLite Durable Object per group (the Workers Free plan has them):
//   GET    /v1/groups/<group>                 the group's copies:
//                                             {"slots": [{"slot", "data" (base64), "updated"}]}
//   PUT    /v1/groups/<group>/slots/<slot>    stores the body (one device's copy), 204
//   DELETE /v1/groups/<group>/slots/<slot>    removes it, 204
// <group> is 64 and <slot> 32 lowercase hex characters, both derived from the group's secret.
// Every request carries `Authorization: Bearer <token>`, also derived from it. The first write
// to a group records the token's SHA-256; from then on only that token reads or writes it.
//
// Deploy: `cd SyncDrop && npx wrangler@4 deploy` (README, Device sync's Cloudflare fallback).

import { DurableObject } from "cloudflare:workers";

const ROUTE = /^\/v1\/groups\/([0-9a-f]{64})(?:\/slots\/([0-9a-f]{32}))?$/;
/// One device's copy: the trusted senders (with tombstones), a few settings and the devices.
const MAX_BODY = 256 * 1024;
/// Devices per group.
const MAX_SLOTS = 64;
/// Copies nobody refreshed for a year are deleted on the group's next write.
const STALE_MS = 365 * 24 * 3600 * 1000;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const route = url.pathname.match(ROUTE);
    if (!route) return new Response(null, { status: 404 });
    const [, group, slot] = route;
    const token = request.headers.get("Authorization")?.match(/^Bearer ([0-9a-f]{64})$/)?.[1];
    if (!token) return new Response(null, { status: 401 });
    const tokenHash = await sha256(token);
    const stub = env.GROUPS.get(env.GROUPS.idFromName(group));
    if (slot === undefined) {
      if (request.method !== "GET") return new Response(null, { status: 405 });
      return answer(await stub.list(tokenHash));
    }
    if (request.method === "PUT") {
      if (Number(request.headers.get("Content-Length") ?? 0) > MAX_BODY) return new Response(null, { status: 413 });
      const data = await request.arrayBuffer();
      if (data.byteLength === 0) return new Response(null, { status: 400 });
      if (data.byteLength > MAX_BODY) return new Response(null, { status: 413 });
      return answer(await stub.put(tokenHash, slot, data));
    }
    if (request.method === "DELETE") return answer(await stub.remove(tokenHash, slot));
    return new Response(null, { status: 405 });
  },
};

function answer({ status, body }) {
  if (body === undefined) return new Response(null, { status });
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

async function sha256(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/// Constant-time comparison for the token hashes.
function equal(a, b) {
  if (typeof a !== "string" || typeof b !== "string" || a.length !== b.length) return false;
  let difference = 0;
  for (let i = 0; i < a.length; i++) difference |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return difference === 0;
}

function base64(buffer) {
  const bytes = new Uint8Array(buffer);
  let text = "";
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(text);
}

/// One sync group: the token hash its first write recorded and each device's copy.
export class Group extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(
      "CREATE TABLE IF NOT EXISTS owner (id INTEGER PRIMARY KEY CHECK (id = 1), token_hash TEXT NOT NULL);" +
        "CREATE TABLE IF NOT EXISTS slots (slot TEXT PRIMARY KEY, data BLOB NOT NULL, updated INTEGER NOT NULL);",
    );
  }

  /// "owner" when `tokenHash` is the group's, "none" before its first write, else "other".
  #caller(tokenHash) {
    const row = this.sql.exec("SELECT token_hash FROM owner").toArray()[0];
    if (!row) return "none";
    return equal(row.token_hash, tokenHash) ? "owner" : "other";
  }

  async list(tokenHash) {
    const caller = this.#caller(tokenHash);
    if (caller === "other") return { status: 403 };
    if (caller === "none") return { status: 200, body: { slots: [] } };
    const slots = this.sql
      .exec("SELECT slot, data, updated FROM slots ORDER BY slot")
      .toArray()
      .map((row) => ({ slot: row.slot, data: base64(row.data), updated: row.updated }));
    return { status: 200, body: { slots } };
  }

  async put(tokenHash, slot, data) {
    const caller = this.#caller(tokenHash);
    if (caller === "other") return { status: 403 };
    if (caller === "none") this.sql.exec("INSERT INTO owner (id, token_hash) VALUES (1, ?)", tokenHash);
    const now = Date.now();
    this.sql.exec("DELETE FROM slots WHERE updated < ?", now - STALE_MS);
    const known = this.sql.exec("SELECT 1 FROM slots WHERE slot = ?", slot).toArray().length > 0;
    const count = this.sql.exec("SELECT COUNT(*) AS count FROM slots").one().count;
    if (!known && count >= MAX_SLOTS) return { status: 409 };
    this.sql.exec(
      "INSERT INTO slots (slot, data, updated) VALUES (?, ?, ?) " +
        "ON CONFLICT (slot) DO UPDATE SET data = excluded.data, updated = excluded.updated",
      slot,
      data,
      now,
    );
    return { status: 204 };
  }

  async remove(tokenHash, slot) {
    const caller = this.#caller(tokenHash);
    if (caller === "other") return { status: 403 };
    if (caller === "owner") this.sql.exec("DELETE FROM slots WHERE slot = ?", slot);
    return { status: 204 };
  }
}
