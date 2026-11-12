#!/usr/bin/env python3
"""Lands an issue's bookmark on `main` and pushes it, one lander at a time (MS-179).

Usage: python3 scripts/land.py MS-12      (in your checkout; lands bookmark ms-12)

In the jj workspace it runs in, holding the lock `.jj/land.lock` of the default checkout
(~/projects/mailstream) so that two landers never move `main` at once: `jj git fetch`, `jj rebase
-b ms-12 -d main@origin`, and a stop if that left a conflict (resolve it in ms-12's commits, rerun
the tests it touches, look again in the real app when it touches UI, then run land.py again).
Then `jj bookmark set main -r ms-12` and `jj git push -b main`; when someone outside the lock (the
Windows machine, the user) pushed `main` meanwhile, it puts `main` back and tries again from the
fetch, up to three times. Last it deletes ms-12 (on origin too, if it was pushed) and prints the
landed commits for the hand-over comment.

ms-12 must point at described commits on top of `main`, not at the working copy: `jj commit -m
"…"`, then `jj bookmark set ms-12 -r @-`. A checkout opened before MS-179, with a jj repository
of its own, lands the same way; the lock is shared."""
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.dont_write_bytecode = True
from repo import default_checkout  # noqa: E402

TRIES = 3


def jj(*args, check=True):
    done = subprocess.run(["jj", "--no-pager", *args], text=True, capture_output=True)
    if check and done.returncode:
        fail(f"jj {' '.join(args)} failed ({done.returncode}):\n{done.stderr.strip()}")
    return done


def ids(revset, template='commit_id ++ "\\n"'):
    return jj("log", "--no-graph", "-r", revset, "-T", template).stdout.splitlines()


def fail(message):
    sys.exit(f"land: {message}")


def lock(path):
    """Holds an exclusive lock on `path` until the process exits; says who holds it while waiting."""
    handle = open(path, "a+")
    if os.name == "nt":
        import msvcrt
        import time
        while True:
            try:
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
                break
            except OSError:
                time.sleep(1)
    else:
        import fcntl
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            handle.seek(0)
            print(f"land: waiting for {handle.read().strip() or 'another land.py'}", flush=True)
            fcntl.flock(handle, fcntl.LOCK_EX)
        handle.seek(0)
        handle.truncate()
        handle.write(f"{Path.cwd()} landing {sys.argv[1]} (pid {os.getpid()})\n")
        handle.flush()
    return handle


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"(?i)ms-\d+", sys.argv[1]):
        sys.exit(__doc__.split("\n\n")[1])
    bookmark = sys.argv[1].lower()
    if not ids(f"present({bookmark})"):
        fail(f"no bookmark {bookmark} (jj bookmark create {bookmark} -r @-)")
    if ids(bookmark) == ids("@"):
        fail(f"{bookmark} is the working copy: `jj commit -m …`, then `jj bookmark set {bookmark} -r @-`")
    held = lock(default_checkout() / ".jj" / "land.lock")

    for _ in range(TRIES):
        jj("git", "fetch")
        if ids("main") != ids("main@origin"):
            fail("main differs from main@origin outside land.py; see `jj bookmark list main "
                 "--all-remotes` (`jj bookmark set main --allow-backwards -r main@origin` drops "
                 "the local move)")
        jj("rebase", "-b", bookmark, "-d", "main@origin")
        landing = f"main@origin..{bookmark}"
        if conflicted := ids(f"{landing} & conflicts()", 'commit_id.short() ++ "\\n"'):
            fail(f"rebased {bookmark} onto main@origin with conflicts in {', '.join(conflicted)}: "
                 "resolve them, rerun the tests they touch, then run land.py again")
        if undescribed := ids(f"{landing} & description(exact:'')", 'commit_id.short() ++ "\\n"'):
            fail(f"commits without a description in {bookmark}: {', '.join(undescribed)}")
        landed = ids(landing, 'commit_id.short(12) ++ " " ++ description.first_line() ++ "\\n"')
        if not landed:
            fail(f"{bookmark} has nothing that main lacks")
        jj("bookmark", "set", "main", "-r", bookmark)
        pushed = jj("git", "push", "-b", "main", check=False)
        if not pushed.returncode:
            break
        print(f"land: push failed, trying again:\n{pushed.stderr.strip()}", file=sys.stderr)
        jj("bookmark", "set", "main", "--allow-backwards", "-r", "main@origin")
    else:
        fail(f"could not push main in {TRIES} tries; {bookmark} is unchanged on main@origin")

    on_origin = ids(f"present({bookmark}@origin)")
    jj("bookmark", "delete", bookmark)
    if on_origin:
        jj("git", "push", "-b", bookmark)
    held.close()
    print(f"landed {bookmark} on main {ids('main', 'commit_id.short(12)')[0]}:")
    print("\n".join(reversed(landed)))


if __name__ == "__main__":
    main()
