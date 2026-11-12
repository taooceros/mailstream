#!/usr/bin/env python3
"""Builds the engine for FFI front ends: the UniFFI crate in FFI/ with the Rust engine in Engine/
(libmailstream.dylib on macOS, mailstream.dll on Windows, libmailstream.so on Linux), the
mailstream command line (FFI/target/<config>/mailstream, which the Flutter app bundles on macOS
and Linux), on macOS the Apple Intelligence tagger (FFI/target/<config>/mailstream-apple-tagger,
bundled beside it) and, with DART_OUT=<dir>, the Dart bindings into <dir>.
CONFIG=debug|release (default: debug).

The library and the command line come from one cargo run in FFI/'s workspace, so the engine and
its dependencies compile once into FFI/target (a second run in Engine/ compiled them all again
into Engine/target: ~3 min more from cold and every engine edit built twice, MS-159)."""
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.dont_write_bytecode = True
from repo import ROOT, build_env  # noqa: E402

CONFIG = os.environ.get("CONFIG", "debug")
LIBRARY = {"win32": "mailstream.dll", "darwin": "libmailstream.dylib"}.get(sys.platform, "libmailstream.so")
PROFILE = ["--release"] if CONFIG == "release" else []
# Without SDKROOT, CPATH and the like on macOS, which rebuilt C crates in every checkout (repo.py).
ENV = build_env()


def cargo(directory, *args):
    subprocess.run(["cargo", *args], cwd=ROOT / directory, check=True, env=ENV)


def main():
    cargo("FFI", "build", *PROFILE, "-p", "mailstream", "-p", "mailstream-engine", "--lib", "--bins")
    print(f"Built FFI/target/{CONFIG}/{LIBRARY} and FFI/target/{CONFIG}/mailstream", flush=True)
    if sys.platform == "darwin":
        tagger = ROOT / f"FFI/target/{CONFIG}/mailstream-apple-tagger"
        subprocess.run([sys.executable, str(ROOT / "scripts/apple-tagger.py"), str(tagger)], check=True)
        print(f"Built {tagger.relative_to(ROOT)}", flush=True)
    if os.environ.get("DART_OUT"):
        cargo("FFI", "run", "-q", "-p", "mailstream-bindgen", "--", f"target/{CONFIG}/{LIBRARY}",
              "--out-dir", os.environ["DART_OUT"])


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
