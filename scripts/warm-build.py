#!/usr/bin/env python3
"""Refreshes the warm build of `main` in the default checkout (~/projects/mailstream), which every
new ovnfs checkout starts from as its own `target/` and `.turso/` (MS-151): the Turso library
(`scripts/turso.sh`) and the repository's one cargo workspace (MS-279; its members are the root
Cargo.toml's) into `target/`, in every configuration the scripts and agents build it. Cargo
unifies dependencies' features per build, over the packages it selects, so each configuration is
built the way its users run it: the release engine, command line and Apple Intelligence tagger
(`CONFIG=release scripts/ffi.py`); on the Mac every member's tests (`cargo test -p MEMBER
--no-run`, as `cargo test` in its folder) and GPUI in release (install.sh, bench.sh), release
with `shots` (shots.py) and debug (GPUI/run.sh; MS-273); on Linux the engine's tests and, in
`devenv shell` as install-linux.py and shots.py build it, GPUI in release and release with
`shots` (MS-294). Flutter is paused (MS-300): neither its Dart bindings nor their
generator (`FFI/bindgen`) are built. A crate added to the workspace gets its tests warmed with
no change here; a new build configuration (a feature, a profile) is a new step.
Crate-level `*/target` folders, from before the workspace, are deleted. On the Mac it first
installs main's compile budget (`scripts/build-budget.py install`, MS-260). It builds without the
toolchain variables scripts/repo.py drops on macOS (SDKROOT, CPATH, …): agents' shells don't set
them, and a warm build made with them rebuilt 15 crates in every checkout (MS-159).

It builds only when the warm build stopped serving new checkouts (MS-325): what it gives them is
the dependencies' artifacts (hundreds of crates, minutes cold). Cargo decides freshness by
modification time, and a checkout keeps the times of the files it didn't change, so a checkout
of a newer `main` reuses every artifact whose sources are unchanged and rebuilds only the repo's
crates that changed since the warm build, as it would have to for its own edits anyway. The
dependencies' artifacts change only with INPUTS below and the toolchain; their digest is kept in
STAMP after each build (failed steps included: the dependencies are built before a member fails),
and a run whose digest matches, within MAX_AGE of that build, only moves the source. MAX_AGE
bounds how stale the repo's own crates get (and anything INPUTS misses, such as the shell's
environment). --force builds anyway; so does a missing STAMP (a deleted `target/`).

Usage: python3 scripts/warm-build.py [--force] [--allow CHECKOUT ...]

Runs only in the default checkout, with no source changes there. It fetches `main` and moves the
default checkout's empty working-copy change onto it, always; then it builds only while every
open ovnfs checkout of it froze these folders when it opened (scripts/checkout.py; those keep the
build they opened with): any other (opened with `ovnfs new` by hand, `--rev` or before MS-152)
reads them live, so a build under it can hand it artifacts built from other source (MS-152), and
it exits 1 instead. --allow names such a checkout that builds nothing and may stay. Tracked files
end as `main` has them and nothing else in the tree changes. On the Mac it unlocks the default
checkout's source for its run and locks it again when it ends, failed or not
(scripts/source_lock.py, MS-156); the lock is macOS ACLs, so Linux's default checkout has none."""
import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.dont_write_bytecode = True
from repo import ROOT, build_env, capped, checkouts_of, is_default_checkout, jj  # noqa: E402
from checkout import frozen  # noqa: E402
import source_lock  # noqa: E402

OUTPUT = ["target", ".turso"]
STAMP = ROOT / "target" / ".warm-build.json"
MAX_AGE = 12 * 3600
# What the dependencies' artifacts are built from, as a jj fileset: versions and features
# (Cargo.lock, manifests), patched crates (the vendor folders), cargo's and the C toolchain's
# settings (.cargo, build scripts, devenv on Linux) and how this script builds (its steps, ffi.py's
# environment in repo.py, the Turso version in turso.sh).
INPUTS = ('Cargo.lock | glob:"**/Cargo.toml" | glob:"**/build.rs" | .cargo | Engine/vendor | '
          'GPUI/vendor | glob:"devenv.*" | glob:"rust-toolchain*" | scripts/warm-build.py | '
          'scripts/repo.py | scripts/ffi.py | scripts/turso.sh')


def fail(message):
    sys.exit(f"warm-build: {message}")


def size(path):
    target = ROOT / path
    if not target.exists():
        return "absent"
    kilobytes = int(subprocess.run(["du", "-sk", str(target)], check=True, text=True,
                                   capture_output=True).stdout.split()[0])
    return f"{kilobytes / 1024 / 1024:.1f} GB"


def inputs_digest():
    """Digest of INPUTS' files as the working copy has them, and of the toolchain."""
    digest = hashlib.sha256()
    for name in jj("file", "list", INPUTS).splitlines():
        digest.update(name.encode() + b"\0" + (ROOT / name).read_bytes() + b"\0")
    for tool in (["rustc", "-vV"], ["cargo", "-V"]):
        digest.update(subprocess.run(tool, cwd=ROOT, check=True, capture_output=True).stdout)
    return digest.hexdigest()


def still_warm(digest):
    """The last build's stamp when it serves `digest`, else None."""
    try:
        stamp = json.loads(STAMP.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    fresh = stamp.get("inputs") == digest and time.time() - stamp.get("built", 0) < MAX_AGE
    return stamp if fresh else None


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--allow", action="append", default=[], metavar="CHECKOUT",
                        help="an open checkout that builds nothing and may stay open")
    parser.add_argument("--force", action="store_true",
                        help="build even when no input changed since the last build")
    args = parser.parse_args()
    allowed = set(args.allow)

    if not is_default_checkout():
        fail(f"run it in the default checkout, not {ROOT}")
    if jj("diff", "--name-only").strip() or jj("log", "--no-graph", "-r", "@", "-T", "description").strip():
        fail("the default checkout has changes or a described working-copy change; see `jj st`")
    open_now = checkouts_of(ROOT)
    blocking = [c["name"] for c in open_now if c["name"] not in allowed and not frozen(c)]

    locked = sys.platform == "darwin"
    if locked:
        source_lock.unlock()
    try:
        # A build folder that isn't there yet would be created in the locked root, which
        # `linear-poll.py watch` may lock again during the run (MS-273); the folders themselves
        # are never locked.
        for path in OUTPUT:
            (ROOT / path).mkdir(exist_ok=True)
        update_source()
        digest = inputs_digest()
        if not args.force and (stamp := still_warm(digest)):
            hours = (time.time() - stamp["built"]) / 3600
            print(f"Moved the source to main; the warm build of {stamp['revision']} ({hours:.1f} h "
                  "ago) still serves it: no dependency input changed, nothing built", flush=True)
            return
        if blocking:
            fail("moved the source to main but built nothing: checkouts that read the build "
                 "folders live are open: " + ", ".join(blocking) + " (run it again once they are "
                 "removed, or --allow one that builds nothing)")
        remove_stale()
        failed = build([c["name"] for c in open_now])
        revision = jj("log", "--no-graph", "-r", "@-", "-T", "commit_id.short()")
        STAMP.write_text(json.dumps({"revision": revision, "inputs": digest, "built": time.time()}))
        if failed:
            fail("these steps failed (the others are warm): " + ", ".join(failed))
    finally:
        if locked:
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


def remove_stale():
    """Deletes crate-level target folders (`Engine/target`, `FFI/target`, `GPUI/target`), which
    nothing builds into since the workspace (MS-279); open checkouts froze their own copies. Each
    is first moved into `target/` (one rename in its locked parent, while that is unlocked), then
    deleted there."""
    for stale in sorted(ROOT.glob("*/target")):
        if stale.is_symlink() or not stale.is_dir():
            continue
        trash = ROOT / "target" / f".stale-{stale.parent.name}"
        shutil.rmtree(trash, ignore_errors=True)
        stale.rename(trash)
        shutil.rmtree(trash)
        print(f"Deleted {stale.relative_to(ROOT)}", flush=True)


# Flutter's Dart binding generator: Flutter is paused (MS-300), so nothing builds it.
PAUSED = {"mailstream-bindgen"}


def members():
    """The workspace's member packages, by name, without PAUSED."""
    metadata = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                                         cwd=ROOT, check=True, text=True, capture_output=True).stdout)
    return sorted(package["name"] for package in metadata["packages"]
                  if package["id"] in metadata["workspace_members"] and package["name"] not in PAUSED)


def build(open_before):
    revision = jj("log", "--no-graph", "-r", "main", "-T", 'commit_id.short() ++ " " ++ description.first_line()')
    print(f"Building main at {revision}", flush=True)

    started = time.monotonic()
    # The Mac's compile budget (MS-260) as main has it: its rustc wrapper in ~/.cargo/config.toml.
    subprocess.run([sys.executable, "scripts/build-budget.py", "install"], cwd=ROOT, check=True)
    steps = [
        # ffi.py caps its own cargo.
        ("turso.sh", capped(["sh", "scripts/turso.sh"]), {}),
        ("ffi.py", [sys.executable, "scripts/ffi.py"], {"CONFIG": "release"}),
    ]
    gpui = ["cargo", "build", "-p", "mailstream-gpui"]
    if sys.platform == "darwin":
        steps += [(f"{member} tests", capped(["cargo", "test", "-p", member, "--no-run"]), {})
                  for member in members()]
        # GPUI as its scripts build it (MS-273): release for install.sh, bench.sh and
        # gpui-bundle.sh, release with `shots` for scripts/shots.py, debug for GPUI/run.sh.
        steps += [
            ("GPUI release", capped([*gpui, "--release"]), {}),
            ("GPUI shots", capped([*gpui, "--release", "--features", "shots"]), {}),
            ("GPUI debug", capped(gpui), {}),
        ]
    else:
        steps.append(("engine tests", capped(["cargo", "test", "-p", "mailstream-engine", "--no-run"]), {}))
        # GPUI in `devenv shell` (the libraries it links, devenv.nix), release for install-linux.py
        # and with `shots` for scripts/shots.py (MS-294).
        gpui = ["devenv", "shell", "--", *gpui, "--release"]
        steps += [
            ("GPUI release", capped(gpui), {}),
            ("GPUI shots", capped([*gpui, "--features", "shots"]), {}),
        ]
    failed = []
    for name, command, extra in steps:
        step = time.monotonic()
        # A step that fails (a member's tests not compiling on main) leaves the others warm:
        # every step still runs, and the run fails at the end naming it.
        code = subprocess.run(command, cwd=ROOT, env=build_env(**extra)).returncode
        print(f"{name}: {time.monotonic() - step:.0f} s" + (f", FAILED ({code})" if code else ""), flush=True)
        if code:
            failed.append(name)
    print(f"Warm build done in {time.monotonic() - started:.0f} s; " +
          ", ".join(f"{path} {size(path)}" for path in OUTPUT))

    if changed := jj("diff", "--name-only").strip():
        fail("the build changed tracked files:\n" + changed)
    if opened := [c["name"] for c in checkouts_of(ROOT) if c["name"] not in open_before]:
        print("warm-build: checkouts opened during the build and may hold half-built output: " +
              ", ".join(opened), file=sys.stderr)
    return failed


try:
    main()
except subprocess.CalledProcessError as error:
    if error.stderr:
        sys.stderr.write(error.stderr)
    sys.exit(f"warm-build: {' '.join(map(str, error.cmd))} failed ({error.returncode})")
