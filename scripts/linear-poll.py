#!/usr/bin/env python3
"""One pass over Linear (`watch`: one a minute until one has output): claims what an omp session
should act on now and prints it as JSON lines, which that session hands to subagents, each
working in its own checkout (AGENTS.md, Version control). Each line carries what the session
needs to hand it out, so it doesn't ask Linear about it (MS-174). Python 3.9+ standard library
only, so the same script runs on macOS and Windows.

  {"kind": "issue", "id", "identifier", "resume", "unblockedBy", "title", "state", "labels",
    "creator", "project", "parent", "description"}: an issue to start. labels are its label names
    (`omp` left out), creator the name of whoever filed it, parent `<identifier> <title>`,
    description its first DESCRIPTION characters (default 600, `…` when cut); project, parent
    and description are left out when unset. Claimed when it is
    - in Todo, created by a workspace member or by omp itself (not another app, a guest or a
      GitHub import: the repo is public and the subagents run with full access), not claimed
      before and not yet commented on by an app (omp); or
    - claimed before, out of In Progress and no longer blocked: every issue blocking it (Linear
      "blocked by") is In Review, Done, Canceled, a duplicate or labelled `install` (pushed to
      `main`, waiting only for the batch install). Claimed once per set of blockers, so
      blocking it again by another issue resumes it again; or
    - labelled `omp`, in any state but started, done or canceled; the label is removed on
      claim, so adding it again reruns the issue.
    The first two skip issues with a blocker still open: a blocked issue waits until its
    blockers are on `main` (`install` or In Review), not for the user's review. resume is true
    for an issue claimed before (the subagent carries on from the issue's comments); unblockedBy
    lists the identifiers of its resolved blockers. Agents file their own issues in Backlog and move
    issues to Todo to ask the user something (with an omp comment), and the polling session leaves
    issues it queues in Todo with an omp comment saying so (MS-175), so none of them is picked up
    again unless the user moves an uncommented one to Todo, adds `omp`, or it was claimed before
    and gets unblocked.
  {"kind": "install", "issues"}: the identifiers of every open issue labelled `install` (INSTALL),
    which code agents add once their change is tested and on `main` (AGENTS.md, Release
    delivery). Printed when at least one waits, no agent whose id starts with `Install` is in
    RUNNING, and the previous install line is INSTALL_EVERY seconds old (default 3600). The
    caller starts one `ms-install` agent (.omp/agents) that installs `main`'s tip once for every
    listed issue and checks only that the install worked (revision, signature, launch).
  {"kind": "comment", "id", "issue", "issueId", "title", "state", "createdAt", "author", "body"}: a
    comment posted on any team issue in the last 7 days, each once, by a workspace member (not an
    app, a guest or a GitHub sync) or by another machine's omp app (one of PEERS, comma-separated
    app names, default `omp,windows-omp`); title and state are its issue's. The caller routes it
    to the issue's subagent or starts one.
  {"kind": "stale", "id", "identifier", "title", "agent", "claimedAt"}: an In Progress issue labelled
    `claimed` whose owner, the agent named in its latest start comment
    (`[omp @ <checkout>] start (<agent id>): …`, posted at claimedAt), is not in RUNNING, and
    whose claim is at least 10 minutes old. agent is null when no start comment names one. Each
    start comment is reported once: whoever takes the issue over posts a new one. Needs the
    agent ids running now, as the caller sees them, as a JSON array: in RUNNING_FILE (re-read
    each pass; wins when the file exists) or RUNNING; without either nothing is reported stale.
    Issues out of In Progress whose owner is not running lose `claimed`.
Machines: the poller covers the teams in TEAMS (comma-separated ids, default Mailstream `MS` and
`WIN`). Issues of a team in HOST_TEAMS (comma-separated `KEY=host`, default `WIN=windows`) belong
to that host's poller whatever their labels; in other teams, issues labelled with one of
HOST_LABELS (comma-separated, default `windows`) belong to the poller started with that
HOST_LABEL; every other issue to the poller without one (the Mac).
Each poller claims, resumes, reports stale and routes comments only for its own issues, except
that a comment naming a machine's app (`@windows-omp`) goes to that machine alone, whatever the
issue: that is how the user and the other machine address it. Another machine's app reaches it
only that way.
Token: the Mac reads omp's from the keychain and renews it with RENEW. A machine without
`security` sets LINEAR_TOKEN_FILE (the OAuth token JSON) and LINEAR_CREDENTIALS (a file with the
OAuth app's client id on line 1 and secret on line 2); renewal fetches a client_credentials token
into LINEAR_TOKEN_FILE.
Claims are kept in ~/.local/state/linear-poll/ (STATE): `launched` (issue ids), `comments`
(comment ids), `unblocked` (issue id and the blocker ids it was claimed with), `stale` (issue id
and the start comment reported), `install` (when the last install line was printed).
DRY_RUN=1 prints without claiming (unclaim lines included). `linear-poll.py seen comment|issue
<id>` puts omp's 👀 on a comment or issue once the caller has handed it to a subagent, so the
user sees it was picked up.
`linear-poll.py watch` runs a pass every INTERVAL seconds (default 60) and exits after the first
pass that prints lines, with that pass's lines and status; passes with nothing to print (unclaims
included) keep it looping silently. Each pass runs the script afresh, so edits take effect
without a restart. A failed pass (network, Linear error, token renewal) is logged to stderr and
retried next interval; after MAX_FAILURES (default 10) failures in a row it exits 1. The caller
starts it once as a background job with no deadline, waits for its result, handles the lines and
starts it again, and rewrites RUNNING_FILE whenever its agents start or finish. With DRY_RUN=1 it
exits after the first plan holding a line other than unclaim.
On macOS, run in the default checkout, `watch` locks that checkout's source again before each
pass when anything there is writable (scripts/source_lock.py, MS-156), logging it to stderr.
The caller keeps context while it is still in the prompt cache (1 h on a Claude subscription,
5 min on an API key). A user comment or rework on an issue goes to the agent that worked it,
woken with `write agent://<id>` even after it finished (it reads its transcript from cache, even
parked), when its last request is under ~55 minutes old and its context under 400K tokens.
Otherwise, and for any other issue or a stale one, a new agent picks up from the issue's
comments and posts its own start comment: rewriting an expired transcript costs more than
re-reading the files the work needs. New agents start from the repo's templates in .omp/agents
(`ms-issue`, `ms-discuss`, `ms-install`), which carry the fixed instructions, so the task names
only the issue and what the issue doesn't say (AGENTS.md, Polling session). Forking an agent's
session (`omp --fork`) saves nothing: the fork runs with the main system prompt and tools, so
its prefix misses the cache. An agent that is running gets more work only on its own issue.
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

import source_lock

TEAMS = os.environ.get("TEAMS", ",".join([
    "9f8fe59f-3788-4542-8c18-4e5c5dd5ddc2",  # MS: Mailstream; ids survive key renames
    "e94eb87b-32d5-4de3-9e90-11c5c17f336d",  # WIN: the Windows machine's own work
])).split(",")
HOST_TEAMS = dict(pair.split("=", 1) for pair in os.environ.get("HOST_TEAMS", "WIN=windows").split(",") if pair)
LABEL = os.environ.get("LABEL", "omp")
CLAIMED = os.environ.get("CLAIMED", "claimed")
HOST_LABEL = os.environ.get("HOST_LABEL", "")
HOST_LABELS = os.environ.get("HOST_LABELS", "windows").split(",")
PEERS = os.environ.get("PEERS", "omp,windows-omp").split(",")
RENEW = Path.home() / ".pi/agent/extensions/linear/renew-omp-token.sh"
TOKEN_FILE = os.environ.get("LINEAR_TOKEN_FILE", "")
CREDENTIALS = os.environ.get("LINEAR_CREDENTIALS", "")
STATE = Path(os.environ.get("STATE") or Path.home() / ".local/state/linear-poll")
LAUNCHED = STATE / "launched"
HANDLED = STATE / "comments"
UNBLOCKED = STATE / "unblocked"
STALE = STATE / "stale"
INSTALL = os.environ.get("INSTALL", "install")
INSTALLED = STATE / "install"
INSTALL_EVERY = int(os.environ.get("INSTALL_EVERY", "3600"))
DESCRIPTION = int(os.environ.get("DESCRIPTION", "600"))
START = re.compile(r"^\[omp @ [^\]]*\] start \((?P<a>[^)]+)\)")


class Failed(Exception):
    """A logged failure: the pass exits 1."""


def log(message):
    print(f"{datetime.now():%Y-%m-%d %H:%M:%S} {message}", file=sys.stderr, flush=True)


def emit(line):
    print(json.dumps(line, ensure_ascii=False, separators=(",", ":")), flush=True)


def token():
    if TOKEN_FILE:
        return json.loads(Path(TOKEN_FILE).read_text(encoding="utf-8"))["access_token"]
    return subprocess.run(["security", "find-generic-password", "-s", "linear-omp", "-a", "access_token", "-w"],
                          check=True, capture_output=True, text=True).stdout.strip()


def renew():
    if not TOKEN_FILE:
        subprocess.run([str(RENEW)], check=True, stdout=subprocess.DEVNULL)
        return
    client_id, secret = Path(CREDENTIALS).read_text(encoding="utf-8").splitlines()[:2]
    form = urllib.parse.urlencode({"grant_type": "client_credentials", "client_id": client_id.strip(),
                                   "client_secret": secret.strip(), "scope": "read,write"}).encode()
    with urllib.request.urlopen("https://api.linear.app/oauth/token", form, timeout=60) as response:
        out = json.load(response)
    if not out.get("access_token"):
        raise ValueError("no access_token in the response")
    Path(TOKEN_FILE).write_text(json.dumps(out), encoding="utf-8")


def post(body):
    request = urllib.request.Request("https://api.linear.app/graphql", body, {
        "Authorization": f"Bearer {token()}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
    except urllib.error.HTTPError as error:  # Linear answers GraphQL and auth errors with a JSON body
        raw = error.read()
    except (OSError, subprocess.CalledProcessError) as error:
        log(f"linear request failed: {error}")
        raise Failed from error
    try:
        out = json.loads(raw)
    except ValueError:
        out = None
    if not isinstance(out, dict):
        log(f"linear: unreadable response: {raw[:200].decode(errors='replace')}")
        raise Failed
    return out


def gql(query, variables):
    """Returns .data; renews the omp app token once on an authentication error."""
    body = json.dumps({"query": query, "variables": variables}).encode()
    out = post(body)
    if any((e.get("extensions") or {}).get("code") == "AUTHENTICATION_ERROR" for e in out.get("errors") or []):
        log("token rejected; renewing")
        try:
            renew()
        except Exception as error:
            log(f"token renewal failed: {error}")
            raise Failed from error
        out = post(body)
    if out.get("errors"):
        log(f"linear error: {json.dumps([e.get('message') for e in out['errors']], ensure_ascii=False)}")
        raise Failed
    return out["data"]


def lines(path):
    return path.read_text(encoding="utf-8").split("\n")


def append(path, text):
    with path.open("a", encoding="utf-8", newline="\n") as f:
        f.write(text + "\n")


def remove_label(issue, label):
    gql("mutation($id: String!, $label: String!) { issueRemoveLabel(id: $id, labelId: $label) { success } }",
        {"id": issue, "label": label})


def seen(kind, id):
    field = {"comment": "commentId", "issue": "issueId"}.get(kind)
    if not field or not id:
        print(f"usage: {sys.argv[0]} seen comment|issue <id>", file=sys.stderr)
        return 2
    gql(f'mutation($id: String!) {{ reactionCreate(input: {{ {field}: $id, emoji: "eyes" }}) {{ success }} }}',
        {"id": id})
    return 0


def relock():
    """Locks the default checkout again when its source is writable (source_lock.py, MS-156)."""
    if sys.platform != "darwin":
        return
    try:
        if source_lock.default_checkout() and (open_paths := source_lock.writable()):
            source_lock.lock()
            log(f"locked the default checkout again: {len(open_paths)} tracked files or folders were writable")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        detail = (getattr(error, "stderr", None) or "").strip()
        log(f"could not check the default checkout's lock: {error}" + (f": {detail}" if detail else ""))


def watch():
    failures = 0
    while True:
        relock()
        result = subprocess.run([sys.executable, __file__], stdout=subprocess.PIPE, text=True, encoding="utf-8")
        out = result.stdout.rstrip("\n")
        # Real passes print only what the caller handles; a dry run also shows unclaim lines.
        if any(not line.startswith('{"kind":"unclaim"') for line in out.split("\n") if line):
            print(out, flush=True)
            return result.returncode
        if result.returncode == 0:
            failures = 0
        else:
            failures += 1
            if failures >= int(os.environ.get("MAX_FAILURES", "10")):
                log(f"pass failed {failures} times in a row; giving up")
                return 1
            log(f"pass failed (exit {result.returncode}); retrying in {os.environ.get('INTERVAL', '60')}s")
        time.sleep(int(os.environ.get("INTERVAL", "60")))


QUERY = """query($teams: [ID!], $label: String!, $claimed: String!, $install: String!) {
  viewer { id name }
  queued: issues(first: 100, filter: {
    team: { id: { in: $teams } }, state: { type: { in: ["triage", "backlog", "unstarted"] } },
    or: [{ labels: { some: { name: { eq: $label } } } }, { state: { name: { eq: "Todo" } } },
      { hasBlockedByRelations: { eq: true } }] }) {
    nodes { id identifier number title description team { key } state { name }
      creator { id name app guest } project { name } parent { identifier title }
      labels { nodes { id name } } comments(first: 100) { nodes { user { app } } }
      inverseRelations(first: 50) { nodes { type issue { id identifier state { name type }
        labels { nodes { name } } } } } } }
  installs: issues(first: 100, filter: {
    team: { id: { in: $teams } }, labels: { some: { name: { eq: $install } } },
    state: { type: { nin: ["completed", "canceled", "duplicate"] } } }) {
    nodes { identifier number team { key } labels { nodes { name } } } }
  claimed: issues(first: 100, filter: {
    team: { id: { in: $teams } }, labels: { some: { name: { eq: $claimed } } } }) {
    nodes { id identifier title team { key } state { name } labels { nodes { id name } }
      comments(first: 100) { nodes { id createdAt body user { app } } } } }
  comments(first: 100, filter: { createdAt: { gt: "-P7D" }, issue: { team: { id: { in: $teams } } } }) {
    nodes { id createdAt body user { id name app guest }
      issue { id identifier title team { key } state { name } labels { nodes { name } } } } }
}"""


def labelled(issue):
    return next((l["id"] for l in issue["labels"]["nodes"] if l["name"] == LABEL), None)


def host(issue):
    team = HOST_TEAMS.get(issue["team"]["key"])
    if team is not None:
        return team
    names = [l["name"] for l in issue["labels"]["nodes"]]
    return next((h for h in HOST_LABELS if h in names), "")


def member(user):
    return user is not None and user.get("app") is not True and user.get("guest") is not True


def mentions(body, name):
    return re.search(rf"@{re.escape(name)}(?![\w-])", body) is not None


def routed(comment, me):
    """Whether a comment goes to this poller: see "Machines" above."""
    user, body = comment["user"], comment["body"]
    if user is None or user["id"] == me["id"]:
        return False
    if member(user):
        named = [peer for peer in PEERS if mentions(body, peer)]
        return me["name"] in named if named else host(comment["issue"]) == HOST_LABEL
    return user.get("app") is True and user["name"] in PEERS and mentions(body, me["name"])


def blockers(issue):
    return [r["issue"] for r in issue["inverseRelations"]["nodes"] if r["type"] == "blocks"]


def resolved(issue):
    return (issue["state"]["type"] in ("completed", "canceled", "duplicate") or issue["state"]["name"] == "In Review"
            or any(l["name"] == INSTALL for l in issue["labels"]["nodes"]))


def about(issue):
    """What the caller needs to hand an issue out without asking Linear: see the issue line above."""
    out = {"title": issue["title"], "state": issue["state"]["name"],
           "labels": [l["name"] for l in issue["labels"]["nodes"] if l["name"] != LABEL],
           "creator": (issue["creator"] or {}).get("name")}
    if issue["project"]:
        out["project"] = issue["project"]["name"]
    if issue["parent"]:
        out["parent"] = f"{issue['parent']['identifier']} {issue['parent']['title']}"
    text = (issue["description"] or "").strip()
    if text:
        out["description"] = text if len(text) <= DESCRIPTION else text[:DESCRIPTION].rstrip() + "…"
    return out


def timestamp(text):
    return datetime.strptime(re.sub(r"\.[0-9]+Z$", "Z", text), "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def plan(data, running):
    """Issue lines oldest first, then stale and comment lines oldest first."""
    omp = data["viewer"]["id"]
    done = [line.split(" ")[0] for line in lines(LAUNCHED)]
    handled, freed, reported = lines(HANDLED), lines(UNBLOCKED), lines(STALE)
    now = datetime.now(timezone.utc)
    todo = []
    for issue in data["queued"]["nodes"]:
        if host(issue) != HOST_LABEL:
            continue
        claimed = issue["id"] in done
        blocking = blockers(issue)
        free = all(resolved(b) for b in blocking)
        key = f"{issue['id']} {','.join(sorted(b['id'] for b in blocking))}" if blocking and free else None
        fresh = (issue["state"]["name"] == "Todo"
                 and (member(issue["creator"]) or (issue["creator"] or {}).get("id") == omp)
                 and not claimed and not any((c["user"] or {}).get("app") is True for c in issue["comments"]["nodes"]))
        unblocked = claimed and key is not None and key not in freed
        if labelled(issue) is not None or (free and (fresh or unblocked)):
            todo.append({"kind": "issue", "id": issue["id"], "identifier": issue["identifier"], "resume": claimed,
                         "unblockedBy": [b["identifier"] for b in blocking if resolved(b)], **about(issue),
                         "label": labelled(issue), "unblockKey": key, "number": issue["number"]})
    out = sorted(todo, key=lambda line: line["number"])
    for line in out:
        del line["number"]
    waiting = sorted((i for i in data["installs"]["nodes"] if host(i) == HOST_LABEL), key=lambda i: i["number"])
    last = INSTALLED.read_text(encoding="utf-8").strip()
    if (waiting and not any(agent.startswith("Install") for agent in running or [])
            and (not last or (now - timestamp(last)).total_seconds() >= INSTALL_EVERY)):
        out.append({"kind": "install", "issues": [i["identifier"] for i in waiting]})
    if running is not None:
        for issue in data["claimed"]["nodes"]:
            if host(issue) != HOST_LABEL:
                continue
            starts = [c for c in issue["comments"]["nodes"] if (c["user"] or {}).get("app") is True and START.match(c["body"])]
            start = max(starts, key=lambda c: c["createdAt"]) if starts else None
            agent = START.match(start["body"]).group("a") if start else None
            if agent in running:
                continue
            label = next((l["id"] for l in issue["labels"]["nodes"] if l["name"] == CLAIMED), None)
            if issue["state"]["name"] != "In Progress":
                out.append({"kind": "unclaim", "id": issue["id"], "label": label})
                continue
            key = f"{issue['id']} {start['id'] if start else 'none'}"
            if key not in reported and (start is None or (now - timestamp(start["createdAt"])).total_seconds() >= 600):
                out.append({"kind": "stale", "id": issue["id"], "identifier": issue["identifier"], "title": issue["title"],
                            "agent": agent, "claimedAt": start["createdAt"] if start else None, "staleKey": key})
    for comment in sorted(data["comments"]["nodes"], key=lambda c: c["createdAt"]):
        if routed(comment, data["viewer"]) and comment["id"] not in handled:
            out.append({"kind": "comment", "id": comment["id"], "issue": comment["issue"]["identifier"],
                        "issueId": comment["issue"]["id"], "title": comment["issue"]["title"],
                        "state": comment["issue"]["state"]["name"], "createdAt": comment["createdAt"],
                        "author": comment["user"]["name"], "body": comment["body"]})
    return out


def run_pass():
    STATE.mkdir(parents=True, exist_ok=True)
    for path in (LAUNCHED, HANDLED, UNBLOCKED, STALE, INSTALLED):
        path.touch()
    running = json.loads(os.environ.get("RUNNING", "null"))
    running_file = os.environ.get("RUNNING_FILE")
    if running_file and Path(running_file).exists():
        running = json.loads(Path(running_file).read_text(encoding="utf-8"))
        if not isinstance(running, list):
            log(f"RUNNING_FILE {running_file} is not a JSON array")
            return 1
    data = gql(QUERY, {"teams": TEAMS, "label": LABEL, "claimed": CLAIMED, "install": INSTALL})
    out = plan(data, running)
    if os.environ.get("DRY_RUN"):
        for line in out:
            emit(line)
        return 0
    for line in out:
        kind = line["kind"]
        if kind == "comment":
            append(HANDLED, line["id"])
            emit(line)
        elif kind == "stale":
            append(STALE, line.pop("staleKey"))
            emit(line)
        elif kind == "install":
            INSTALLED.write_text(datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ") + "\n", encoding="utf-8")
            emit(line)
        elif kind == "unclaim":
            remove_label(line["id"], line["label"])
        else:
            label, key = line.pop("label"), line.pop("unblockKey")
            if not line["resume"]:
                append(LAUNCHED, f"{line['id']} {line['identifier']}")
            if key:
                append(UNBLOCKED, key)
            if label:
                remove_label(line["id"], label)
            emit(line)
    return 0


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    args = sys.argv[1:]
    try:
        if args[:1] == ["seen"]:
            return seen(*(args[1:3] + [None, None])[:2])
        if args[:1] == ["watch"]:
            return watch()
        return run_pass()
    except Failed:
        return 1


if __name__ == "__main__":
    sys.exit(main())
