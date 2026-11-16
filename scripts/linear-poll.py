#!/usr/bin/env python3
"""One pass over Linear (`watch`: every 5 s until one has output): claims what an omp session
should act on now and prints it as JSON lines, which that session hands to subagents, each
working in its own checkout (AGENTS.md, Version control). Each line carries what the session
needs to hand it out, so it doesn't ask Linear about it (MS-174). Python 3.9+ standard library
only, so the same script runs on macOS, Windows and Linux.

  {"kind": "issue", "id", "identifier", "resume", "unblockedBy", "title", "state", "priority",
    "priorityValue", "labels", "creator", "project", "parent", "description", "session"}: an issue to start.
    priority is Linear's priority label (`Urgent`, `High`, `Medium`, `Low`, `No priority`) and
    priorityValue its number (1 Urgent … 4 Low, 0 none); an Urgent issue starts at once, past the
    limit on heavy agents (AGENTS.md, Polling session, MS-252). labels are its label names
    (`omp` left out), creator the name of whoever filed it, parent `<identifier> <title>`,
    description its first DESCRIPTION characters (default 600, `…` when cut); project, parent
    and description are left out when unset. The identifier's prefix is its team, which is its
    project (`MS` Mailstream, `WIN`, `MRV`, `HET`). Only issues delegated to this machine's app
    (Machines) are claimed, when one is
    - not started, done or canceled, created by a workspace member or by one of the machines' apps
      in APPS (not another app, a guest or a GitHub import: the repos are public and the subagents
      run with full access) and not claimed before: delegating an issue starts it; or
    - claimed before, out of In Progress and no longer blocked: every issue blocking it (Linear
      "blocked by") is In Review, Done, Canceled, a duplicate or labelled `install` (pushed to
      `main`, waiting only for the batch install). Claimed once per set of blockers, so
      blocking it again by another issue resumes it again; or
    - labelled `omp`, in any state but started, done or canceled; the label is removed on
      claim, so adding it again reruns the issue.
    session is the id of the agent session that asked for it (see Agent sessions), else left out.
    The first two skip issues with a blocker still open: a blocked issue waits until its
    blockers are on `main` (`install` or In Review), not for the user's review. resume is true
    for an issue claimed before (the subagent carries on from the issue's comments); unblockedBy
    lists the identifiers of its resolved blockers. An issue is claimed once per machine (`launched`):
    agents leave issues in Todo or Backlog to ask the user, wait for a blocker or queue them
    (MS-175), and none of them is picked up again unless it gets `omp` or unblocked. Agents file
    issues meant for later undelegated, since delegating starts them.
  {"kind": "install", "issues"}: the identifiers of every open issue of this machine labelled `install` (INSTALL),
    which code agents add once their change is tested and on `main` (AGENTS.md, Release
    delivery). Printed when at least one waits, no agent whose id starts with `Install` is in
    RUNNING, and the previous install line is INSTALL_EVERY seconds old (default 3600). The
    caller starts one `ms-install` agent (.omp/agents) that installs `main`'s tip once for every
    listed issue and checks only that the install worked (revision, signature, launch).
  {"kind": "comment", "id", "issue", "issueId", "title", "state", "createdAt", "author", "body",
    "session"}: a comment posted in the last 7 days, each once, on an issue delegated to this
    machine or naming its app (Machines), by a workspace member (not an app, a guest or a GitHub
    sync) or by another machine's app (one of APPS); title and state are its issue's. session is
    the agent session it was written in, else left out; a message sent in a session without a
    comment has the prompt activity's id as id. The caller routes it to the issue's subagent or
    starts one.
  An issue or comment line with "overdue" (minutes since it was first printed) is one printed before
    that nobody has answered on the issue (Ledger below): the caller hands it out again, or writes
    on the issue why not (left to another machine, waiting on something).
  {"kind": "stop", "session", "issue", "issueId", "title", "author", "createdAt"}: the user asked to
    stop the agent of that session (the `stop` signal). The caller stops the agent working on the
    issue (`proc://<id>/kill`); the script has already answered the session.
  {"kind": "stale", "id", "identifier", "title", "agent", "claimedAt"}: an In Progress issue labelled
    `claimed` whose owner, the agent named in its latest start comment
    (`[omp @ <checkout> · <model>] start (<agent id>): …`, posted at claimedAt; START also takes
    the older `[omp @ <checkout>]`), is not in RUNNING, and whose claim is at least 10 minutes
    old. agent is null when no start comment names one. Each start comment is reported once:
    whoever takes the issue over posts a new one. Needs the agent ids running now, as the caller
    sees them, as a JSON array: in RUNNING_FILE (re-read each pass; wins when the file exists) or
    RUNNING; without either nothing is reported stale.
    Issues out of In Progress whose owner is not running lose `claimed`.
Machines (MS-386, MS-387): each machine's poller runs as its own Linear OAuth app (`mac-omp`,
`windows-omp`, `linux-omp`, `saturn-omp`: the Mac, Windows, Linux, saturn) and watches every team,
one team per project. The script knows the apps by their Linear user ids (APPS, comma-separated,
default those four and FORMER's) and itself by the token's viewer, never by name: each full pass
reads the apps' current names, so renaming an app in Linear changes nothing here. FORMER lists a
machine's earlier apps (the Mac's `omp`, replaced by the new app `mac-omp` in MS-387 since Linear
couldn't rename it): they still count as that machine, so issues delegated to them, comments
written by them and mentions of their names are its own. An issue belongs to the machine whose
app it is delegated to (Linear's "assign to
agent" sets the issue's delegate; an app can't be its assignee): that poller alone claims,
resumes, installs, reports stale and routes comments for it. An issue delegated to nobody belongs
to no machine, so nothing happens on it until the user delegates it. A comment naming a machine's
app (`@linux-omp`, or a real Linear mention, which the API writes as the app's profile URL) goes to
that machine alone, whatever the issue: that is how the user and the other machines address it.
Another machine's app reaches it only that way. Labels and teams play no part.
Agent sessions (MS-377): with the scopes `app:assignable` and `app:mentionable` the app can be
delegated an issue and @-mentioned, and Linear opens an agent session for each
(https://linear.app/developers/agents, https://linear.app/developers/agent-interaction). Without
a webhook the pass finds them itself: the app's SESSION_COUNT most recently updated sessions
(`agentSessions`, default 10, newest first, kept to the app's own), each with its `prompt`
activities (the user's messages). A new session from a workspace member becomes a comment line
(a mention: the comment that opened it) or an issue line (a delegation), each new message in it a
comment line, a `stop` message a stop line; all carry the session id and go to this machine
whatever the issue's delegate. As it prints one, the script posts a `thought` activity saying it
was handed out, since Linear marks a session unresponsive without an activity within 10 s and
stale after 30 min without one (both recover with the next activity,
https://linear.app/developers/agent-best-practices). It answers the session itself at hand-over:
once this machine's app has commented on the issue after that thought (start comments aside) and the issue is
out of In Progress and Todo, it posts a `response` with the start of that comment and its link,
or an `elicitation` as soon as such a comment asks the user (`需要你回答`). Sessions started or
messages sent by anyone else (a guest, an app, an automation) get an `error` and no line, as do
sessions not on an issue. A delegation of an issue an agent already holds (In Progress,
`claimed`) gets only the thought. Delegated issues (above) and mentions (Machines) are claimed
without a session too, so the work arrives where Linear opens none for an app that has no
webhook. Without those scopes the app can't be delegated or mentioned at all.
Token: the Mac reads its app's from the Keychain (service `linear-mac-omp`; the old `omp`'s was
`linear-omp`) and renews it with RENEW. A machine without
`security` sets LINEAR_TOKEN_FILE (the OAuth token JSON) and LINEAR_CREDENTIALS (a file with the
OAuth app's client id on line 1 and secret on line 2); renewal fetches a client_credentials token
into LINEAR_TOKEN_FILE with the scopes in LINEAR_SCOPE (default
`read,write,app:assignable,app:mentionable`; RENEW asks for the same). The app can be delegated
issues only while its token has `app:assignable`. Every process that renews the app's token must
ask for the same set: Linear revokes the app's other tokens when the set changes
(https://linear.app/developers/oauth-actor-authorization).
Claims are kept in ~/.local/state/linear-poll/ (STATE): `launched` (issue ids), `comments`
(comment ids), `unblocked` (issue id and the blocker ids it was claimed with), `stale` (issue id
and the start comment reported), `install` (when the last install line was printed), `probe`
(latest issue/comment/session updatedAt watermark for `watch`), `sessions` (JSON: per agent
session the units handled, `created` and prompt activity ids, when it was last acknowledged and
whether it was answered since), `ledger.jsonl`, `checkpoint` and `ledger-checked` (below).
Ledger, checkpoint and rewind (MS-385, scripts/poll_ledger.py, whose header has the details):
every issue and comment line printed is recorded in `ledger.jsonl` and stays open until this
machine's app comments on its issue. `checkpoint` is the start of the last full pass whose lines
were all handled; each full pass rewinds to it less 10 minutes and fetches every comment written
since, page by page, skipping those in `comments`, so a stopped or failing poller loses none, and
a stop is recorded as a gap. An item still open OVERDUE seconds (default 1800) after it was last
printed, on an issue no running agent holds, is printed again as the same line with `overdue`,
every OVERDUE seconds until it is answered, finished or no longer this machine's: a session that
leaves an item to another machine says so on the issue. `linear-poll.py ledger [HOURS]` lists
what was printed more than once or is still open, who answered and how late, and the gaps;
`linear-poll.py ledger note MS-12 TEXT` adds a remark.
DRY_RUN=1 prints without claiming or posting (unclaim lines included, and `reply` lines with the
activity a session would get). Each issue it claims for the first
time (resume false) and each comment line written by a workspace member gets the app's 👀 reaction
as it is printed, so the user sees it was picked up whichever session handles it (MS-355); a
failed reaction is only logged.
`linear-poll.py watch` checks Linear every INTERVAL seconds (default 5) and exits after the first
pass that prints lines, with that pass's lines and status; passes with nothing to print (unclaims
included) keep it looping silently. After its first full pass, `watch` uses a lightweight
`updatedAt` probe (~5 complexity vs ~2,200 for the full query) and runs the full query only when
Linear changed, RUNNING/script changed, or FULL_EVERY seconds (default 60) elapsed. Each pass runs
the script afresh, so edits take effect without a restart. A failed pass (network, Linear error,
token renewal) is logged to stderr and retried next interval; after MAX_FAILURES (default 10)
failures in a row it exits 1. The caller starts it once as a background job with no deadline, waits
for its result, handles the lines and starts it again, and rewrites RUNNING_FILE whenever its agents
start or finish. With DRY_RUN=1 it exits after the first plan holding a line other than unclaim.
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

import poll_ledger
import source_lock

LABEL = os.environ.get("LABEL", "omp")
CLAIMED = os.environ.get("CLAIMED", "claimed")
APPS = os.environ.get("APPS", ",".join((
    "3a60f58a-3306-4ec5-8e88-1819e1f9ce43",  # mac-omp
    "26c89eea-6fa1-4455-a506-a9dbee9b5d6a",  # windows-omp
    "6b274855-c5e9-4203-84f4-56ef84f7209c",  # linux-omp
    "e09f599a-5bcb-4e17-b981-3ef92d3594a9",  # saturn-omp
    "da14602e-8f26-4082-8ca7-1be2cc5d3c5c",  # omp, the Mac's app until MS-387 (FORMER)
))).split(",")
FORMER = {"3a60f58a-3306-4ec5-8e88-1819e1f9ce43": ["da14602e-8f26-4082-8ca7-1be2cc5d3c5c"]}  # see Machines
RENEW = Path.home() / ".pi/agent/extensions/linear/renew-omp-token.sh"
TOKEN_FILE = os.environ.get("LINEAR_TOKEN_FILE", "")
CREDENTIALS = os.environ.get("LINEAR_CREDENTIALS", "")
SCOPE = os.environ.get("LINEAR_SCOPE", "read,write,app:assignable,app:mentionable")
STATE = Path(os.environ.get("STATE") or Path.home() / ".local/state/linear-poll")
LAUNCHED = STATE / "launched"
HANDLED = STATE / "comments"
UNBLOCKED = STATE / "unblocked"
STALE = STATE / "stale"
INSTALL = os.environ.get("INSTALL", "install")
INSTALLED = STATE / "install"
PROBE = STATE / "probe"
SESSIONS = STATE / "sessions"
SESSION_COUNT = int(os.environ.get("SESSION_COUNT", "10"))
INTERVAL = int(os.environ.get("INTERVAL", "5"))
FULL_EVERY = int(os.environ.get("FULL_EVERY", "60"))
INSTALL_EVERY = int(os.environ.get("INSTALL_EVERY", "3600"))
DESCRIPTION = int(os.environ.get("DESCRIPTION", "600"))
START = re.compile(r"^\[omp @ [^\]]*\] start \((?P<a>[^)]+)\)")
PREFIX = re.compile(r"^\[omp @ [^\]]*\]\s*")
QUESTION = "需要你回答"
TAG = "[linear-poll]"
WORKING_STATES = ("In Progress", "Todo")  # an issue in neither has been handed over


class Failed(Exception):
    """A logged failure: the pass exits 1."""


def log(message):
    print(f"{datetime.now():%Y-%m-%d %H:%M:%S} {message}", file=sys.stderr, flush=True)


def emit(line):
    print(json.dumps(line, ensure_ascii=False, separators=(",", ":")), flush=True)


def token():
    if TOKEN_FILE:
        return json.loads(Path(TOKEN_FILE).read_text(encoding="utf-8"))["access_token"]
    return subprocess.run(["security", "find-generic-password", "-s", "linear-mac-omp", "-a", "access_token", "-w"],
                          check=True, capture_output=True, text=True).stdout.strip()


def renew():
    if not TOKEN_FILE:
        subprocess.run([str(RENEW)], check=True, stdout=subprocess.DEVNULL)
        return
    client_id, secret = Path(CREDENTIALS).read_text(encoding="utf-8").splitlines()[:2]
    form = urllib.parse.urlencode({"grant_type": "client_credentials", "client_id": client_id.strip(),
                                   "client_secret": secret.strip(), "scope": SCOPE}).encode()
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
    """Returns .data; renews this machine's app token once on an authentication error."""
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


def react(kind, id):
    """Puts the app's 👀 on an issue or comment; a failure is logged and doesn't stop the pass."""
    field = {"comment": "commentId", "issue": "issueId"}[kind]
    try:
        gql(f'mutation($id: String!) {{ reactionCreate(input: {{ {field}: $id, emoji: "eyes" }}) {{ success }} }}',
            {"id": id})
    except Failed:
        log(f"could not react to {kind} {id}")


def activity(session, content):
    """Posts an agent activity to a session; a failure is logged and doesn't stop the pass."""
    try:
        gql("mutation($input: AgentActivityCreateInput!) { agentActivityCreate(input: $input) { success } }",
            {"input": {"agentSessionId": session, "content": content}})
    except Failed:
        log(f"could not post a {content['type']} to agent session {session}")


def relock():
    """Locks the default checkout again when its source is writable (source_lock.py, MS-156)."""
    if sys.platform != "darwin":
        return
    try:
        if source_lock.is_default_checkout() and (open_paths := source_lock.writable()):
            source_lock.lock()
            log(f"locked the default checkout again: {len(open_paths)} tracked files or folders were writable")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        detail = (getattr(error, "stderr", None) or "").strip()
        log(f"could not check the default checkout's lock: {error}" + (f": {detail}" if detail else ""))


def watch():
    failures = 0
    probe = False
    while True:
        relock()
        env = {**os.environ, "_POLL_PROBE": "1" if probe else "0"}
        result = subprocess.run([sys.executable, __file__], stdout=subprocess.PIPE, text=True,
                                encoding="utf-8", env=env)
        out = result.stdout.rstrip("\n")
        # Real passes print only what the caller handles; a dry run also shows unclaim lines.
        if any(not line.startswith('{"kind":"unclaim"') for line in out.split("\n") if line):
            print(out, flush=True)
            return result.returncode
        if result.returncode == 0:
            failures = 0
            probe = True
        else:
            failures += 1
            if failures >= int(os.environ.get("MAX_FAILURES", "10")):
                log(f"pass failed {failures} times in a row; giving up")
                return 1
            log(f"pass failed (exit {result.returncode}); retrying in {INTERVAL}s")
        time.sleep(INTERVAL)


WATERMARKS = """
  latestIssue: issues(first: 1, orderBy: updatedAt) { nodes { id updatedAt } }
  latestComment: comments(first: 1, orderBy: updatedAt, filter: { createdAt: { gt: "-P7D" } }) {
    nodes { id updatedAt }
  }
  latestSession: agentSessions(first: 1, orderBy: updatedAt) { nodes { id updatedAt } }"""

PROBE_QUERY = "query {" + WATERMARKS + "\n}"

USER = "{ id name app guest }"
NOTE = "{ id createdAt body url user " + USER + " }"


# This machine's app and the machines' apps (APPS) with their names now (Machines).
APPS_QUERY = """query($ids: [ID!]) { viewer { id name displayName }
  apps: users(first: 50, includeDisabled: true, filter: { id: { in: $ids } }) { nodes { id name displayName } } }"""


def handles(app):
    """What a comment may name an app by: its name, or its display name in a profile URL."""
    return sorted({app["name"], app["displayName"]})


def comments_query(apps):
    """Every comment written since the checkpoint (`since`, poll_ledger.py), page by page (MS-385): a
    single page of the newest 100 lost the oldest ones after an hour's stop at busy times. Only those
    on issues delegated to this machine (`$me`, its app ids) or naming one of the machines' apps
    (Machines)."""
    named = "".join(f", {{ body: {{ contains: {json.dumps(prefix + handle)} }} }}"
                    for app in apps for handle in handles(app) for prefix in ("@", "profiles/"))
    return """comments(first: 100, after: $after, filter: { createdAt: { gt: $since },
      or: [{ issue: { delegate: { id: { in: $me } } } }""" + named + """] }) {
    nodes { id createdAt body user { id name app guest }
      issue { id identifier title delegate { id } state { name } } }
    pageInfo { hasNextPage endCursor } }"""


def comments_page(apps):
    return "query($me: [ID!], $since: DateTimeOrDuration!, $after: String) {\n  " + comments_query(apps) + "\n}"


# The issues of overdue ledger items: whether they are finished, still this poller's, held by an agent.
LEDGER_ISSUES = """query($ids: [ID!], $n: Int!) { issues(first: $n, filter: { id: { in: $ids } }) {
  nodes { id identifier delegate { id } state { name type } labels { nodes { name } }
    comments(first: 50) { nodes { createdAt body } } } } }"""


def query(apps):
    return """query($me: [ID!], $claimed: String!, $install: String!, $sessions: Int!, $since: DateTimeOrDuration!, $after: String) {""" + WATERMARKS + """
  queued: issues(first: 100, filter: {
    delegate: { id: { in: $me } }, state: { type: { in: ["triage", "backlog", "unstarted"] } } }) {
    nodes { id identifier number title description priority priorityLabel state { name }
      creator { id name app guest } project { name } parent { identifier title }
      labels { nodes { id name } }
      inverseRelations(first: 50) { nodes { type issue { id identifier state { name type }
        labels { nodes { name } } } } } } }
  installs: issues(first: 100, filter: {
    delegate: { id: { in: $me } }, labels: { some: { name: { eq: $install } } },
    state: { type: { nin: ["completed", "canceled", "duplicate"] } } }) {
    nodes { identifier number } }
  claimed: issues(first: 100, filter: {
    delegate: { id: { in: $me } }, labels: { some: { name: { eq: $claimed } } } }) {
    nodes { id identifier title state { name } labels { nodes { id name } }
      comments(first: 100) { nodes { id createdAt body user { app } } } } }
  """ + comments_query(apps) + """
  sessions: agentSessions(first: $sessions, orderBy: updatedAt) {
    nodes { id createdAt dismissedAt appUser { id } creator """ + USER + """
      comment """ + NOTE + """ sourceComment """ + NOTE + """
      activities(first: 10, filter: { type: { eq: "prompt" } }) {
        nodes { id createdAt signal user """ + USER + """ sourceComment """ + NOTE + """
          content { ... on AgentActivityPromptContent { body } } } }
      issue { id identifier number title description priority priorityLabel team { key } state { name }
        creator { id name app guest } project { name } parent { identifier title }
        labels { nodes { id name } } comments(first: 20) { nodes { id createdAt body url user { id } } } } } }
}"""


def labelled(issue):
    return next((l["id"] for l in issue["labels"]["nodes"] if l["name"] == LABEL), None)


def mine(issue, me):
    """Whether an issue belongs to this machine: it is delegated to this machine's app, or one of
    its FORMER apps (see Machines)."""
    return (issue.get("delegate") or {}).get("id") in me["ids"]


def member(user):
    return user is not None and user.get("app") is not True and user.get("guest") is not True


def mentions(body, name):
    """`@name` as typed, or a real mention, which the API writes as the profile URL."""
    return re.search(rf"(?:@|linear\.app/[\w-]+/profiles/){re.escape(name)}(?![\w-])", body) is not None


def named(body, apps):
    """The ids of the machines' apps a comment names."""
    return {app["id"] for app in apps if any(mentions(body, handle) for handle in handles(app))}


def routed(comment, me, apps):
    """Whether a comment goes to this poller: see "Machines" above."""
    user, body = comment["user"], comment["body"]
    if user is None or user["id"] in me["ids"]:
        return False
    names, ours = named(body, apps), set(me["ids"])
    if member(user):
        return bool(names & ours) if names else mine(comment["issue"], me)
    return user.get("app") is True and user["id"] in APPS and bool(names & ours)


def blockers(issue):
    return [r["issue"] for r in issue["inverseRelations"]["nodes"] if r["type"] == "blocks"]


def resolved(issue):
    return (issue["state"]["type"] in ("completed", "canceled", "duplicate") or issue["state"]["name"] == "In Review"
            or any(l["name"] == INSTALL for l in issue["labels"]["nodes"]))


def about(issue):
    """What the caller needs to hand an issue out without asking Linear: see the issue line above."""
    out = {"title": issue["title"], "state": issue["state"]["name"],
           "priority": issue["priorityLabel"], "priorityValue": issue["priority"],
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


def comment_line(comment, issue, react):
    return {"kind": "comment", "id": comment["id"], "issue": issue["identifier"], "issueId": issue["id"],
            "title": issue["title"], "state": issue["state"]["name"], "createdAt": comment["createdAt"],
            "author": comment["user"]["name"], "body": comment["body"], "react": react}


def thought(body):
    return {"type": "thought", "body": f"{TAG} {body}"}


NOT_MEMBER = {"type": "error", "body": f"{TAG} 只接受工作区正式成员的请求：仓库是公开的，agent 有完整权限。"}
NO_ISSUE = "{tag} 只处理 issue 上的会话：请在 issue 里指派或提及 {app}。"


def prompts(session):
    """The user's messages in a session, oldest first."""
    return sorted(session["activities"]["nodes"], key=lambda p: p["createdAt"])


def opener(session):
    """The member's comment that opened a session by mentioning the app; None for a delegation."""
    notes = [session["sourceComment"], session["comment"]] + [p["sourceComment"] for p in prompts(session)]
    return next((c for c in notes if c and member(c["user"])), None)


def summary(comment, issue):
    """The start of omp's hand-over comment and its link, for the session's answer."""
    text = PREFIX.sub("", comment["body"]).strip().split("\n\n")[0]
    if len(text) > 300:
        text = text[:300].rstrip() + "…"
    return f"{TAG} {issue['identifier']}（{issue['state']['name']}）：{text}\n\n{comment['url']}"


def plan_sessions(data, known, done, handled, todo):
    """Issue lines, comment and stop lines, and replies for the app's agent sessions (see Agent
    sessions above). A line's `reply` is the activity to post as it is printed; it may attach to an
    issue line already in todo."""
    me, ours, app = data["viewer"]["id"], data["viewer"]["ids"], data["viewer"]["name"]
    no_issue = {"type": "error", "body": NO_ISSUE.format(tag=TAG, app=app)}
    issues, notes, replies = [], [], []
    for session in (data.get("sessions") or {}).get("nodes") or []:
        if session["appUser"]["id"] != me or session["dismissedAt"]:
            continue
        sid, issue, entry = session["id"], session["issue"], known.get(session["id"])

        def reply(units, content, closes=False):
            return {"session": sid, "units": units, "content": content, "closes": closes}

        if entry is None:
            units = ["created"] + [p["id"] for p in prompts(session)]
            if not member(session["creator"]) or issue is None:
                replies.append(reply(units, NOT_MEMBER if not member(session["creator"]) else no_issue, True))
                continue
            ack = thought(f"收到，已交给 {app} 这台机器上的 omp agent 处理 {issue['identifier']}；它交接时结果会发在这里。")
            note = opener(session)
            line = next((l for l in todo if l["id"] == issue["id"]), None)
            if note is not None and note["id"] not in handled:
                notes.append({**comment_line(note, issue, True), "session": sid, "reply": reply(units, ack)})
            elif note is None and line is not None:
                line.update({"session": sid, "reply": reply(units, ack)})
            elif note is None and not (issue["state"]["name"] == "In Progress"
                                       and any(l["name"] == CLAIMED for l in issue["labels"]["nodes"])):
                claimed = issue["id"] in done
                issues.append({"kind": "issue", "id": issue["id"], "identifier": issue["identifier"], "resume": claimed,
                               "unblockedBy": [], **about(issue), "session": sid, "label": None, "unblockKey": None,
                               "number": issue["number"], "react": not claimed, "reply": reply(units, ack)})
            else:  # its comment was already printed, or an agent already holds the issue
                replies.append(reply(units, thought(f"收到；{issue['identifier']} 已有 agent 在处理，它交接时结果会发在这里。")))
            continue
        new = [p for p in prompts(session) if p["id"] not in entry["units"]]
        for prompt in new:
            if not member(prompt["user"]) or issue is None:
                replies.append(reply([prompt["id"]], NOT_MEMBER if not member(prompt["user"]) else no_issue))
            elif prompt["signal"] == "stop":
                notes.append({"kind": "stop", "session": sid, "issue": issue["identifier"], "issueId": issue["id"],
                              "title": issue["title"], "author": prompt["user"]["name"], "createdAt": prompt["createdAt"],
                              "reply": reply([prompt["id"]], {"type": "response", "body": f"{TAG} 已请 {app} 的轮询会话停下 {issue['identifier']} 的 agent。"}, True)})
            else:
                note = prompt["sourceComment"] or {"id": prompt["id"], "createdAt": prompt["createdAt"], "user": prompt["user"],
                                                   "body": (prompt["content"] or {}).get("body") or ""}
                ack = thought(f"收到，已转给处理 {issue['identifier']} 的 agent；它交接时结果会发在这里。")
                if note["id"] in handled:
                    replies.append(reply([prompt["id"]], ack))
                else:
                    notes.append({**comment_line(note, issue, prompt["sourceComment"] is not None), "session": sid,
                                  "reply": reply([prompt["id"]], ack)})
        if new or issue is None or not entry.get("ackedAt") or entry.get("answered"):
            continue
        acked = timestamp(entry["ackedAt"])
        after = [c for c in issue["comments"]["nodes"] if (c["user"] or {}).get("id") in ours
                 and timestamp(c["createdAt"]) > acked and not START.match(c["body"])]
        question = max((c for c in after if QUESTION in c["body"]), key=lambda c: c["createdAt"], default=None)
        if question is not None:
            replies.append(reply([], {"type": "elicitation", "body": summary(question, issue)}, True))
        elif after and issue["state"]["name"] not in WORKING_STATES:
            final = max(after, key=lambda c: c["createdAt"])
            replies.append(reply([], {"type": "response", "body": summary(final, issue)}, True))
    return issues, notes, [{"kind": "reply", "reply": r} for r in replies]


def plan(data, running, known):
    """Issue lines oldest first, then install, stale and unclaim lines, comment and stop lines oldest
    first, and the replies to agent sessions."""
    me = data["viewer"]
    done = [line.split(" ")[0] for line in lines(LAUNCHED)]
    handled, freed, reported = lines(HANDLED), lines(UNBLOCKED), lines(STALE)
    now = datetime.now(timezone.utc)
    todo = []
    for issue in data["queued"]["nodes"]:  # every one is delegated to this machine (the query's filter)
        claimed = issue["id"] in done
        blocking = blockers(issue)
        free = all(resolved(b) for b in blocking)
        key = f"{issue['id']} {','.join(sorted(b['id'] for b in blocking))}" if blocking and free else None
        creator = issue["creator"] or {}
        trusted = member(creator) or (creator.get("app") is True and creator.get("id") in APPS)
        unblocked = claimed and key is not None and key not in freed
        if labelled(issue) is not None or (free and ((trusted and not claimed) or unblocked)):
            todo.append({"kind": "issue", "id": issue["id"], "identifier": issue["identifier"], "resume": claimed,
                         "unblockedBy": [b["identifier"] for b in blocking if resolved(b)], **about(issue),
                         "label": labelled(issue), "unblockKey": key, "number": issue["number"], "react": not claimed})
    issues, notes, replies = plan_sessions(data, known, done, handled, todo)
    out = sorted(todo + issues, key=lambda line: line["number"])
    for line in out:
        del line["number"]
    waiting = sorted(data["installs"]["nodes"], key=lambda i: i["number"])
    last = INSTALLED.read_text(encoding="utf-8").strip()
    if (waiting and not any(agent.startswith("Install") for agent in running or [])
            and (not last or (now - timestamp(last)).total_seconds() >= INSTALL_EVERY)):
        out.append({"kind": "install", "issues": [i["identifier"] for i in waiting]})
    if running is not None:
        for issue in data["claimed"]["nodes"]:
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
    covered = {line["id"] for line in notes if line["kind"] == "comment"}
    for comment in data["comments"]["nodes"]:
        if routed(comment, me, data["apps"]) and comment["id"] not in handled and comment["id"] not in covered:
            notes.append(comment_line(comment, comment["issue"], member(comment["user"])))
    return out + sorted(notes, key=lambda line: line["createdAt"]) + replies


def watermark(data):
    def head(key):
        nodes = (data.get(key) or {}).get("nodes") or []
        return [nodes[0]["id"], nodes[0]["updatedAt"]] if nodes else None
    return {"issue": head("latestIssue"), "comment": head("latestComment"), "session": head("latestSession")}


def probe_context(running):
    return {
        "label": LABEL,
        "claimed": CLAIMED,
        "install": INSTALL,
        "apps": APPS,
        "running": running,
        "mtime": Path(__file__).stat().st_mtime_ns,
    }


def unchanged(running):
    if os.environ.get("_POLL_PROBE") != "1" or FULL_EVERY <= 0 or os.environ.get("DRY_RUN"):
        return False
    try:
        saved = json.loads(PROBE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    if not isinstance(saved, dict) or saved.get("context") != probe_context(running):
        return False
    if time.time() - saved.get("fullAt", 0) >= FULL_EVERY:
        return False
    return watermark(gql(PROBE_QUERY, {})) == saved.get("watermark")


def answer(known, reply):
    """Records a reply in `sessions` and posts it: a thought acknowledges (the hand-over answer is
    then awaited), a closing reply ends the wait."""
    entry = known.setdefault(reply["session"], {"units": [], "ackedAt": None, "answered": False})
    entry["units"] += reply["units"]
    if reply["content"]["type"] == "thought":
        entry["ackedAt"], entry["answered"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), False
    if reply["closes"]:
        entry["answered"] = True
    temporary = SESSIONS.with_suffix(".tmp")
    temporary.write_text(json.dumps(known, ensure_ascii=False), encoding="utf-8")
    temporary.replace(SESSIONS)
    activity(reply["session"], reply["content"])


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
    if unchanged(running):
        return 0
    started = datetime.now(timezone.utc)
    ledger = poll_ledger.Ledger(STATE, dry=bool(os.environ.get("DRY_RUN")))
    machines = gql(APPS_QUERY, {"ids": APPS})
    me, apps = machines["viewer"], machines["apps"]["nodes"]
    me["ids"] = [me["id"], *FORMER.get(me["id"], [])]
    page_vars = {"me": me["ids"], "since": ledger.rewind(), "after": None}
    if me["id"] not in {app["id"] for app in apps}:
        apps.append(me)
    data = {**gql(query(apps), {**page_vars, "claimed": CLAIMED, "install": INSTALL, "sessions": SESSION_COUNT}),
            "viewer": me, "apps": apps}
    page = data["comments"]
    while page["pageInfo"]["hasNextPage"]:
        page = gql(comments_page(apps), {**page_vars, "after": page["pageInfo"]["endCursor"]})["comments"]
        data["comments"]["nodes"] += page["nodes"]
    if not os.environ.get("DRY_RUN"):
        PROBE.write_text(json.dumps({
            "watermark": watermark(data),
            "context": probe_context(running),
            "fullAt": time.time(),
        }), encoding="utf-8")
    known = json.loads(SESSIONS.read_text(encoding="utf-8")) if SESSIONS.exists() else {}
    ledger.answer(data["comments"]["nodes"], me["ids"])
    out = plan(data, running, known)
    printed = {line["id"] for line in out if line["kind"] in ("issue", "comment")}
    again = ledger.overdue(lambda ids: gql(LEDGER_ISSUES, {"ids": ids, "n": len(ids)})["issues"]["nodes"], running,
                           lambda issue: mine(issue, data["viewer"]))
    out += [line for line in again if line["id"] not in printed]
    if os.environ.get("DRY_RUN"):
        for line in out:
            emit(line)
        return 0
    for line in out:
        if "overdue" in line:  # printed before: its claim is done, only the ledger notes it again
            ledger.handed(line)
            emit(line)
            continue
        kind = line["kind"]
        react_to = line.pop("react", False)
        reply = line.pop("reply", None)
        if kind == "comment":
            append(HANDLED, line["id"])
        elif kind == "stale":
            append(STALE, line.pop("staleKey"))
        elif kind == "install":
            INSTALLED.write_text(datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ") + "\n", encoding="utf-8")
        elif kind == "unclaim":
            remove_label(line["id"], line["label"])
        elif kind == "issue":
            label, key = line.pop("label"), line.pop("unblockKey")
            if not line["resume"]:
                append(LAUNCHED, f"{line['id']} {line['identifier']}")
            if key:
                append(UNBLOCKED, key)
            if label:
                remove_label(line["id"], label)
        if react_to:
            react(kind, line["id"])
        if reply:
            answer(known, reply)
        if kind in ("issue", "comment"):
            ledger.handed(line)
        if kind not in ("unclaim", "reply"):
            emit(line)
    ledger.advance(started)
    return 0


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    args = sys.argv[1:]
    try:
        if args == ["watch"]:
            return watch()
        if args[:1] == ["ledger"]:
            ledger = poll_ledger.Ledger(STATE)
            if args[1:2] == ["note"] and len(args) >= 4:
                ledger.note(args[2], " ".join(args[3:]))
                return 0
            if len(args) <= 2 and all(a.isdigit() for a in args[1:]):
                print(ledger.report(int(args[1]) if args[1:] else 72))
                return 0
        if args:
            print(f"usage: {sys.argv[0]} [watch | ledger [HOURS] | ledger note MS-12 TEXT]", file=sys.stderr)
            return 2
        return run_pass()
    except Failed:
        return 1


if __name__ == "__main__":
    sys.exit(main())
