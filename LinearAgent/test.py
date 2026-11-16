#!/usr/bin/env python3
"""Tests the Linear agent inbox (worker.js) and scripts/linear-agent.py together, without a
Cloudflare account or Linear: runs the Worker under `npx wrangler@4 dev` (local workerd and
Durable Object) from a temporary copy with test secrets, sends it signed and forged webhooks,
then lets linear-agent.py pull from it with Linear's API and the omp run replaced by fakes.
`python3 LinearAgent/test.py`; exits 1 on the first failure.
"""
import hashlib
import hmac
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
SECRET = "test-webhook-secret"
PULL = "test-pull-token"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def call(base, method, path, body=None, headers=None):
    request = urllib.request.Request(base + path, body, headers or {}, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def payload(action, session, creator="u-member", **extra):
    body = {"type": "AgentSessionEvent", "action": action, "webhookTimestamp": int(time.time() * 1000),
            "organizationId": "org", "appUserId": "app", "oauthClientId": "client", "createdAt": "now",
            "webhookId": "hook",
            "agentSession": {"id": session, "creatorId": creator,
                             "issue": {"id": "i1", "identifier": "MS-999", "title": "Test"}},
            "promptContext": '<issue identifier="MS-999"><title>Test</title></issue>'}
    body.update(extra)
    return body


def send(base, body, secret=SECRET, delivery=None):
    raw = json.dumps(body).encode()
    signature = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    headers = {"Content-Type": "application/json", "Linear-Signature": signature,
               "Linear-Delivery": delivery or str(uuid.uuid4()), "Linear-Event": body.get("type", "")}
    return call(base, "POST", "/webhook", raw, headers)[0]


def check(condition, what):
    if not condition:
        sys.exit(f"FAIL: {what}")
    print(f"ok: {what}")


def worker_tests(base):
    auth = {"Authorization": f"Bearer {PULL}"}
    check(call(base, "GET", "/v1/events")[0] == 401, "events need the pull token")
    check(call(base, "GET", "/v1/events", headers={"Authorization": "Bearer wrong"})[0] == 401, "a wrong pull token is refused")
    check(send(base, payload("created", "s0"), secret="forged") == 401, "a forged signature is refused")
    old = payload("created", "s0")
    old["webhookTimestamp"] -= 120_000
    check(send(base, old) == 401, "a webhook older than a minute is refused")
    check(call(base, "POST", "/webhook", b"{}", {"Linear-Signature": "00"})[0] == 401, "a bad signature header is refused")
    other = {"type": "PermissionChange", "action": "teamAccessChanged", "webhookTimestamp": int(time.time() * 1000)}
    check(send(base, other) == 200, "other webhook categories are accepted")
    delivery = str(uuid.uuid4())
    check(send(base, payload("created", "s1"), delivery=delivery) == 200, "a signed created event is accepted")
    check(send(base, payload("created", "s1"), delivery=delivery) == 200, "its redelivery is accepted")
    status, raw = call(base, "GET", "/v1/events", headers=auth)
    events = json.loads(raw)["events"]
    check(status == 200 and [e["id"] for e in events] == [delivery], "the event is stored once, other categories not at all")
    check(events[0]["payload"]["agentSession"]["id"] == "s1", "the payload comes back whole")
    check(call(base, "PUT", "/v1/token", b"tok en", auth)[0] == 400, "a malformed token is refused")
    check(call(base, "PUT", "/v1/token", b"not-a-real-token", auth)[0] == 204, "the machine's token is stored")
    check(call(base, "DELETE", f"/v1/events/{delivery}", headers=auth)[0] == 204, "the event is deleted")
    check(json.loads(call(base, "GET", "/v1/events", headers=auth)[1])["events"] == [], "nothing waits after the delete")
    check(send(base, payload("created", "s1"), delivery=delivery) == 200
          and json.loads(call(base, "GET", "/v1/events", headers=auth)[1])["events"] == [],
          "a late redelivery of a handled event is not stored again")


def load_agent(state, config):
    os.environ["STATE"] = str(state)
    os.environ["LINEAR_AGENT_DIR"] = str(config)
    os.environ["LINEAR_TOKEN_FILE"] = str(config / "token.json")
    spec = importlib.util.spec_from_file_location("linear_agent", HERE.parent / "scripts/linear-agent.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def script_tests(base, folder):
    config, state = folder / "config", folder / "state"
    config.mkdir()
    (config / "url").write_text(base + "\n")
    (config / "pull-token").write_text(PULL + "\n")
    (config / "token.json").write_text(json.dumps({"access_token": "fake-linear-token"}))
    agent = load_agent(state, config)
    posted, runs = [], []
    users = {"u-member": {"active": True, "guest": False, "app": False},
             "u-guest": {"active": True, "guest": True, "app": False}}

    def gql(query, variables):
        if "agentActivityCreate" in query:
            posted.append((variables["input"]["agentSessionId"], variables["input"]["content"]["type"],
                           variables["input"]["content"]["body"]))
            return {"agentActivityCreate": {"success": True}}
        if "user(id" in query:
            return {"user": users.get(variables["id"])}
        raise AssertionError(query)

    def run_agent(session, event_id, prompt):
        runs.append((session, prompt))
        return 0, "[omp @ ms999-session · test] done"

    agent.poll.gql = gql
    agent.run_agent = run_agent
    for body in (payload("created", "s2"),
                 payload("created", "s3", creator="u-guest"),
                 payload("prompted", "s2", agentActivity={"userId": "u-member", "content": {"type": "prompt", "body": "also add tests"}}),
                 payload("created", "s4"),
                 payload("prompted", "s4", agentActivity={"userId": "u-member", "signal": "stop", "content": {"type": "prompt", "body": "stop"}})):
        send(base, body)
        time.sleep(0.02)  # the Worker orders events by arrival time in milliseconds
    agent.run_pass()
    check([r[0] for r in runs] == ["s2", "s2"], "the member's session and its follow-up ran; the guest's and the stopped one did not")
    check("MS-999. It came as a Linear agent session (s2)" in runs[0][1] and "<issue identifier=\"MS-999\">" in runs[0][1]
          and "`MS999Session`" in runs[0][1], "the task names the issue, session, agent id and Linear's prompt context")
    check("also add tests" in runs[1][1], "a follow-up's task carries the user's new message")
    kinds = [(s, k) for s, k, _ in posted]
    check(kinds == [("s2", "thought"), ("s2", "response"), ("s3", "error"), ("s2", "thought"), ("s2", "response"), ("s4", "response")],
          f"activities: thought then response, error for the guest, response to stop ({kinds})")
    check(posted[1][2].startswith("[omp @ ms999-session"), "the agent's final message is the response")
    auth = {"Authorization": f"Bearer {PULL}"}
    check(json.loads(call(base, "GET", "/v1/events", headers=auth)[1])["events"] == [], "every event was deleted from the Worker")
    before = len(posted)
    agent.run_pass()
    check(len(posted) == before, "a second pass finds nothing to do")


def main():
    if not shutil.which("npx"):
        sys.exit("npx (Node.js) is needed for wrangler")
    with tempfile.TemporaryDirectory() as tmp:
        folder = Path(tmp)
        site = folder / "worker"
        site.mkdir()
        for name in ("worker.js", "wrangler.toml"):
            shutil.copy(HERE / name, site)
        (site / ".dev.vars").write_text(f"LINEAR_WEBHOOK_SECRET={SECRET}\nPULL_TOKEN={PULL}\n")
        port = free_port()
        log = (folder / "wrangler.log").open("w")
        dev = subprocess.Popen(["npx", "--yes", "wrangler@4", "dev", "--ip", "127.0.0.1", "--port", str(port),
                                "--persist-to", str(folder / "state-do")], cwd=site, stdout=log,
                               stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        base = f"http://127.0.0.1:{port}"
        try:
            for _ in range(120):
                try:
                    if call(base, "GET", "/")[0] == 404:
                        break
                except OSError:
                    pass
                time.sleep(0.5)
            else:
                sys.exit("wrangler dev didn't start:\n" + (folder / "wrangler.log").read_text())
            worker_tests(base)
            script_tests(base, folder)
        finally:
            os.killpg(dev.pid, 15)
            dev.wait(timeout=30)
    print("all passed")


if __name__ == "__main__":
    main()
