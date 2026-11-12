#!/usr/bin/env python3
"""Opens an agent's ovnfs checkout of the default checkout (~/projects/mailstream), with its build
folders frozen as they are now, as a jj workspace of the default checkout's repository at `main`
(.omp/skills/ovnfs/SKILL.md); `rm` removes one.

Usage: python3 ~/projects/mailstream/scripts/checkout.py NAME
       python3 ~/projects/mailstream/scripts/checkout.py rm NAME

It runs `ovnfs new ~/projects/mailstream --name NAME`, checks that the repo's `.o2/ovnfs.yml` took
effect (frozen() below), makes the checkout jj workspace NAME of the default checkout's repository
(graft() below), then runs `jj git fetch` and `jj new main` in it and prints its path.

`.o2/ovnfs.yml` makes the warm build's folders (every `target/`, `.turso/`) `source`, so ovnfs
captures them when the checkout opens (APFS clones) instead of serving the default checkout's
live: a later build or delete there no longer reaches the checkout, and cargo can't keep an
artifact built from other source (MS-152). It hides the default checkout's `.jj`. No `--rev`:
ovnfs v0.3.0 serves ignored files of a `--rev` checkout live whatever the rules say; the default
checkout is clean on `main` anyway (it is locked, and only warm-build.py moves it), and `jj new
main` brings the checkout to `main`'s tip.

On Linux the cargo target folders live outside the checkout (MS-201): ovnfs's FUSE mount answers a
directory listing with the sizes files had when the directory was opened, and the kernel takes them
over newer writes, so a file just written reads as empty or cut short for a second
(https://github.com/stencil-hq/ovnfs/issues/48). Every rustc lists `target/*/deps` as it starts
while others write objects there, and cargo builds in the mount fail ("failed to map object file:
memory map must have a non-zero length"). unmount_target() below gives the cargo workspace (MS-279)
its target folder in ~/.cache/mailstream-checkouts/NAME/target, starting as a reflink copy of the
default checkout's warm build (btrfs: no copying, no extra disk), and makes the checkout's `target`
a symlink to it; `rm` deletes it.

Every checkout shares the default checkout's jj repository (MS-179): commits, bookmarks, the
operation log and each checkout's working copy (`NAME@`) show in every checkout at once, without
a fetch or push. ovnfs knows only git (https://github.com/stencil-hq/ovnfs/issues/37) and jj adds
a workspace only in an empty folder, so graft() adds it in an empty temporary folder and moves its
`.jj` into the checkout, pointing `.jj/repo` at the default checkout's repository. jj doesn't
document this: after upgrading jj, test it again (two fresh checkouts: `jj st` clean in each, a
commit made in one shows in the other's `jj log`). `jj workspace list` shows the temporary folder
as the workspace's path. The checkout's `.git` (ovnfs's private git directory) stays as captured
and goes stale: use jj there, not git.

`rm NAME` forgets workspace NAME, runs `ovnfs rm NAME` and deletes its target folders outside the
mount; any of them may be gone already."""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
from repo import ROOT, default_checkout, jj, ovnfs, run  # noqa: E402

BUILD = ["target", ".turso"]
TARGETS = Path.home() / ".cache/mailstream-checkouts"


def frozen(checkout):
    """Whether a checkout (an `ovnfs --json ls` entry) keeps the build folders it opened with:
    opened without `--rev` (ovnfs keeps a `revision-index` in the base of a `--rev` one) and with
    the build folders `source` and `.jj` hidden."""
    if (Path(ovnfs("status")["state"]) / "bases" / checkout["base"] / "revision-index").exists():
        return False
    def kind(path):
        return ovnfs("explain", checkout["name"], path)["class"]
    return kind(".jj") == "hidden" and all(kind(path) == "source" for path in BUILD)


def workspaces(source):
    listing = jj("--ignore-working-copy", "workspace", "list", "-T", 'name ++ "\\n"',
                      cwd=source)
    return listing.split()


def forget(source, name):
    """Forgets workspace `name` in the default checkout's repository, if it has one. jj abandons
    its working-copy change when that is empty; `--ignore-working-copy` leaves the default
    checkout's own working copy alone."""
    if name in workspaces(source):
        jj("--ignore-working-copy", "workspace", "forget", name, cwd=source)


def graft(source, path, name):
    """Makes `path` (a fresh checkout of `source`) jj workspace `name` of `source`'s repository, its
    working copy on the revision ovnfs captured: the parent of the default checkout's working
    copy, which `jj workspace add` takes when given no revision. A workspace of the same name left
    by a checkout removed without `rm` is forgotten first: the ovnfs name was free, so no checkout
    uses it."""
    forget(source, name)
    with tempfile.TemporaryDirectory(prefix=f"checkout-{name}-") as temporary:
        workspace = Path(temporary) / name
        jj("workspace", "add", "--name", name, str(workspace), cwd=source)
        shutil.move(str(workspace / ".jj"), str(path / ".jj"))
    (path / ".jj" / "repo").write_text(str(source / ".jj" / "repo"))


def remove(name):
    source = default_checkout()
    forget(source, name)
    removed = subprocess.run(["ovnfs", "rm", name], text=True, capture_output=True)
    if removed.returncode and "not_found" not in removed.stderr:
        sys.exit(f"checkout: ovnfs rm {name} failed: {removed.stderr.strip()}")
    shutil.rmtree(TARGETS / name, ignore_errors=True)
    print(f"removed {name}")


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "rm":
        return remove(sys.argv[2])
    if len(sys.argv) != 2 or sys.argv[1].startswith("-"):
        sys.exit(__doc__.split("\n\n")[1])
    source = default_checkout()
    opened = ovnfs("new", str(source), "--name", sys.argv[1])
    checkout = opened["checkout"]
    path = Path(checkout["path"])
    if not frozen(checkout):
        run("ovnfs", "rm", checkout["name"])
        sys.exit("checkout: the default checkout has no .o2/ovnfs.yml freezing the build folders "
                 "yet (the next scripts/warm-build.py there brings it); removed " + checkout["name"])
    capture = opened["capture"]
    print(f"Opened {path}: {capture['files']} files ({capture['bytes'] / 1e9:.1f} GB, build "
          f"folders included) in {capture['seconds']:.1f} s", flush=True)
    try:
        set_up(source, path, checkout["name"])
    except BaseException:
        remove(checkout["name"])
        raise


def unmount_target(source, path, name):
    """Linux: replaces the workspace's target folder in the checkout with a symlink to one outside
    the FUSE mount, started from the default checkout's warm build (header). Folders a checkout of
    the same name left behind without `rm` are deleted first: the ovnfs name was free."""
    outside = TARGETS / name
    shutil.rmtree(outside, ignore_errors=True)
    outside.mkdir(parents=True)
    target = outside / "target"
    warm = source / "target"
    if warm.is_dir():
        run("cp", "-a", "--reflink=auto", str(warm), str(target))
    else:
        target.mkdir()
    inside = path / "target"
    shutil.rmtree(inside, ignore_errors=True)
    inside.symlink_to(target, target_is_directory=True)


def set_up(source, path, name):
    graft(source, path, name)

    if jj("diff", "--name-only", cwd=path).strip():
        # The capture and the default checkout's working copy disagree (warm-build.py moved it
        # meanwhile): the fresh checkout holds no work, so take the revision's files.
        jj("restore", cwd=path)
    jj("git", "fetch", cwd=path)
    jj("new", "main", cwd=path)
    if sys.platform == "linux":
        # After `jj new main`, whose .gitignore also ignores the symlinks.
        unmount_target(source, path, name)
    print(path, "at main", jj("log", "--no-graph", "-r", "main",
                              "-T", 'commit_id.short() ++ " " ++ description.first_line()', cwd=path))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        if error.stderr:
            sys.stderr.write(error.stderr)
        sys.exit(f"checkout: {' '.join(map(str, error.cmd))} failed ({error.returncode})")
