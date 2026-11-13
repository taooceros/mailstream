"""Helpers the repo's Python scripts share (MS-239): running commands, jj and ovnfs in the
repository, telling the default checkout (~/projects/mailstream) from an ovnfs checkout of it,
and the environment and limit cargo builds run in. Standard library only, like every script here."""
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# C build scripts (ring, simsimd) rebuild, and everything above them with them, whenever one of
# these differs from the build being reused. Shells pick them up unevenly (the warm build of
# 11-09 had SDKROOT and CPATH, agents' shells don't: every checkout rebuilt 15 crates twice,
# MS-159); on macOS the C compiler finds the SDK through xcrun without them. Elsewhere (nix on
# Linux) the toolchain needs them, so they stay.
DRIFTING = ["SDKROOT", "CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH"]


def build_env(**extra):
    """os.environ for a cargo build, without DRIFTING on macOS, plus `extra`."""
    drop = DRIFTING if sys.platform == "darwin" else []
    return {**{k: v for k, v in os.environ.items() if k not in drop}, **extra}


def capped(argv):
    """argv run under the machine's compile limit (`scripts/capped`, MS-268); unchanged on Windows,
    which has no bash to run it."""
    return argv if sys.platform == "win32" else [str(ROOT / "scripts/capped"), *argv]


def run(*args, cwd=ROOT):
    """Runs a command, raising CalledProcessError (stderr captured) on failure; returns stdout."""
    return subprocess.run(args, cwd=cwd, check=True, text=True, capture_output=True).stdout


def jj(*args, cwd=ROOT):
    return run("jj", "--no-pager", *args, cwd=cwd)


def clone_tree(source, destination):
    """Copies folder `source` to `destination` (which must not exist) as a copy-on-write clone,
    modification times kept for cargo: APFS clones on macOS, reflinks (btrfs) on Linux."""
    flags = ["-c", "-R", "-p"] if sys.platform == "darwin" else ["-a", "--reflink=auto"]
    run("cp", *flags, str(source), str(destination))


def ovnfs(*args):
    return json.loads(run("ovnfs", "--json", *args))


def is_default_checkout():
    """Whether ROOT is the default checkout rather than an ovnfs checkout of it."""
    mount = Path(ovnfs("status")["mount"]).resolve()
    return (ROOT / ".jj").is_dir() and not ROOT.is_relative_to(mount)


def default_checkout():
    """The default checkout, also when the script runs from one of its checkouts."""
    for checkout in ovnfs("ls").get("checkouts") or []:
        if Path(checkout["path"]).resolve() == ROOT:
            return Path(checkout["source"])
    return ROOT


def checkouts_of(source):
    """The open ovnfs checkouts of `source` (`ovnfs --json ls` entries)."""
    return [c for c in ovnfs("ls").get("checkouts") or [] if Path(c["source"]).resolve() == source]
