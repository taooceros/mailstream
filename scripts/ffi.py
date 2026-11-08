#!/usr/bin/env python3
"""Builds the engine for FFI front ends: the UniFFI crate in FFI/ with the Rust engine in Engine/
(libmailstream.dylib on macOS, mailstream.dll on Windows), the mailstream command line
(Engine/target/<config>/mailstream, which the Flutter app bundles on macOS) and, with
DART_OUT=<dir>, the Dart bindings into <dir>.
CONFIG=debug|release (default: debug)."""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = os.environ.get("CONFIG", "debug")
LIBRARY = "mailstream.dll" if sys.platform == "win32" else "libmailstream.dylib"
PROFILE = ["--release"] if CONFIG == "release" else []


def cargo(directory, *args):
    subprocess.run(["cargo", *args], cwd=ROOT / directory, check=True)


try:
    cargo("FFI", "build", *PROFILE, "-p", "mailstream")
    cargo("Engine", "build", *PROFILE, "--bin", "mailstream")
    print(f"Built FFI/target/{CONFIG}/{LIBRARY} and Engine/target/{CONFIG}/mailstream", flush=True)
    if os.environ.get("DART_OUT"):
        cargo("FFI", "run", "-q", "-p", "mailstream-bindgen", "--", f"target/{CONFIG}/{LIBRARY}",
              "--out-dir", os.environ["DART_OUT"])
except subprocess.CalledProcessError as error:
    sys.exit(error.returncode)
