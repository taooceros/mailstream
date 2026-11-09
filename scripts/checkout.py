#!/usr/bin/env python3
"""Opens an agent's ovnfs checkout of the default checkout (~/projects/mailstream) at `main`, with
its build folders frozen as they are now, and sets up jj there (AGENTS.md, Version control).

Usage: python3 ~/projects/mailstream/scripts/checkout.py NAME

It runs `ovnfs new --rev main --name NAME` with SNAPSHOT_RULE, then `jj git init --colocate`, `jj
bookmark track main@origin`, `jj git fetch` and `jj new main` in the new checkout (it starts at the
default checkout's `main`, which can lag `origin`), and prints the checkout's path.

ovnfs's built-in rules make ignored folders (`cache`) live views of the default checkout's: a
build or delete there after the checkout opened showed up in it, and cargo kept artifacts built
from the default checkout's source over the checkout's own edits (MS-152). SNAPSHOT_RULE makes
the warm build's folders (every `target/`, `.turso/`) part of the checkout's captured base
instead: ovnfs hashes them and keeps an APFS clone of each file (no extra disk until the default
checkout replaces it, ~3 s for its 6 GB), so the checkout keeps the warm build as it was when it
opened. warm-build.py refuses only checkouts opened without it."""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SNAPSHOT_RULE = "source ignored & (**/target | **/target/** | .turso | .turso/**)"


def run(*args, cwd=ROOT):
    return subprocess.run(args, cwd=cwd, check=True, text=True, capture_output=True).stdout


def default_checkout():
    """The default checkout, also when this script runs from one of its checkouts."""
    for checkout in json.loads(run("ovnfs", "--json", "ls")).get("checkouts") or []:
        if Path(checkout["path"]).resolve() == ROOT:
            return Path(checkout["source"])
    return ROOT


def main():
    if len(sys.argv) != 2 or sys.argv[1].startswith("-"):
        sys.exit(__doc__.split("\n\n")[1])
    opened = json.loads(run("ovnfs", "--json", "new", str(default_checkout()), "--rev", "main",
                            "--name", sys.argv[1], "--rule", SNAPSHOT_RULE))
    path = Path(opened["checkout"]["path"])
    capture = opened["capture"]
    print(f"Opened {path}: captured {capture['files']} files, build folders included "
          f"({capture['bytes'] / 1e9:.1f} GB), in {capture['seconds']:.1f} s", flush=True)

    def jj(*args):
        return run("jj", "--no-pager", *args, cwd=path)

    jj("git", "init", "--colocate")
    jj("bookmark", "track", "main@origin")
    jj("git", "fetch")
    jj("new", "main")
    print(path, "at main", jj("log", "--no-graph", "-r", "main",
                              "-T", 'commit_id.short() ++ " " ++ description.first_line()'))


try:
    main()
except subprocess.CalledProcessError as error:
    if error.stderr:
        sys.stderr.write(error.stderr)
    sys.exit(f"checkout: {' '.join(map(str, error.cmd))} failed ({error.returncode})")
