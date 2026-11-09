#!/usr/bin/env python3
"""Refreshes the warm build of `main` in the default checkout (~/projects/mailstream), which every
new ovnfs checkout starts from as its own `target/` and `.turso/` (MS-151): the release engine,
command line, Apple Intelligence tagger and Dart binding generator (`CONFIG=release
scripts/ffi.py`, into FFI/target), the engine's tests (`cargo test --no-run` in Engine/, into
Engine/target/debug) and the Turso library (`scripts/turso.sh`). It builds without the toolchain
variables ffi.py drops (SDKROOT, CPATH, …): agents' shells don't set them, and a warm build made
with them rebuilt 15 crates in every checkout (MS-159).

Usage: python3 scripts/warm-build.py [--allow CHECKOUT ...]

Runs only in the default checkout, with no source changes there. It fetches `main` and moves the
default checkout's empty working-copy change onto it, always; then it builds only while every
open ovnfs checkout of it froze these folders when it opened (scripts/checkout.py; those keep the
build they opened with): any other (opened with `ovnfs new` by hand, `--rev` or before MS-152)
reads them live, so a build under it can hand it artifacts built from other source (MS-152), and
it exits 1 instead. --allow names such a checkout that builds nothing and may stay. Tracked files
end as `main` has them and nothing else in the tree changes (the Dart bindings go to a temporary
folder). It unlocks the default checkout's source for its run and locks it again when it ends,
failed or not (scripts/source_lock.py, MS-156)."""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ["FFI/target", "Engine/target", ".turso"]
sys.path.insert(0, str(ROOT / "scripts"))
sys.dont_write_bytecode = True
from ffi import DRIFTING  # noqa: E402
from checkout import frozen  # noqa: E402
import source_lock  # noqa: E402
ENV = {k: v for k, v in os.environ.items() if k not in DRIFTING}


def run(*args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, text=True, capture_output=True, **kwargs).stdout


def jj(*args):
    return run("jj", "--no-pager", *args)


def fail(message):
    sys.exit(f"warm-build: {message}")


def checkouts():
    """The open ovnfs checkouts of this folder (`ovnfs --json ls` entries)."""
    listing = json.loads(run("ovnfs", "--json", "ls"))
    return [c for c in listing.get("checkouts") or [] if Path(c["source"]).resolve() == ROOT]


def size(path):
    target = ROOT / path
    if not target.exists():
        return "absent"
    kilobytes = int(subprocess.run(["du", "-sk", str(target)], check=True, text=True,
                                   capture_output=True).stdout.split()[0])
    return f"{kilobytes / 1024 / 1024:.1f} GB"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--allow", action="append", default=[], metavar="CHECKOUT",
                        help="an open checkout that builds nothing and may stay open")
    allowed = set(parser.parse_args().allow)

    mount = json.loads(run("ovnfs", "--json", "status"))["mount"]
    if ROOT.is_relative_to(Path(mount).resolve()) or not (ROOT / ".jj").is_dir():
        fail(f"run it in the default checkout, not {ROOT}")
    if jj("diff", "--name-only").strip() or jj("log", "--no-graph", "-r", "@", "-T", "description").strip():
        fail("the default checkout has changes or a described working-copy change; see `jj st`")
    open_now = checkouts()
    blocking = [c["name"] for c in open_now if c["name"] not in allowed and not frozen(c)]

    source_lock.unlock()
    try:
        update_source()
        if blocking:
            fail("moved the source to main but built nothing: checkouts that read the build "
                 "folders live are open: " + ", ".join(blocking) + " (run it again once they are "
                 "removed, or --allow one that builds nothing)")
        build([c["name"] for c in open_now])
    finally:
        source_lock.lock()


def update_source():
    """Moves the default checkout's empty working-copy change onto `main`. Safe under any open
    checkout: none reads the tracked files live (`--rev` ones read git, the others their capture),
    and new checkouts get main's checkout.py and .o2/ovnfs.yml, so they freeze the build folders.
    Without this a stale checkout.py there kept opening live checkouts that blocked every build
    (MS-152)."""
    jj("git", "fetch")
    if not jj("log", "--no-graph", "-r", "@- & ::main", "-T", "commit_id").strip():
        fail("the default checkout's working copy is not on a revision of main")
    if jj("log", "--no-graph", "-r", "@-", "-T", "commit_id") != jj("log", "--no-graph", "-r", "main", "-T", "commit_id"):
        jj("new", "main")


def build(open_before):
    revision = jj("log", "--no-graph", "-r", "main", "-T", 'commit_id.short() ++ " " ++ description.first_line()')
    print(f"Building main at {revision}", flush=True)

    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="mailstream-warm-dart-") as dart:
        steps = [
            ("turso.sh", ["sh", "scripts/turso.sh"], ROOT, {}),
            ("ffi.py", [sys.executable, "scripts/ffi.py"], ROOT, {"CONFIG": "release", "DART_OUT": dart}),
            ("engine tests", ["cargo", "test", "--no-run"], ROOT / "Engine", {}),
        ]
        for name, command, cwd, extra in steps:
            step = time.monotonic()
            subprocess.run(command, cwd=cwd, check=True, env={**ENV, **extra})
            print(f"{name}: {time.monotonic() - step:.0f} s", flush=True)
    print(f"Warm build done in {time.monotonic() - started:.0f} s; " +
          ", ".join(f"{path} {size(path)}" for path in OUTPUT))

    if changed := jj("diff", "--name-only").strip():
        fail("the build changed tracked files:\n" + changed)
    if opened := [c["name"] for c in checkouts() if c["name"] not in open_before]:
        print("warm-build: checkouts opened during the build and may hold half-built output: " +
              ", ".join(opened), file=sys.stderr)


try:
    main()
except subprocess.CalledProcessError as error:
    if error.stderr:
        sys.stderr.write(error.stderr)
    sys.exit(f"warm-build: {' '.join(map(str, error.cmd))} failed ({error.returncode})")
