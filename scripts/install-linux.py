#!/usr/bin/env python3
"""Linux (NixOS): installs affected, already-installed front ends from a clean private checkout at
`main` (MS-224), as scripts/install.sh does on the Mac; install.sh hands Linux runs to this script.

Usage: scripts/install.sh gpui   (or python3 scripts/install-linux.py gpui)
flutter is refused: the Flutter front end is paused (MS-300); ~/.local/opt/mailstream stays as last
installed.

Installed front end, a folder under ~/.local/opt:
- gpui: ~/.local/opt/mailstream-gpui/ holds the release binary `mailstream-gpui` and the launcher
  `mailstream-gpui.sh`, which sets LD_LIBRARY_PATH to the devenv shell's (GPUI dlopens Vulkan,
  Wayland and fontconfig; NixOS has no global library path) and starts it. The desktop entry
  ~/.local/share/applications/dev.mailstream.MailstreamGPUI.desktop starts the launcher.
The folder keeps its config.json (OAuth clients; the engine reads it beside the executable) and
AppIcon.png, gets the built commit in `revision`, and a `nix-env` profile holding the devenv shell
the build used, so a nix garbage collection keeps the libraries the binaries load.

Builds run in `devenv shell`. Cargo can't build inside the ovnfs mount (MS-201), so the cargo
workspace's `target` (MS-279) becomes a symlink to ~/.cache/mailstream-checkouts/<checkout>/target
(where scripts/checkout.py puts it; `checkout.py rm` deletes it) unless it is a symlink already.
The running installed copy is stopped (SIGTERM, never forced) and the folder swapped; the previous
one stays in ~/.cache/mailstream-install/previous-<folder> until the next install. Then check each
install: `revision` equals main, and the binary stays up 30 s in a scripts/vhypr.py desktop
(AGENTS.md, Release delivery)."""
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HOME = Path.home()
OPT = HOME / ".local/opt"
PREVIOUS = HOME / ".cache/mailstream-install"
OUTSIDE = HOME / ".cache/mailstream-checkouts" / ROOT.name
LOCK = Path("/tmp/mailstream-release-install.lock")
FRONTENDS = {
    "gpui": {"folder": OPT / "mailstream-gpui", "executable": "mailstream-gpui",
             "outside": ["target"], "icon": "GPUI/assets/AppIcon.png"},
}
DESKTOP = HOME / ".local/share/applications/dev.mailstream.MailstreamGPUI.desktop"


def fail(message):
    sys.exit(f"install: {message}")


def run(*args, capture=False):
    result = subprocess.run(args, cwd=ROOT, text=True, stdout=subprocess.PIPE if capture else None)
    if result.returncode:
        fail(f"{' '.join(args)} exited {result.returncode}")
    return result.stdout if capture else None


def devenv(script, capture=False):
    # Under the machine's compile limit (scripts/capped, MS-268).
    return run(str(ROOT / "scripts/capped"), "devenv", "shell", "--", "sh", "-euc", script, capture=capture)


def main_revision():
    return run("jj", "--no-pager", "log", "--no-graph", "-r", "main", "-T", "commit_id", capture=True).strip()


def move_outside(relative):
    """MS-201: a cargo target in the FUSE mount fails to build; keep it outside."""
    inside = ROOT / relative
    if inside.is_symlink():
        return
    target = OUTSIDE / relative
    target.mkdir(parents=True, exist_ok=True)
    if inside.exists():
        shutil.rmtree(inside)
    inside.symlink_to(target, target_is_directory=True)


def build(frontend):
    """Builds `frontend` (gpui) in devenv shell; returns the folder to install."""
    devenv("cd GPUI && cargo build --release")
    staged = Path(os.path.realpath(ROOT / "target")) / "release/mailstream-gpui-install"
    shutil.rmtree(staged, ignore_errors=True)
    staged.mkdir()
    shutil.copy2(ROOT / "target/release/mailstream-gpui", staged / "mailstream-gpui")
    return staged


def stop(executable):
    """SIGTERM the installed copy (only that path) and wait up to 15 s; never force it."""
    pattern = f"^{executable}( |$)"
    pids = subprocess.run(["pgrep", "-f", pattern], text=True, capture_output=True).stdout.split()
    for pid in pids:
        os.kill(int(pid), signal.SIGTERM)
    for pid in pids:
        for _ in range(15):
            try:
                os.kill(int(pid), 0)
            except ProcessLookupError:
                break
            time.sleep(1)
        else:
            fail(f"installed process {pid} has not exited; not forcing termination")


def install(frontend, built, revision, ld_library_path, shell):
    spec = FRONTENDS[frontend]
    folder = spec["folder"]
    staging = folder.with_name(f".{folder.name}-install-{os.getpid()}")
    if staging.exists():
        fail(f"{staging} already exists")
    shutil.copytree(built, staging, symlinks=True)
    for keep in ["config.json"]:
        if (folder / keep).exists():
            shutil.copy2(folder / keep, staging / keep)
    shutil.copy2(ROOT / spec["icon"], staging / "AppIcon.png")
    (staging / "revision").write_text(revision + "\n")
    executable = folder / spec["executable"]
    if frontend == "gpui":
        launcher = staging / "mailstream-gpui.sh"
        launcher.write_text(
            "#!/bin/sh\n"
            f"# Mailstream GPUI, built from main {revision} (scripts/install-linux.py).\n"
            "# NixOS has no global library path: GPUI dlopens Vulkan, Wayland and fontconfig from the\n"
            "# devenv shell, kept from garbage collection by the nix-env profile beside this script.\n"
            "# --help/--version only print what is installed: probing the launcher must never open the\n"
            "# app on the person's screen (two install agents did on 11-12).\n"
            'case "${1:-}" in -h|--help|--version|-V)\n'
            f'  echo "Mailstream GPUI, built from main {revision}; run without arguments to start it"\n'
            "  exit 0;;\n"
            "esac\n"
            f'export LD_LIBRARY_PATH="{ld_library_path}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}"\n'
            f'exec "{executable}" "$@"\n')
        launcher.chmod(0o755)
    stop(executable)
    PREVIOUS.mkdir(parents=True, exist_ok=True)
    previous = PREVIOUS / f"previous-{folder.name}"
    shutil.rmtree(previous, ignore_errors=True)
    folder.rename(previous)
    try:
        staging.rename(folder)
    except OSError as error:
        previous.rename(folder)
        fail(f"installation failed ({error}); previous folder restored")
    # A GC root for the devenv shell's closure, which holds every library LD_LIBRARY_PATH and the
    # binaries' RPATHs name; nix-env registers a profile by its absolute path, so set it here.
    for link in folder.glob("nix-env*"):
        link.unlink()
    run("nix-env", "--profile", str(folder / "nix-env"), "--set", shell)

    if frontend == "gpui":
        DESKTOP.parent.mkdir(parents=True, exist_ok=True)
        DESKTOP.write_text(
            "[Desktop Entry]\nType=Application\nName=Mailstream\n"
            "Comment=Mail, chat and calendar for Gmail and Outlook (GPUI)\n"
            f"Exec={folder / 'mailstream-gpui.sh'}\nIcon={folder / 'AppIcon.png'}\n"
            "Terminal=false\nCategories=Network;Email;\nStartupWMClass=dev.mailstream.MailstreamGPUI\n")
    print(f"Installed {folder} at {revision}; previous folder kept at {previous}.", flush=True)


def main():
    targets = sys.argv[1:]
    if not targets:
        fail("choose affected installed front ends: gpui (see AGENTS.md)")
    for frontend in targets:
        if frontend == "flutter":
            fail("Flutter is paused (MS-300): no Flutter installs; ~/.local/opt/mailstream stays as it is")
        if frontend not in FRONTENDS:
            fail(f"unknown front end on Linux: {frontend}")
        spec = FRONTENDS[frontend]
        if not (spec["folder"] / spec["executable"]).is_file():
            fail(f"{spec['folder']} is not installed")
    revision = main_revision()
    if not run("jj", "--no-pager", "log", "--no-graph", "-r", "main & (@ | @-)", "-T", "commit_id", capture=True):
        fail("create a clean private checkout at current main (scripts/checkout.py NAME); do not deploy an area workspace")
    if run("jj", "--no-pager", "diff", "--from", "main", "--to", "@", "--name-only", capture=True).strip():
        fail("checkout differs from main")
    if ROOT == (HOME / "projects/mailstream").resolve():
        fail("never install from the default checkout")

    try:
        LOCK.mkdir()
    except FileExistsError:
        fail(f"another release deployment holds {LOCK}; coordinate before retrying")
    try:
        for frontend in dict.fromkeys(targets):
            for relative in FRONTENDS[frontend]["outside"]:
                move_outside(relative)
        ld_library_path = devenv('printf %s "$LD_LIBRARY_PATH"', capture=True).strip()
        shell = os.path.realpath(ROOT / ".devenv/gc/shell")
        if not ld_library_path or not shell.startswith("/nix/store/"):
            fail("devenv shell gave no LD_LIBRARY_PATH or .devenv/gc/shell")
        closure = run("nix-store", "--query", "--requisites", shell, capture=True).split()
        missing = [p for p in ld_library_path.split(":") if p.rsplit("/lib", 1)[0] not in closure]
        if missing:
            fail(f"devenv shell's closure misses {missing}")
        for frontend in dict.fromkeys(targets):
            built = build(frontend)
            if main_revision() != revision:
                fail("main advanced during build; rebuild at current main before installing")
            install(frontend, built, revision, ld_library_path, shell)
    finally:
        LOCK.rmdir()
    print("Check each install: revision equals main, launch in a vhypr desktop (AGENTS.md).")


if __name__ == "__main__":
    main()
