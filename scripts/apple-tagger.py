#!/usr/bin/env python3
"""Builds mailstream-apple-tagger (Engine/apple/AppleTagger.swift), the AI tagger's Apple
Intelligence backend, into the given path. The front ends bundle it beside their executable,
where the engine looks for it. macOS only; needs Xcode with the macOS 26 SDK (Foundation Models).
Usage: scripts/apple-tagger.py <output path>"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "Engine/apple/AppleTagger.swift"

if len(sys.argv) != 2:
    sys.exit(__doc__)
output = Path(sys.argv[1])
output.parent.mkdir(parents=True, exist_ok=True)
# Skips the compile when the binary is newer than its source, as cargo does.
if output.exists() and output.stat().st_mtime >= SOURCE.stat().st_mtime:
    sys.exit(0)
result = subprocess.run(["xcrun", "swiftc", "-O", "-target", "arm64-apple-macos26.0", str(SOURCE), "-o", str(output)])
sys.exit(result.returncode)
