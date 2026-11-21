// Pure FCM HTTP v1 silent wake-up helpers used by Push/worker.js (MS-443).
// Kept free of `cloudflare:workers` imports so Node unit tests can exercise every path directly.

const FCM_SCOPE = "https://www.googleapis.com/auth/firebase.messaging";
const OAUTH_TOKEN_URL = "https://oauth2.googleapis.com/token";
const TOKEN_REFRESH_MARGIN_MS = 5 * 60 * 1000;
const DEVICE_ID_RE = /^[A-Za-z0-9._:-]{1,128}$/;

export const MAX_DEVICES = 64;
export const DEVICE_STALE_MS = 90 * 24 * 3600 * 1000;

export function validDeviceId(id) {
  return typeof id === "string" && DEVICE_ID_RE.test(id);
}

/// Normalizes a device registration payload from `PUT /devices/<deviceId>`.
export function normalizeDevicePayload(body) {
  if (!body || typeof body !== "object") return null;
  const fcmToken = typeof body.fcmToken === "string" ? body.fcmToken.trim() : "";
  if (!fcmToken || fcmToken.length > 4096) return null;
  const platform = typeof body.platform === "string" && body.platform.trim() ? body.platform.trim().slice(0, 32) : "android";
  const rawAccounts = Array.isArray(body.accounts) ? body.accounts : [];
  const accounts = [...new Set(
    rawAccounts
      .filter((a) => typeof a === "string")
      .map((a) => a.trim().toLowerCase())
      .filter((a) => a.length > 0 && a.length <= 320),
  )].slice(0, 64);
  return { fcmToken, platform, accounts };
}

/// Parses `env.FCM_SERVICE_ACCOUNT` JSON (GCP service account key with FCM Messaging permissions).
export function parseServiceAccount(raw) {
  if (typeof raw !== "string" || !raw.trim()) return null;
  try {
    const parsed = JSON.parse(raw);
    const projectId = typeof parsed?.project_id === "string" ? parsed.project_id.trim() : "";
    const clientEmail = typeof parsed?.client_email === "string" ? parsed.client_email.trim() : "";
    const privateKey = typeof parsed?.private_key === "string" ? parsed.private_key.trim() : "";
    if (!projectId || !clientEmail || !privateKey) return null;
    return { projectId, clientEmail, privateKey };
  } catch {
    return null;
  }
}

/// Decodes a PEM-encoded PKCS#8 private key into a raw DER ArrayBuffer.
export function pemToDer(pem) {
  const cleaned = pem
    .replace(/-----BEGIN PRIVATE KEY-----/g, "")
    .replace(/-----END PRIVATE KEY-----/g, "")
    .replace(/\s+/g, "");
  const bin = atob(cleaned);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes.buffer;
}

export function base64UrlEncode(input) {
  const bytes = typeof input === "string" ? new TextEncoder().encode(input) : new Uint8Array(input);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

/// Signs an RS256 Google OAuth2 service-account JWT using standard Web Crypto.
export async function signServiceAccountJwt(sa, nowSeconds = Math.floor(Date.now() / 1000)) {
  const header = base64UrlEncode(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const claims = base64UrlEncode(
    JSON.stringify({
      iss: sa.clientEmail,
      scope: FCM_SCOPE,
      aud: OAUTH_TOKEN_URL,
      iat: nowSeconds,
      exp: nowSeconds + 3600,
    }),
  );
  const signingInput = `${header}.${claims}`;
  const key = await crypto.subtle.importKey(
    "pkcs8",
    pemToDer(sa.privateKey),
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, new TextEncoder().encode(signingInput));
  return `${signingInput}.${base64UrlEncode(signature)}`;
}

/// Caches the short-lived Google OAuth2 access token used for FCM HTTP v1 calls.
export class FcmTokenCache {
  constructor(fetchImpl = fetch) {
    this.fetch = fetchImpl;
    this.token = null;
    this.expiresAt = 0;
  }

  async getAccessToken(sa, nowMs = Date.now()) {
    if (this.token && nowMs + TOKEN_REFRESH_MARGIN_MS < this.expiresAt) {
      return this.token;
    }
    const assertion = await signServiceAccountJwt(sa, Math.floor(nowMs / 1000));
    const resp = await this.fetch(OAUTH_TOKEN_URL, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
        assertion,
      }).toString(),
    });
    if (!resp.ok) {
      throw new Error(`FCM OAuth token exchange failed (${resp.status})`);
    }
    const json = await resp.json();
    if (!json?.access_token) {
      throw new Error("FCM OAuth response missing access_token");
    }
    const ttlSeconds = Number(json.expires_in ?? 3600);
    this.token = String(json.access_token);
    this.expiresAt = nowMs + Math.max(60, ttlSeconds) * 1000;
    return this.token;
  }
}

/// Selects unique `(device, wakePayload)` deliveries for a batch of relay events.
/// Silent tickle only: zero message subjects, bodies, or credentials are included.
export function buildFcmDeliveries(devices, events) {
  const deliveries = [];
  const seen = new Set();

  for (const event of events ?? []) {
    if (!event || typeof event !== "object") continue;
    const source = String(event.source ?? "");
    const account = typeof event.email === "string"
      ? event.email.toLowerCase()
      : typeof event.account === "string"
        ? event.account.toLowerCase()
        : "";
    const subscriptionId = typeof event.subscriptionId === "string" ? event.subscriptionId : "";
    const historyId = typeof event.historyId === "string" ? event.historyId : "";

    for (const device of devices ?? []) {
      if (!device?.fcmToken) continue;
      if (account && Array.isArray(device.accounts) && device.accounts.length > 0 && !device.accounts.includes(account)) {
        continue;
      }
      const dedupKey = `${device.deviceId}|${source}|${account || subscriptionId || "*"}`;
      if (seen.has(dedupKey)) continue;
      seen.add(dedupKey);
      deliveries.push({
        deviceId: device.deviceId,
        fcmToken: device.fcmToken,
        body: {
          message: {
            token: device.fcmToken,
            android: {
              priority: "HIGH",
              ttl: "300s",
              collapse_key: "mailstream_sync",
            },
            data: {
              source,
              account,
              subscriptionId,
              historyId,
            },
          },
        },
      });
    }
  }
  return deliveries;
}

/// True when FCM reports the device's registration token is permanently dead and should be pruned.
export function isDeadFcmToken(status, bodyJson) {
  if (status === 404) return true;
  const details = bodyJson?.error?.details;
  if (Array.isArray(details)) {
    for (const d of details) {
      if (d?.errorCode === "UNREGISTERED") return true;
    }
  }
  if (status === 400 && bodyJson?.error?.status === "INVALID_ARGUMENT") {
    const msg = String(bodyJson?.error?.message ?? "").toLowerCase();
    if (msg.includes("registration token") || msg.includes("not a valid fcm")) return true;
  }
  return false;
}

/// Dispatches silent FCM HTTP v1 wake messages and returns deviceIds whose tokens were pruned.
export async function dispatchFcmWakes(sa, tokenCache, devices, events, fetchImpl = fetch) {
  const deliveries = buildFcmDeliveries(devices, events);
  if (deliveries.length === 0) return { sent: 0, deadDeviceIds: [] };
  const accessToken = await tokenCache.getAccessToken(sa);
  const endpoint = `https://fcm.googleapis.com/v1/projects/${encodeURIComponent(sa.projectId)}/messages:send`;
  const deadDeviceIds = [];
  let sent = 0;

  await Promise.all(
    deliveries.map(async (item) => {
      try {
        const resp = await fetchImpl(endpoint, {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify(item.body),
        });
        if (resp.ok) {
          sent += 1;
          return;
        }
        const errJson = await resp.json().catch(() => null);
        if (isDeadFcmToken(resp.status, errJson)) {
          deadDeviceIds.push(item.deviceId);
        }
      } catch {
        // Transient network failures are ignored: the client catches up on reconnect or next push.
      }
    }),
  );

  return { sent, deadDeviceIds: [...new Set(deadDeviceIds)] };
}
