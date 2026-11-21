import test from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";

import {
  FcmTokenCache,
  buildFcmDeliveries,
  dispatchFcmWakes,
  isDeadFcmToken,
  normalizeDevicePayload,
  parseServiceAccount,
  signServiceAccountJwt,
  validDeviceId,
} from "./fcm.js";

function makeTestServiceAccount() {
  const { privateKey } = generateKeyPairSync("rsa", {
    modulusLength: 2048,
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
    publicKeyEncoding: { type: "spki", format: "pem" },
  });
  return {
    projectId: "mailstream-test",
    clientEmail: "fcm@mailstream-test.iam.gserviceaccount.com",
    privateKey: privateKey.trim(),
  };
}

test("validDeviceId and normalizeDevicePayload validate and sanitize inputs", () => {
  assert.equal(validDeviceId("android-abc_123:def"), true);
  assert.equal(validDeviceId(""), false);
  assert.equal(validDeviceId("../bad"), false);

  assert.equal(normalizeDevicePayload(null), null);
  assert.equal(normalizeDevicePayload({ fcmToken: "   " }), null);
  const normalized = normalizeDevicePayload({
    fcmToken: " tok-123 ",
    accounts: ["Alice@Example.com", "alice@example.com", "", 42, "bob@outlook.com"],
    platform: "android",
  });
  assert.deepEqual(normalized, {
    fcmToken: "tok-123",
    platform: "android",
    accounts: ["alice@example.com", "bob@outlook.com"],
  });
});

test("buildFcmDeliveries filters by account, deduplicates, and carries zero content", () => {
  const devices = [
    { deviceId: "d1", fcmToken: "t1", accounts: ["alice@gmail.com"] },
    { deviceId: "d2", fcmToken: "t2", accounts: ["bob@gmail.com"] },
    { deviceId: "d3", fcmToken: "t3", accounts: [] },
  ];
  const events = [
    { source: "gmail", email: "alice@gmail.com", historyId: "100" },
    { source: "gmail", email: "alice@gmail.com", historyId: "101" },
    { source: "graph", subscriptionId: "sub-1" },
  ];
  const deliveries = buildFcmDeliveries(devices, events);
  // d1 gets gmail(alice) once (deduped) + graph(sub-1); d2 gets graph(sub-1); d3 gets both.
  assert.equal(deliveries.length, 5);
  const d1Gmail = deliveries.find((d) => d.deviceId === "d1" && d.body.message.data.source === "gmail");
  assert.ok(d1Gmail);
  assert.equal(d1Gmail.body.message.android.priority, "HIGH");
  assert.equal(d1Gmail.body.message.notification, undefined);
  assert.deepEqual(d1Gmail.body.message.data, {
    source: "gmail",
    account: "alice@gmail.com",
    subscriptionId: "",
    historyId: "100",
  });
});

test("signServiceAccountJwt, FcmTokenCache, and dispatchFcmWakes prune dead tokens", async () => {
  const sa = makeTestServiceAccount();
  const parsed = parseServiceAccount(
    JSON.stringify({
      project_id: sa.projectId,
      client_email: sa.clientEmail,
      private_key: sa.privateKey,
    }),
  );
  assert.deepEqual(parsed, sa);

  const jwt = await signServiceAccountJwt(sa, 1_700_000_000);
  assert.equal(jwt.split(".").length, 3);

  let oauthCalls = 0;
  const fcmCalls = [];
  const fakeFetch = async (url, init) => {
    if (url.includes("oauth2.googleapis.com")) {
      oauthCalls += 1;
      return new Response(JSON.stringify({ access_token: "ya29.test", expires_in: 3600 }), { status: 200 });
    }
    fcmCalls.push({ url, body: JSON.parse(init.body), auth: init.headers.Authorization });
    const token = JSON.parse(init.body).message.token;
    if (token === "dead-tok") {
      return new Response(
        JSON.stringify({ error: { code: 404, details: [{ errorCode: "UNREGISTERED" }] } }),
        { status: 404 },
      );
    }
    return new Response(JSON.stringify({ name: "projects/mailstream-test/messages/1" }), { status: 200 });
  };

  const cache = new FcmTokenCache(fakeFetch);
  const devices = [
    { deviceId: "good", fcmToken: "good-tok", accounts: ["a@gmail.com"] },
    { deviceId: "dead", fcmToken: "dead-tok", accounts: ["a@gmail.com"] },
  ];
  const res1 = await dispatchFcmWakes(
    sa,
    cache,
    devices,
    [{ source: "gmail", email: "a@gmail.com", historyId: "55" }],
    fakeFetch,
  );
  assert.equal(res1.sent, 1);
  assert.deepEqual(res1.deadDeviceIds, ["dead"]);
  assert.equal(oauthCalls, 1);

  // Second dispatch reuses cached OAuth token.
  const res2 = await dispatchFcmWakes(
    sa,
    cache,
    [devices[0]],
    [{ source: "graph", subscriptionId: "s-9" }],
    fakeFetch,
  );
  assert.equal(res2.sent, 1);
  assert.equal(oauthCalls, 1);
  assert.equal(fcmCalls[0].auth, "Bearer ya29.test");

  assert.equal(isDeadFcmToken(404, null), true);
  assert.equal(isDeadFcmToken(400, { error: { status: "INVALID_ARGUMENT", message: "The registration token is not a valid FCM registration token" } }), true);
  assert.equal(isDeadFcmToken(500, { error: { status: "INTERNAL" } }), false);
});
