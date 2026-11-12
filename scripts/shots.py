#!/usr/bin/env python3
"""Delivery screenshots of the GPUI app without computer use (MS-243): one command builds the app
with its screenshot mode, fills a synthetic home, runs a script of clicks, keys and shots in light
and dark, and can attach the PNGs to a Linear issue.

Usage: python3 scripts/shots.py SCRIPT [--size WxH] [--appearance light,dark] [--out DIR]
                                 [--release] [--no-build] [--home DIR] [--upload MS-123]
       python3 scripts/shots.py -e 'click 600 290; wait 500; shot reader' ...

The app (GPUI/, built with `--features shots`, `src/shots.rs`) renders its window to PNGs with
GPUI's `render_to_image`: the frame as the GPU draws it, never read from the screen. So it works
with the Mac locked, the window behind others or the user at work; input goes to the window
alone, never to the user's pointer or keyboard, and the window never comes forward.

SCRIPT is a file (or `-e` text, `;` between commands) of the commands `src/shots.rs` lists,
one per line, `#` for comments; coordinates are the window's points, origin top left:
`size W H`, `move X Y`, `click X Y [right] [2] [cmd|shift|alt|ctrl]`, `scroll X Y DY [DX]`
(positive DY scrolls toward the top), `key escape down cmd-r`, `type TEXT`, `wait MS`, `shot NAME`,
`info`. `shot NAME` saves `OUT/NAME-light.png` and `OUT/NAME-dark.png` (2× on a Retina Mac),
taken once the window stops changing (bodies, avatars and animations land first). Each
appearance is its own launch at `--size` (default 1360x880), started with
`-AppleInterfaceStyle Light|Dark`, so the script starts from the same window both times. To find
coordinates, take a `shot` first and read them off the PNG (pixels / 2 on Retina).

The home is synthetic (`Engine/examples/shots_home.rs`: two accounts, a thread of three with
attachments and quotes, a people conversation, an invitation, a long HTML mail, filler mail, a
chat, three weeks of calendar), written fresh into a temporary folder for each run, or
`--home DIR` (written there once, kept and reused). The app runs with HOME and CFFIXED_USER_HOME
there and a Keychain item of its own, so it never sees the user's mail, settings or sign-ins and
reaches no provider. After the run the home is deleted unless `--home` named it.

Builds: debug by default into GPUI/target/debug (the shipped app's release build stays without
the feature); `--release` for performance-sensitive looks, `--no-build` reuses the last build.
Builds drop the SDKROOT-style variables as `scripts/ffi.py` does, so they reuse the warm build.

`--upload MS-123` uploads every PNG to Linear's private storage as the omp app (the poller's
token) and prints a Markdown image line per file, light before dark, to paste into the issue's
comment; nothing is posted by itself.

The Mac only: on Linux use `scripts/vhypr.py` (its private desktop takes screenshots already);
GPUI Fast's Linux renderer has no `render_to_image`."""
import argparse
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
sys.dont_write_bytecode = True
from ffi import ENV  # noqa: E402

READY = 60  # seconds for the window to open
STEP = 30  # seconds a command may take beyond its waits and shots (8 s each at most)


def fail(message):
    sys.exit(f"shots: {message}")


def commands(args):
    if args.e is not None:
        lines = [line.strip() for line in args.e.split(";")]
    else:
        lines = [line.strip() for line in Path(args.script).read_text(encoding="utf-8").splitlines()]
    lines = [line for line in lines if line and not line.startswith("#")]
    for line in lines:
        if line.split()[0] in ("quit", "appearance"):
            fail(f"`{line}`: the script runs once per appearance and quits by itself")
    return lines


def build(release):
    command = ["cargo", "build", "--features", "shots"] + (["--release"] if release else [])
    print("building:", " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT / "GPUI", check=True, env=ENV)


def seed(home):
    print(f"seeding {home}", flush=True)
    subprocess.run(["cargo", "run", "-q", "--example", "shots_home", "--", str(home)],
                   cwd=ROOT / "Engine", check=True, env=ENV, stdout=subprocess.DEVNULL)


def send(folder, lines, timeout):
    done = folder / "done"
    done.unlink(missing_ok=True)
    request = folder / "cmd.partial"
    request.write_text("\n".join(lines) + "\n", encoding="utf-8")
    request.rename(folder / "cmd")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if done.exists():
            answer = done.read_text(encoding="utf-8")
            done.unlink()
            return answer
        time.sleep(0.1)
    fail(f"no answer from the app in {timeout:.0f} s to: {'; '.join(lines)}")


def run(binary, home, out, appearance, size, lines):
    folder = Path(tempfile.mkdtemp(prefix="mailstream-shots-"))
    env = dict(ENV, HOME=str(home), CFFIXED_USER_HOME=str(home), MAILSTREAM_SHOTS=str(folder),
               MAILSTREAM_KEYCHAIN_SERVICE=f"dev.mailstream.shots.{os.getpid()}")
    log = open(home / f"app-{appearance}.log", "w")
    app = subprocess.Popen([str(binary), "-AppleInterfaceStyle", appearance.capitalize()], env=env,
                           stdout=log, stderr=subprocess.STDOUT, cwd=home)
    try:
        deadline = time.monotonic() + READY
        while not (folder / "ready").exists():
            if app.poll() is not None or time.monotonic() > deadline:
                fail(f"the app didn't open its window; see {home / log.name}")
            time.sleep(0.1)
        shots = []
        script = [f"size {size[0]} {size[1]}", "wait 500"]
        for line in lines:
            verb, _, rest = line.partition(" ")
            if verb == "shot":
                name = rest.strip()
                if not name:
                    fail("`shot` needs a name")
                path = out / f"{name}-{appearance}.png"
                shots.append(path)
                line = f"shot {path}"
            script.append(line)
        waits = sum(int(w) for w in re.findall(r"^wait (\d+)", "\n".join(script), re.M)) / 1000
        answer = send(folder, script, waits + 10 * len(shots) + STEP + len(script))
        print(f"[{appearance}]")
        print(answer, end="")
        if "=> error" in answer:
            fail(f"a command failed ({appearance}); see above")
        send(folder, ["quit"], STEP)
        app.wait(timeout=STEP)
        return shots
    finally:
        if app.poll() is None:
            app.kill()
        log.close()
        shutil.rmtree(folder, ignore_errors=True)


def upload(issue, files):
    """Uploads `files` to Linear's private storage; prints a Markdown line each."""
    spec = importlib.util.spec_from_file_location("linear_poll", ROOT / "scripts/linear-poll.py")
    linear = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(linear)
    import urllib.request
    for path in files:
        data = path.read_bytes()
        reserved = linear.gql(
            "mutation($type: String!, $name: String!, $size: Int!) { fileUpload(contentType: $type,"
            " filename: $name, size: $size, makePublic: false) { success uploadFile { uploadUrl assetUrl"
            " headers { key value } } } }",
            {"type": "image/png", "name": f"{issue.lower()}-{path.name}", "size": len(data)})["fileUpload"]
        if not reserved.get("success"):
            fail(f"Linear refused the upload of {path.name}")
        target = reserved["uploadFile"]
        headers = {"Content-Type": "image/png", "Cache-Control": "public, max-age=31536000"}
        headers.update({h["key"]: h["value"] for h in target["headers"]})
        request = urllib.request.Request(target["uploadUrl"], data=data, headers=headers, method="PUT")
        with urllib.request.urlopen(request, timeout=120) as response:
            if response.status not in (200, 201):
                fail(f"uploading {path.name} answered {response.status}")
        print(f"![{path.stem}]({target['assetUrl']})")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("script", nargs="?", help="file of commands")
    parser.add_argument("-e", metavar="COMMANDS", help="commands inline, ';' between them")
    parser.add_argument("--size", default="1360x880", help="window content size in points (1360x880)")
    parser.add_argument("--appearance", default="light,dark", help="light, dark or both (light,dark)")
    parser.add_argument("--out", default="shots", help="folder for the PNGs (./shots)")
    parser.add_argument("--release", action="store_true", help="release build")
    parser.add_argument("--no-build", action="store_true", help="use the last build")
    parser.add_argument("--home", help="synthetic home to write once and keep")
    parser.add_argument("--upload", metavar="ISSUE", help="upload the PNGs for this issue, print Markdown")
    args = parser.parse_args()
    if sys.platform != "darwin":
        fail("the Mac only; on Linux use scripts/vhypr.py")
    if (args.script is None) == (args.e is None):
        parser.error("give a SCRIPT file or -e, one of them")
    match = re.fullmatch(r"(\d+)x(\d+)", args.size)
    if not match:
        parser.error("--size is WxH, e.g. 1360x880")
    appearances = args.appearance.split(",")
    if any(a not in ("light", "dark") for a in appearances):
        parser.error("--appearance takes light, dark or light,dark")
    lines = commands(args)

    binary = ROOT / "GPUI/target" / ("release" if args.release else "debug") / "mailstream-gpui"
    if not args.no_build:
        build(args.release)
    elif not binary.exists():
        fail(f"no build at {binary}; run without --no-build")
    subprocess.run([str(ROOT / "scripts/sign.sh"), str(binary)], check=True)
    home = Path(args.home).resolve() if args.home else Path(tempfile.mkdtemp(prefix="mailstream-shots-home-"))
    if not (home / "Library/Application Support/Mailstream/mail.db").exists():
        seed(home)
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    try:
        shots = []
        for appearance in appearances:
            shots += run(binary, home, out, appearance, (match[1], match[2]), lines)
    finally:
        if not args.home:
            shutil.rmtree(home, ignore_errors=True)
    print("\n".join(str(path) for path in shots))
    if args.upload:
        upload(args.upload, sorted(shots, key=lambda p: (p.stem.rsplit("-", 1)[0], p.stem.endswith("-dark"))))


if __name__ == "__main__":
    main()
