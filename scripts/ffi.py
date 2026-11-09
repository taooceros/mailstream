#!/usr/bin/env python3
"""Builds the engine for FFI front ends: the UniFFI crate in FFI/ with the Rust engine in Engine/
(libmailstream.dylib on macOS, mailstream.dll on Windows), the mailstream command line
(FFI/target/<config>/mailstream, which the Flutter app bundles on macOS), on macOS the Apple
Intelligence tagger (FFI/target/<config>/mailstream-apple-tagger, bundled beside it) and, with
DART_OUT=<dir>, the Dart bindings into <dir>.
CONFIG=debug|release (default: debug).

The library and the command line come from one cargo run in FFI/'s workspace, so the engine and
its dependencies compile once into FFI/target (a second run in Engine/ compiled them all again
into Engine/target: ~3 min more from cold and every engine edit built twice, MS-159)."""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = os.environ.get("CONFIG", "debug")
LIBRARY = "mailstream.dll" if sys.platform == "win32" else "libmailstream.dylib"
PROFILE = ["--release"] if CONFIG == "release" else []
# C build scripts (ring, simsimd) rebuild, and everything above them with them, whenever one of
# these differs from the build being reused. Shells pick them up unevenly (the warm build of
# 11-09 had SDKROOT and CPATH, agents' shells don't: every checkout rebuilt 15 crates twice,
# MS-159); on macOS the C compiler finds the SDK through xcrun without them.
DRIFTING = ["SDKROOT", "CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH"]
ENV = {k: v for k, v in os.environ.items() if sys.platform != "darwin" or k not in DRIFTING}


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
