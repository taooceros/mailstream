#!/usr/bin/env python3
"""Linear's agent sessions for this machine's app (MS-377, step 1 of replacing linear-poll.py's
discovery): someone delegates an issue to the app (`linux-omp`) or mentions it, Linear sends an
Agent session webhook to the Cloudflare Worker in `LinearAgent/` (the machine is often off and
not reachable from outside), and this script pulls it from there, hands it to an omp agent and
answers in the session. Python 3.9+ standard library only. linear-poll.py keeps running beside it.

  linear-agent.py watch   pulls every INTERVAL seconds (default 5) and handles what waits, one
                          event at a time; a failed pass is logged to stderr and retried.
  linear-agent.py once    one pass, then exits (1 when it failed).

Each pass first leaves the machine's current Linear access token with the Worker (only when it
changed), so the Worker can acknowledge a new session within Linear's 10 seconds while this
machine is off. Then, for each waiting event, oldest first:
- `created` (delegated or mentioned) and `prompted` (a new message in the session): only from
  an active workspace member, not a guest or an app (the repo is public and the agent runs with
  full access); anything else gets an `error` activity. Otherwise a `thought` activity says the
  machine started, then `omp -p --auto-approve` runs in this repo with the `ms-issue` agent
  template (`.omp/agents/ms-issue.md`) appended to its system prompt and a task naming the
  issue, the session and Linear's promptContext (for `prompted`, the new message), as the
  polling session's subagents get it. While it runs, an ephemeral `thought` goes out every
  HEARTBEAT seconds (default 1200), since Linear marks a session stale after 30 minutes without
  activity. Its final message becomes the session's `response`; a failed run an `error`.
- a `prompted` event with Linear's `stop` signal drops the other waiting events of that session
  and answers that it stopped (step 1 can't stop an agent that is already running).
The event is then deleted from the Worker; its id also goes to STATE/done first, so a failed
delete doesn't run it twice. The agent's stderr goes to STATE/logs/<event id>.log.
Config: LINEAR_AGENT_DIR (default ~/.config/linear-agent) holds `url` (the Worker's
https://….workers.dev) and `pull-token` (the Worker's PULL_TOKEN), both written by
`LinearAgent/deploy.py`. The Linear token comes as in linear-poll.py (LINEAR_TOKEN_FILE and
LINEAR_CREDENTIALS on Linux and Windows, its renewal included), with its functions; the token's
scope set must include `app:assignable,app:mentionable` (LINEAR_SCOPE there). STATE defaults to
~/.local/state/linear-agent.
"""
import importlib.util
import json
import os
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
_spec = importlib.util.spec_from_file_location("linear_poll", HERE / "linear-poll.py")
poll = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(poll)

CONFIG = Path(os.environ.get("LINEAR_AGENT_DIR") or Path.home() / ".config/linear-agent")
STATE = Path(os.environ.get("STATE") or Path.home() / ".local/state/linear-agent")
DONE = STATE / "done"
LOGS = STATE / "logs"
INTERVAL = int(os.environ.get("INTERVAL", "5"))
HEARTBEAT = int(os.environ.get("HEARTBEAT", "1200"))
TEMPLATE = ROOT / ".omp/agents/ms-issue.md"
TAG = "[linear-agent]"

log = poll.log
Failed = poll.Failed


def worker(method, path, body=None):
    """Calls the Worker; returns its JSON answer (None for an empty one)."""
    url = (CONFIG / "url").read_text(encoding="utf-8").strip().rstrip("/")
    key = (CONFIG / "pull-token").read_text(encoding="utf-8").strip()
    request = urllib.request.Request(url + path, body, {"Authorization": f"Bearer {key}"}, method=method)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
    except (OSError, urllib.error.HTTPError) as error:
        log(f"worker {method} {path} failed: {error}")
        raise Failed from error
    return json.loads(raw) if raw else None


def activity(session, content, ephemeral=False):
    poll.gql("mutation($input: AgentActivityCreateInput!) { agentActivityCreate(input: $input) { success } }",
             {"input": {"agentSessionId": session, "content": content, "ephemeral": ephemeral}})


def member(user_id):
    """True for an active workspace member who is neither a guest nor an app."""
    if not user_id:
        return False
    user = poll.gql("query($id: String!) { user(id: $id) { active guest app } }", {"id": user_id})["user"]
    return bool(user and user["active"] and not user["guest"] and not user["app"])


def task(payload):
    session = payload["agentSession"]
    issue = session["issue"]["identifier"]
    agent = issue.replace("-", "") + "Session"
    checkout = issue.replace("-", "").lower() + "-session"
    lines = [
        f"{issue}. It came as a Linear agent session ({session['id']}): the user "
        + ("delegated the issue to this machine's app or mentioned it." if payload["action"] == "created"
           else "wrote a new message in the session, below; earlier messages are on the issue."),
        f"Your agent id is `{agent}` (checkout `{checkout}`).",
        "Your final message becomes the session's response on Linear, so it starts with the "
        "`[omp @ <checkout> · <model>]` prefix (AGENTS.md, Writing).",
    ]
    if payload["action"] == "prompted":
        lines += ["", "The user's message:", message(payload)]
    if payload.get("promptContext"):
        lines += ["", "Linear's prompt context for the session:", payload["promptContext"]]
    return "\n".join(lines)


def message(payload):
    activity_ = payload.get("agentActivity") or {}
    return (activity_.get("content") or {}).get("body") or activity_.get("body") or ""


def template():
    text = TEMPLATE.read_text(encoding="utf-8")
    if text.startswith("---\n"):
        text = text.split("\n---\n", 1)[1]
    return text.strip()


def run_agent(session, event_id, prompt):
    """Runs omp on the task; returns (exit status, final message)."""
    LOGS.mkdir(parents=True, exist_ok=True)
    command = ["omp", "-p", "--auto-approve", "--cwd", str(ROOT), "--append-system-prompt", template(), prompt]
    with (LOGS / f"{event_id}.log").open("w", encoding="utf-8") as stderr:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=stderr,
                                   cwd=ROOT, text=True, encoding="utf-8")
        output = []
        reader = threading.Thread(target=lambda: output.append(process.stdout.read()), daemon=True)
        reader.start()
        started = time.monotonic()
        while True:
            try:
                process.wait(timeout=HEARTBEAT)
                break
            except subprocess.TimeoutExpired:
                minutes = int((time.monotonic() - started) / 60)
                try:
                    activity(session, {"type": "thought", "body": f"{TAG} agent 仍在工作（已 {minutes} 分钟）。"}, True)
                except Failed:
                    pass
        reader.join()
    return process.returncode, "".join(output).strip()


def handle(event, stopped):
    """Answers one event; marks it done once its agent has run, so a lost answer doesn't rerun it."""
    payload = event["payload"]
    session = payload["agentSession"]
    sid = session["id"]
    action = payload["action"]
    prompt = payload.get("agentActivity") or {}
    if action == "prompted" and prompt.get("signal") == "stop":
        activity(sid, {"type": "response", "body": f"{TAG} 已停止：这个会话还没开始的任务不再执行。"
                                                    "（第 1 步不能中途停下已经在跑的 agent。）"})
        return
    if sid in stopped:
        return
    if action not in ("created", "prompted"):
        log(f"skipping {action} event {event['id']}")
        return
    if not session.get("issue"):
        activity(sid, {"type": "error", "body": f"{TAG} 只处理 issue 上的会话。"})
        return
    if not member(session.get("creatorId") if action == "created" else prompt.get("userId")):
        activity(sid, {"type": "error", "body": f"{TAG} 只接受工作区正式成员（不是访客或应用）发起的任务。"})
        return
    activity(sid, {"type": "thought", "body": f"{TAG} Linux 机器已拉到任务，交给一个 omp agent 处理。"})
    status, final = run_agent(sid, event["id"], task(payload))
    mark(event["id"])
    if status == 0 and final:
        content = {"type": "response", "body": final}
    else:
        content = {"type": "error", "body": f"{TAG} agent 异常退出（状态 {status}），日志在本机 "
                                            f"`{LOGS / (event['id'] + '.log')}`。" + (f"\n\n{final}" if final else "")}
    for _ in range(5):
        try:
            activity(sid, content)
            return
        except Failed:
            time.sleep(30)
    log(f"gave up posting the result of {event['id']}: {json.dumps(content, ensure_ascii=False)}")


def mark(event_id):
    STATE.mkdir(parents=True, exist_ok=True)
    with DONE.open("a", encoding="utf-8") as f:
        f.write(event_id + "\n")


def done():
    return set(DONE.read_text(encoding="utf-8").split()) if DONE.exists() else set()


pushed = None


def run_pass():
    global pushed
    current = poll.token()
    if current != pushed:
        worker("PUT", "/v1/token", current.encode())
        pushed = current
    events = worker("GET", "/v1/events")["events"]
    stopped = {e["payload"]["agentSession"]["id"] for e in events
               if e["payload"].get("action") == "prompted"
               and (e["payload"].get("agentActivity") or {}).get("signal") == "stop"}
    for event in events:
        if event["id"] not in done():
            payload = event["payload"]
            log(f"{payload.get('action')} {(payload['agentSession'].get('issue') or {}).get('identifier')} {event['id']}")
            try:
                handle(event, stopped)
            except Failed:
                raise  # Linear or the network: the next pass tries again
            except Exception as error:  # an event this script can't read would otherwise block every pass
                log(f"event {event['id']} failed: {error!r}")
            if event["id"] not in done():
                mark(event["id"])
        worker("DELETE", f"/v1/events/{event['id']}")


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    args = sys.argv[1:]
    if args == ["once"]:
        try:
            run_pass()
        except Failed:
            return 1
        return 0
    if args != ["watch"]:
        sys.exit(f"usage: {sys.argv[0]} watch|once")
    while True:
        try:
            run_pass()
        except Failed:
            pass  # already logged
        except Exception as error:  # a broken Worker answer or token file: log it and try again
            log(f"pass failed: {error!r}")
        time.sleep(INTERVAL)


if __name__ == "__main__":
    sys.exit(main())
