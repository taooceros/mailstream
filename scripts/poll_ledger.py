"""What linear-poll.py handed out and whether this machine answered it (MS-385). Python 3.9+
standard library only, like the poller.

The poller used to remember a hand-out only as "claimed" (`launched`, `comments`), so a line its
session dropped (a crash, a hand-over, a decision never written on the issue) was never handed out
again, and nothing showed what was missed. The ledger, STATE/ledger.jsonl, is an append-only list
of JSON events, one per line, each with `at` (UTC, `%Y-%m-%dT%H:%M:%SZ`) and `event`:

  handed    {"line"}: an issue or comment line the poller printed, as printed (an overdue line
            again with its `overdue` minutes). Its item is `line.id`.
  answered  {"id", "issue", "by", "comment", "minutes"}: this machine's app commented on the
            item's issue after the item (a comment line: after the comment was written; an issue
            line: after it was first handed out). by is the agent id of a start comment, else the
            comment's `[omp @ <checkout> …]` prefix, else the app; minutes since the item.
  closed    {"id", "issue", "state"}: the issue was finished (completed, canceled, duplicate), is
            gone or is no longer this machine's (`state` says which) before anyone answered.
  gap       {"from", "to"}: the checkpoint (below) did not move between these times for longer
            than GAP seconds (default 300): the poller was stopped, or Linear or the network kept
            failing.
  note      {"id", "issue", "text"}: a remark written by hand (`linear-poll.py ledger note`).

Checkpoint and rewind (the user's idea): STATE/checkpoint holds the start of the last full pass
whose lines were all handed out and recorded. Each full pass rewinds to it less 10 minutes (at most
7 days back; on a machine's first pass, 10 minutes back from now) and fetches every comment
written since, page by page, skipping by id those already handed out; only once every line is
handled does the checkpoint move. So a timeout, restart, warm build or crash replays its window
instead of losing it, and a comment never falls out of a fixed-size page.

An item is open from its first `handed` until `answered` or `closed`. Each full pass reads the
app's own comments among the replayed ones and records the answers. At most every CHECK
seconds (default 300, kept in STATE/ledger-checked), an open item last handed OVERDUE seconds ago
(default 1800) whose issue no running agent holds (the agent of its latest start comment is not in
RUNNING) is printed again with `overdue` (minutes since first handed), and again every OVERDUE
seconds until it is answered or closed: leaving it to another machine counts only once that is
written on the issue. Without RUNNING nothing is printed again. With DRY_RUN nothing is written.

`linear-poll.py ledger [HOURS]` prints the items of the last HOURS (default 72) that were handed out
more than once or are still open, with who answered and how late, and the gaps;
`linear-poll.py ledger note MS-12 TEXT` adds a note to the item last handed on that issue.
"""
import json
import os
import re
from datetime import datetime, timedelta, timezone

OVERDUE = int(os.environ.get("OVERDUE", "1800"))
CHECK = int(os.environ.get("CHECK", "300"))
GAP = int(os.environ.get("GAP", "300"))
KEEP_DAYS = 14  # finished items and gaps older than this are dropped once the file passes MAX_BYTES
MAX_BYTES = 2_000_000
START = re.compile(r"^\[omp @ [^\]]*\] start \((?P<a>[^)]+)\)")
PREFIX = re.compile(r"^\[omp @ (?P<p>[^\]]*)\]")
FINISHED = ("completed", "canceled", "duplicate")


def now():
    return datetime.now(timezone.utc)


def stamp(when):
    return when.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse(text):
    return datetime.strptime(re.sub(r"\.[0-9]+Z$", "Z", text), "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def minutes(since, until):
    return round((until - since).total_seconds() / 60)


def author(comment):
    start = START.match(comment["body"])
    if start:
        return start.group("a")
    prefix = PREFIX.match(comment["body"])
    return prefix.group("p") if prefix else comment["user"]["name"]


class Ledger:
    def __init__(self, state, dry=False):
        self.path = state / "ledger.jsonl"
        self.checkpoint_path = state / "checkpoint"
        self.checked_path = state / "ledger-checked"
        self.dry = dry
        self.events = []
        if self.path.exists():
            self.events = [json.loads(text) for text in self.path.read_text(encoding="utf-8").splitlines() if text.strip()]

    def write(self, event):
        event = {"at": stamp(now()), **event}
        self.events.append(event)
        if not self.dry:
            with self.path.open("a", encoding="utf-8", newline="\n") as f:
                f.write(json.dumps(event, ensure_ascii=False, separators=(",", ":")) + "\n")

    def handed(self, line):
        self.write({"event": "handed", "line": line})

    def items(self):
        """Per item id: its latest line, first and last hand-out, hand-outs, notes and how it ended."""
        out = {}
        for event in self.events:
            if event["event"] == "handed":
                line = event["line"]
                item = out.setdefault(line["id"], {"first": event["at"], "handed": 0, "end": None, "notes": []})
                item.update(line=line, last=event["at"], handed=item["handed"] + 1)
            elif event["event"] in ("answered", "closed", "note") and event["id"] in out:
                item = out[event["id"]]
                if event["event"] == "note":
                    item["notes"].append(event["text"])
                elif item["end"] is None:
                    item["end"] = event
        return out

    def open_items(self):
        return {id: item for id, item in self.items().items() if item["end"] is None}

    @staticmethod
    def issue_of(id, item):
        return item["line"]["issueId"] if item["line"]["kind"] == "comment" else id

    @staticmethod
    def since_item(item):
        return parse(item["line"]["createdAt"] if item["line"]["kind"] == "comment" else item["first"])

    # Checkpoint and rewind: see the header.

    def checkpoint(self):
        try:
            return parse(self.checkpoint_path.read_text(encoding="utf-8").strip())
        except (OSError, ValueError):
            return None

    def rewind(self, overlap=600, longest=timedelta(days=7)):
        """Where the pass's comment query starts: the checkpoint less `overlap` seconds, at most
        7 days back. Without a checkpoint (the first pass on a machine) it starts `overlap` back
        from now rather than replaying a week that other polls already handled."""
        last = self.checkpoint() or now()
        return stamp(max(now() - longest, last - timedelta(seconds=overlap)))

    def advance(self, started):
        """Moves the checkpoint to `started`, the start of a full pass whose lines were all handled,
        and records the gap since the previous one."""
        last = self.checkpoint()
        if last is not None and (started - last).total_seconds() > GAP:
            self.write({"event": "gap", "from": stamp(last), "to": stamp(started)})
        if not self.dry:
            self.checkpoint_path.write_text(stamp(started) + "\n", encoding="utf-8")
            self.compact()

    # Answers and overdue items.

    def answer(self, comments, me):
        """Records the items this machine's apps (`me`, their user ids: its app and FORMER ones,
        linear-poll.py) answered among `comments`."""
        waiting = {}
        for id, item in self.open_items().items():
            waiting.setdefault(self.issue_of(id, item), []).append((id, item))
        mine = sorted((c for c in comments if (c.get("user") or {}).get("id") in me and c["issue"]["id"] in waiting),
                      key=lambda c: c["createdAt"])
        for comment in mine:
            at, items = parse(comment["createdAt"]), waiting[comment["issue"]["id"]]
            for id, item in [(id, item) for id, item in items if at > self.since_item(item)]:
                self.write({"event": "answered", "id": id, "issue": comment["issue"]["identifier"],
                            "by": author(comment), "comment": comment["id"], "minutes": minutes(self.since_item(item), at)})
                items.remove((id, item))

    def overdue(self, issues, running, mine):
        """Lines to print again. `issues(ids)` returns those issues with `state { name type }` and
        their comments' `createdAt` and `body`; `mine(issue)` says whether one is still this machine's."""
        if running is None:
            return []
        due = {id: item for id, item in self.open_items().items()
               if (now() - parse(item["last"])).total_seconds() >= OVERDUE}
        try:
            checked = parse(self.checked_path.read_text(encoding="utf-8").strip())
        except (OSError, ValueError):
            checked = None
        if not due or (checked is not None and (now() - checked).total_seconds() < CHECK):
            return []
        if not self.dry:
            self.checked_path.write_text(stamp(now()) + "\n", encoding="utf-8")
        keys = {id: self.issue_of(id, item) for id, item in due.items()}
        found = {issue["id"]: issue for issue in issues(sorted(set(keys.values())))}
        out = []
        for id, item in due.items():
            issue, line = found.get(keys[id]), item["line"]
            if issue is None or issue["state"]["type"] in FINISHED or not mine(issue):
                state = ("gone" if issue is None else issue["state"]["name"] if issue["state"]["type"] in FINISHED
                         else "not mine")
                self.write({"event": "closed", "id": id, "issue": line.get("issue") or line.get("identifier"),
                            "state": state})
                continue
            starts = [c for c in issue["comments"]["nodes"] if START.match(c["body"])]
            start = max(starts, key=lambda c: c["createdAt"]) if starts else None
            if start is not None and START.match(start["body"]).group("a") in running:
                continue
            out.append({**line, "overdue": minutes(parse(item["first"]), now())})
        return out

    # Upkeep and reports.

    def compact(self):
        """Drops finished items and gaps older than KEEP_DAYS once the file passes MAX_BYTES."""
        if not self.path.exists() or self.path.stat().st_size < MAX_BYTES:
            return
        cutoff = stamp(now() - timedelta(days=KEEP_DAYS))
        done = {id for id, item in self.items().items() if item["end"] is not None and item["end"]["at"] < cutoff}
        keep = [e for e in self.events
                if not (e["event"] == "gap" and e["to"] < cutoff)
                and (e["line"]["id"] if e["event"] == "handed" else e.get("id")) not in done]
        temporary = self.path.with_suffix(".tmp")
        temporary.write_text("".join(json.dumps(e, ensure_ascii=False, separators=(",", ":")) + "\n" for e in keep),
                             encoding="utf-8")
        temporary.replace(self.path)
        self.events = keep

    def note(self, issue, text):
        """Adds a remark to the item last handed on `issue` (an identifier such as MS-12)."""
        found = [e["line"] for e in self.events if e["event"] == "handed"
                 and issue in (e["line"].get("issue"), e["line"].get("identifier"))]
        if not found:
            raise SystemExit(f"nothing handed out on {issue} in the ledger")
        self.write({"event": "note", "id": found[-1]["id"], "issue": issue, "text": text})

    def report(self, hours):
        cutoff = stamp(now() - timedelta(hours=hours))
        rows = []
        for item in sorted(self.items().values(), key=lambda item: item["first"]):
            end = item["end"]
            if item["last"] < cutoff or (item["handed"] < 2 and end is not None and not item["notes"]):
                continue
            line = item["line"]
            what = (f"{line['issue']} comment by {line.get('author')}: {line.get('body', '')[:60]!r}"
                    if line["kind"] == "comment" else f"{line['identifier']} {line.get('title', '')[:60]}")
            if end is None:
                outcome = f"OPEN for {minutes(parse(item['first']), now())} min"
            elif end["event"] == "answered":
                outcome = f"answered by {end['by']} after {end['minutes']} min"
            else:
                outcome = f"closed ({end['state']})"
            notes = "".join(f"\n    note: {n}" for n in item["notes"])
            rows.append(f"{item['first']}  {what}\n    handed {item['handed']}x, {outcome}{notes}")
        gaps = [f"  {e['from']} → {e['to']}" for e in self.events if e["event"] == "gap" and e["to"] >= cutoff]
        return "\n".join([f"Handed out more than once, noted or still open (last {hours} h):"] + (rows or ["  none"])
                         + [f"Gaps (no full pass for over {GAP} s: poller stopped or failing):"] + (gaps or ["  none"]))
