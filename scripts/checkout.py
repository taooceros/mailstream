#!/usr/bin/env python3
"""Opens an agent's ovnfs checkout of the default checkout (~/projects/mailstream), with its build
folders frozen as they are now, and sets up jj there at `main` (.omp/skills/ovnfs/SKILL.md).

Usage: python3 ~/projects/mailstream/scripts/checkout.py NAME

It runs `ovnfs new ~/projects/mailstream --name NAME`, checks that the repo's `.o2/ovnfs.yml` took
effect (frozen() below), then runs `jj git init --colocate`, `jj bookmark track main@origin`, `jj
git fetch` and `jj new main` in the checkout and prints its path.

`.o2/ovnfs.yml` makes the warm build's folders (every `target/`, `.turso/`) `source`, so ovnfs
captures them when the checkout opens (APFS clones) instead of serving the default checkout's
live: a later build or delete there no longer reaches the checkout, and cargo can't keep an
artifact built from other source (MS-152). It hides the default checkout's `.jj`, so the
checkout's jj starts its own. No `--rev`: ovnfs v0.3.0 serves ignored files of a `--rev`
checkout live whatever the rules say; the default checkout is clean on `main` anyway (it is
locked, and only warm-build.py moves it), and `jj new main` brings the checkout to `origin`."""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ["FFI/target", "Engine/target", ".turso"]


def run(*args, cwd=ROOT):
    return subprocess.run(args, cwd=cwd, check=True, text=True, capture_output=True).stdout


def ovnfs(*args):
    return json.loads(run("ovnfs", "--json", *args))


def default_checkout():
    """The default checkout, also when this script runs from one of its checkouts."""
    for checkout in ovnfs("ls").get("checkouts") or []:
        if Path(checkout["path"]).resolve() == ROOT:
            return Path(checkout["source"])
    return ROOT


def frozen(checkout):
    """Whether a checkout (an `ovnfs --json ls` entry) keeps the build folders it opened with:
    opened without `--rev` (ovnfs keeps a `revision-index` in the base of a `--rev` one) and with
    the build folders `source` and `.jj` hidden."""
    if (Path(ovnfs("status")["state"]) / "bases" / checkout["base"] / "revision-index").exists():
        return False
    def kind(path):
        return ovnfs("explain", checkout["name"], path)["class"]
    return kind(".jj") == "hidden" and all(kind(path) == "source" for path in BUILD)


def main():
    if len(sys.argv) != 2 or sys.argv[1].startswith("-"):
        sys.exit(__doc__.split("\n\n")[1])
    opened = ovnfs("new", str(default_checkout()), "--name", sys.argv[1])
    checkout = opened["checkout"]
    path = Path(checkout["path"])
    if not frozen(checkout):
        run("ovnfs", "rm", checkout["name"])
        sys.exit("checkout: the default checkout has no .o2/ovnfs.yml freezing the build folders "
                 "yet (the next scripts/warm-build.py there brings it); removed " + checkout["name"])
    capture = opened["capture"]
    print(f"Opened {path}: {capture['files']} files ({capture['bytes'] / 1e9:.1f} GB, build "
          f"folders included) in {capture['seconds']:.1f} s", flush=True)

    def jj(*args):
        return run("jj", "--no-pager", *args, cwd=path)

    jj("git", "init", "--colocate")
    jj("bookmark", "track", "main@origin")
    jj("git", "fetch")
    jj("new", "main")
    print(path, "at main", jj("log", "--no-graph", "-r", "main",
                              "-T", 'commit_id.short() ++ " " ++ description.first_line()'))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        if error.stderr:
            sys.stderr.write(error.stderr)
        sys.exit(f"checkout: {' '.join(map(str, error.cmd))} failed ({error.returncode})")
