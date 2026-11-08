#!/usr/bin/env python3
"""Opens a Linear issue in an omp session in a new Tern tab at this repo; the session opens its
own checkout to work in (AGENTS.md, Version control). Linear's "Work on issue → Custom script"
runs it (~/.linear/coding-tools.json: args ["{{issue.identifier}}"]); it also works by hand:
scripts/linear-omp.py MS-12."""
import subprocess
import sys
from pathlib import Path

if len(sys.argv) != 2:
    sys.exit(f"usage: {sys.argv[0]} <issue>")
ROOT = Path(__file__).resolve().parent.parent
sys.exit(subprocess.run(["tern", "new", "tab", "--cwd", str(ROOT), "--", "omp",
                         f"Work on Linear issue {sys.argv[1]}."]).returncode)
