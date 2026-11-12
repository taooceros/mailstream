#!/usr/bin/env python3
"""Screenshots of the GPUI app for a hand-over, light and dark, with no display, compositor or
computer use (MS-243): works with the screen locked, over SSH and beside the person's work, on the
Mac and on Linux. Run it in your own checkout.

Usage: python3 scripts/shots.py SCRIPT [--out DIR] [--size WxH] [--only light|dark]
                                [--upload] [--no-build] [--keep]

It builds `GPUI/` in release with the `shots` feature (Linux: in `devenv shell`, the cargo target
outside the ovnfs mount as scripts/checkout.py and install-linux.py keep it, MS-201; a first build
starts from a reflink copy of the newest other checkout's), then runs `mailstream-gpui --shots`
(`GPUI/src/shots.rs`) once light and once dark. Each run is the app's real window, views and
engine, drawn headless by GPUI's offscreen renderer over the synthetic mail of
`GPUI/src/shots/seed.rs` in a fresh data folder: a Gmail and an Outlook account, threads, a
meeting invitation, an HTML report, Chinese mail, newsletters and notifications, Chat and the
calendar. The person's mail, sign-ins and settings are never read.

SCRIPT (a file, or `-` for stdin) drives the window, one command per line, `#` for comments; X Y
are window points from its top left (a PNG's pixels halved: shots are drawn at 2x):

    wait MS                    let MS pass (data arriving, animations)
    shot NAME                  save NAME-light.png / NAME-dark.png
    click X Y [right|double]   click there
    move X Y                   hover there
    drag X1 Y1 X2 Y2 [hold]    drag (`hold`: the button stays down, for a shot mid-drag)
    release                    let go of a held drag
    scroll X Y DY [DX]         scroll the view under X Y by DY points down (negative: up)
    key COMBO...               escape, down, enter, j, shift-tab, secondary-n (⌘N / Ctrl+N)
    type TEXT                  type into the focused field
    size W H                   resize the window
    window other|main          drive and shoot the other window the app opened (Settings, after
                               `key secondary-,`), or the main one again
    provider google|microsoft answer|hang|fail
                               that provider's requests from now on: answered as usual, never
                               (each times out after the engine's 30 s) or with a 503, for an
                               account that can't connect; `key secondary-shift-n` asks again
    freeze                     stop the pages' clock: a page opened, closed or let go from now on
                               stays where it starts (MS-289)
    film NAME COUNT [FPS]      move that clock on 1/FPS s (default 60) a frame, saving COUNT
                               frames NAME-000 on: a motion frame by frame, whatever drawing costs;
                               logs each frame's draw time (CPU: build, layout, paint) and a
                               summary past the first, to compare a motion's cost (MS-292)
    thaw                       run the pages' clock in real time again

The window opens at --size (default 1240x800, the app's) with the inbox loaded. Each input lets
0.6 s pass. Shot first, then click: open `NAME-light.png` to find where things are.

The PNGs go to DIR (default: a folder under the system's temporary folder, printed). --upload
puts each on Linear (the machine's app token: ~/.config/linear/token.json on Linux and Windows, the
`linear-omp` Keychain item on the Mac, as scripts/linear-poll.py reads it) and prints the
Markdown to paste into the issue's comment, light first, then dark.

The synthetic accounts are signed in to a stand-in provider (`Engine/src/shots.rs`, MS-265), so
marking read as a conversation opens, archive, star and send succeed and the screens after them
can be shot; the run prints each change the provider took (`shots: provider: …`).

What it can't show: the window is opaque (no Liquid Glass or compositor blur behind it); the Mac's
WebView renderer, menus, file dialogs and the touchpad's momentum are the real platform's. For
those, on Linux, use scripts/vhypr.py: it runs the app in a private nested Hyprland desktop and
screenshots that, real compositor and input included. shots.py is the default for hand-over
screenshots of GPUI; vhypr.py is for what needs the real desktop. Flutter isn't covered yet."""
import argparse
import importlib.util
import os
import platform
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LINUX = platform.system() == "Linux"
# The shots build, copied aside so a later default build in the same target can't stand in for it.
BINARY = "target/release/mailstream-gpui-shots"


def fail(message):
    sys.exit(f"shots: {message}")


def target_outside():
    """Linux: the workspace's target as a symlink outside the ovnfs mount (MS-201) when the
    checkout has none yet, seeded from the newest other checkout's build so a first build doesn't
    start cold."""
    inside = ROOT / "target"
    if inside.is_symlink() or inside.exists():
        return
    outside = Path.home() / ".cache/mailstream-checkouts" / ROOT.name / "target"
    if not outside.exists():
        others = [path for path in (Path.home() / ".cache/mailstream-checkouts").glob("*/target")
                  if (path / "release").is_dir() and path != outside]
        outside.parent.mkdir(parents=True, exist_ok=True)
        if others:
            newest = max(others, key=lambda path: (path / "release").stat().st_mtime)
            print(f"shots: starting the build from {newest}", file=sys.stderr)
            subprocess.run(["cp", "-a", "--reflink=auto", str(newest), str(outside)], check=True)
        else:
            outside.mkdir(parents=True)
    inside.symlink_to(outside, target_is_directory=True)


def run(commands):
    """Runs shell commands from the repo root; on Linux in `devenv shell` (GPUI loads Vulkan,
    Wayland's and fontconfig's libraries from there)."""
    script = " && ".join(commands)
    argv = ["devenv", "shell", "--", "sh", "-euc", script] if LINUX else ["sh", "-euc", script]
    result = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, text=True)
    if result.returncode:
        fail(f"exited {result.returncode}")
    return result.stdout


def linear():
    """scripts/linear-poll.py's token handling and `gql`, with this machine's token."""
    token = Path.home() / ".config/linear/token.json"
    if "LINEAR_TOKEN_FILE" not in os.environ and token.exists():
        os.environ["LINEAR_TOKEN_FILE"] = str(token)
        os.environ.setdefault("LINEAR_CREDENTIALS", str(Path.home() / ".config/linear/credentials"))
    sys.path.insert(0, str(ROOT / "scripts"))
    spec = importlib.util.spec_from_file_location("linear_poll", ROOT / "scripts/linear-poll.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def upload(poll, file):
    """Puts `file` on Linear's storage and returns its URL for Markdown."""
    data = file.read_bytes()
    out = poll.gql("""mutation($size: Int!, $name: String!) {
        fileUpload(contentType: "image/png", filename: $name, size: $size) {
            success uploadFile { uploadUrl assetUrl headers { key value } } } }""",
                   {"size": len(data), "name": file.name})["fileUpload"]
    if not out["success"]:
        fail(f"Linear refused the upload of {file.name}")
    target = out["uploadFile"]
    headers = {"Content-Type": "image/png", "Cache-Control": "public, max-age=31536000"}
    headers.update({header["key"]: header["value"] for header in target["headers"]})
    request = urllib.request.Request(target["uploadUrl"], data, headers, method="PUT")
    with urllib.request.urlopen(request, timeout=120):
        pass
    return target["assetUrl"]


def main():
    parser = argparse.ArgumentParser(description="Headless light and dark screenshots of the GPUI app.")
    parser.add_argument("script", help="the script's file, or - for stdin")
    parser.add_argument("--out", help="where the PNGs go")
    parser.add_argument("--size", default="1240x800", help="the window's size in points, WxH")
    parser.add_argument("--only", choices=["light", "dark"], help="one appearance only")
    parser.add_argument("--upload", action="store_true", help="upload to Linear and print Markdown")
    parser.add_argument("--no-build", action="store_true", help="run the last build as it is")
    parser.add_argument("--keep", action="store_true", help="keep the runs' data folders (printed)")
    args = parser.parse_args()

    if ROOT == (Path.home() / "projects/mailstream").resolve():
        fail("run it in your own checkout (scripts/checkout.py), not the default one")
    if args.script == "-":
        script = Path(tempfile.mkstemp(prefix="mailstream-shots-", suffix=".txt")[1])
        script.write_text(sys.stdin.read(), encoding="utf-8")
    else:
        script = Path(args.script).resolve()
        if not script.is_file():
            fail(f"{args.script}: no such file")
    out = Path(args.out).resolve() if args.out else Path(tempfile.gettempdir()) / "mailstream-shots" / script.stem
    out.mkdir(parents=True, exist_ok=True)

    commands = []
    if not args.no_build:
        if LINUX:
            target_outside()
        # Under the machine's compile limit (scripts/capped: Linux's capped slice, MS-268; the
        # Mac's compile budget, MS-260).
        commands.append("(cd GPUI && ../scripts/capped cargo build --release --features shots)")
        commands.append(f"cp target/release/mailstream-gpui {BINARY}")
    elif not (ROOT / BINARY).exists():
        fail(f"no {BINARY} yet: run it once without --no-build")
    appearances = [args.only] if args.only else ["light", "dark"]
    for appearance in appearances:
        flags = f"--size {args.size}" + (" --dark" if appearance == "dark" else "") + (" --keep" if args.keep else "")
        # Without a display to reach, nothing can open on the person's screen.
        commands.append(f"env -u WAYLAND_DISPLAY -u DISPLAY {BINARY} --shots '{script}' --out '{out}' {flags}")
    printed = run(commands)
    files = [Path(line) for line in printed.splitlines() if line.endswith(".png")]
    for file in files:
        print(file)
    if args.upload:
        poll = linear()
        print()
        for file in sorted(files, key=lambda file: file.stem.endswith("-dark")):
            name = file.stem.rsplit("-", 1)
            alt = f"{name[0]} ({'深色' if name[-1] == 'dark' else '浅色'})"
            print(f"![{alt}]({upload(poll, file)})")


if __name__ == "__main__":
    main()
