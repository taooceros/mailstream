#!/usr/bin/env python3
"""Locks the default checkout (~/projects/mailstream) against writes (MS-156): agents' tools start
in that folder, and a relative path or a missing `cwd` put their edits and deletes there instead
of in their ovnfs checkout, where every new checkout picked them up.

Every tracked file gets the macOS ACL entry `everyone deny write,append,delete` and every folder
holding one (the root included) `everyone deny add_file,add_subdirectory,delete_child,delete`, so
editing, creating, deleting or renaming anything there fails with "Permission denied", whatever
tool does it. Reads, `jj st`/`jj log`/`jj git fetch` and `ovnfs new` still work, and the ignored
build folders (`FFI/target`, `Engine/target`, `.turso/`) stay writable inside, though they can't
be deleted or renamed as a whole. An ACL, unlike `chmod a-w`, doesn't reach ovnfs checkouts: a
checkout copies the source's file modes, so read-only files there would drop writes.

Usage: python3 scripts/source_lock.py lock|unlock|status

Runs only in the default checkout, on macOS. warm-build.py unlocks it for its run (moving the
working copy onto `main` writes files) and locks it again after; `linear-poll.py watch` locks it
again before each pass when anything there is writable. status exits 1 when anything is.

A jj command that rewrites the working copy there (`jj new`, `jj edit`, `jj restore`) fails while
it is locked and leaves the working copy stale; `unlock`, which doesn't need jj, then `jj
workspace update-stale` and `lock` recover it."""
import os
import subprocess
import sys
from pathlib import Path

sys.dont_write_bytecode = True
from repo import ROOT, is_default_checkout, jj  # noqa: E402

FILE_ACE = "everyone deny write,append,delete"
DIR_ACE = "everyone deny add_file,add_subdirectory,delete_child,delete"
BATCH = 500


def paths():
    """The tracked files (symlinks left out: chmod would follow them) and the folders holding them."""
    files = [ROOT / name for name in jj("file", "list").splitlines()]
    files = [path for path in files if not path.is_symlink() and path.exists()]
    folders = {ROOT / parent for path in files for parent in path.relative_to(ROOT).parents}
    return files, sorted(folders)


def chmod(*args, targets):
    for start in range(0, len(targets), BATCH):
        subprocess.run(["chmod", *args, *map(str, targets[start:start + BATCH])], check=True)


def lock():
    unlock()
    files, folders = paths()
    chmod("+a", FILE_ACE, targets=files)
    chmod("+a", DIR_ACE, targets=folders)


def unlock():
    """Clears the ACLs of every locked folder and its entries, walking down from ROOT through the
    locked folders only (the build folders' contents are never locked), without asking jj."""
    folders = [ROOT]
    while folders:
        folder = folders.pop()
        entries = [Path(entry.path) for entry in os.scandir(folder) if not entry.is_symlink()]
        folders += [path for path in entries if path.is_dir() and not os.access(path, os.W_OK)]
        chmod("-N", targets=[folder, *entries])


def writable():
    """The tracked files and folders that accept writes."""
    files, folders = paths()
    return [path for path in files + folders if os.access(path, os.W_OK)]


def main():
    command = sys.argv[1:]
    if command not in (["lock"], ["unlock"], ["status"]):
        sys.exit(__doc__.split("\n\n")[2])
    if sys.platform != "darwin":
        sys.exit("source_lock: macOS only")
    if not is_default_checkout():
        sys.exit(f"source_lock: {ROOT} is not the default checkout")
    if command == ["lock"]:
        lock()
        print(f"locked {ROOT}")
    elif command == ["unlock"]:
        unlock()
        print(f"unlocked {ROOT}; lock it again with `python3 scripts/source_lock.py lock`")
    elif open_paths := writable():
        print(f"unlocked: {len(open_paths)} tracked files or folders are writable, e.g. "
              + ", ".join(str(path.relative_to(ROOT)) for path in open_paths[:5]))
        return 1
    else:
        print("locked")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.exit(f"source_lock: {' '.join(map(str, error.cmd))} failed ({error.returncode})\n{error.stderr or ''}")
