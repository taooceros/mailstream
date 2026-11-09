#!/usr/bin/env python3
"""Refreshes the warm build of `main` in the default checkout (~/projects/mailstream), which every
new ovnfs checkout reads as its own `target/` and `.turso/` (MS-151): the release engine, command
line, Apple Intelligence tagger and Dart binding generator (`CONFIG=release scripts/ffi.py`, into
FFI/target), the engine's tests (`cargo test --no-run` in Engine/, into Engine/target/debug) and
the Turso library (`scripts/turso.sh`). It builds without the toolchain variables ffi.py drops
(SDKROOT, CPATH, …): agents' shells don't set them, and a warm build made with them rebuilt 15
crates in every checkout (MS-159).

Usage: python3 scripts/warm-build.py [--allow CHECKOUT ...]

Runs only in the default checkout, with no source changes there, and only while no ovnfs checkout
of it is open: a checkout reads these folders live, so a build under an open one can hand it
artifacts built from other source (MS-152). --allow names an open checkout that builds nothing
and may stay. It fetches `main` and moves the default checkout's empty working-copy change onto
it, then builds; tracked files end as `main` has them and nothing else in the tree changes (the
Dart bindings go to a temporary folder)."""
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
ENV = {k: v for k, v in os.environ.items() if k not in DRIFTING}


def run(*args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, text=True, capture_output=True, **kwargs).stdout


def jj(*args):
    return run("jj", "--no-pager", *args)


def fail(message):
    sys.exit(f"warm-build: {message}")


def open_checkouts(allowed):
    listing = json.loads(run("ovnfs", "--json", "ls"))
    return [c["name"] for c in listing.get("checkouts") or []
            if Path(c["source"]).resolve() == ROOT and c["name"] not in allowed]


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
    if blocking := open_checkouts(allowed):
        fail("checkouts of this folder are open: " + ", ".join(blocking) +
             " (run it once they are removed, or --allow one that builds nothing)")
    if jj("diff", "--name-only").strip() or jj("log", "--no-graph", "-r", "@", "-T", "description").strip():
        fail("the default checkout has changes or a described working-copy change; see `jj st`")

    jj("git", "fetch")
    if not jj("log", "--no-graph", "-r", "@- & ::main", "-T", "commit_id").strip():
        fail("the default checkout's working copy is not on a revision of main")
    if jj("log", "--no-graph", "-r", "@-", "-T", "commit_id") != jj("log", "--no-graph", "-r", "main", "-T", "commit_id"):
        jj("new", "main")
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
    if opened := open_checkouts(allowed):
        print("warm-build: checkouts opened during the build and may hold half-built output: " +
              ", ".join(opened), file=sys.stderr)


try:
    main()
except subprocess.CalledProcessError as error:
    if error.stderr:
        sys.stderr.write(error.stderr)
    sys.exit(f"warm-build: {' '.join(map(str, error.cmd))} failed ({error.returncode})")
