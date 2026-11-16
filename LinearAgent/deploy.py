#!/usr/bin/env python3
"""Deploys the Linear agent inbox (worker.js, MS-377) to the Cloudflare account wrangler is logged
in to (`npx wrangler@4 login` once) and sets its secrets. Python standard library and npx only.

Keeps its state in LINEAR_AGENT_DIR (default ~/.config/linear-agent, mode 0700), which
`scripts/linear-agent.py` reads:
  pull-token      the Worker's PULL_TOKEN; made here (random) the first time
  url             the Worker's https://….workers.dev URL, from wrangler's output
  webhook-secret  the signing secret Linear shows on the app's settings page once webhooks are
                  on; you put it there. Without it the Worker answers Linear's webhook with 503.
Run it again after changing worker.js or adding webhook-secret. It deploys from a temporary copy,
so wrangler's `.wrangler/` stays out of the repo. Nothing secret is printed.
"""
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONFIG = Path(os.environ.get("LINEAR_AGENT_DIR") or Path.home() / ".config/linear-agent")
WRANGLER = ["npx", "--yes", "wrangler@4"]


def private(path, text):
    path.touch(mode=0o600, exist_ok=True)
    path.chmod(0o600)
    path.write_text(text + "\n", encoding="utf-8")


def main():
    CONFIG.mkdir(mode=0o700, parents=True, exist_ok=True)
    pull = CONFIG / "pull-token"
    if not pull.exists():
        private(pull, secrets.token_hex(32))
    with tempfile.TemporaryDirectory() as folder:
        for name in ("worker.js", "wrangler.toml"):
            shutil.copy(HERE / name, folder)
        out = subprocess.run(WRANGLER + ["deploy"], cwd=folder, capture_output=True, text=True)
        if out.returncode:
            sys.exit(out.stdout + out.stderr)
        url = re.search(r"https://[a-z0-9.-]+\.workers\.dev", out.stdout + out.stderr)
        if not url:
            sys.exit(out.stdout + out.stderr + "\nNo workers.dev URL in wrangler's output.")
        (CONFIG / "url").write_text(url.group(0) + "\n", encoding="utf-8")
        names = {"PULL_TOKEN": pull, "LINEAR_WEBHOOK_SECRET": CONFIG / "webhook-secret"}
        for name, path in names.items():
            if not path.exists():
                continue
            value = path.read_text(encoding="utf-8").strip()
            put = subprocess.run(WRANGLER + ["secret", "put", name], cwd=folder, input=value,
                                 capture_output=True, text=True)
            if put.returncode:
                sys.exit(f"wrangler secret put {name} failed:\n{put.stdout}{put.stderr}")
    print(f"Worker at {url.group(0)}; Linear's webhook URL: {url.group(0)}/webhook")
    if not (CONFIG / "webhook-secret").exists():
        print(f"Next: put the app's webhook signing secret in {CONFIG / 'webhook-secret'} and run this again.")


if __name__ == "__main__":
    main()
