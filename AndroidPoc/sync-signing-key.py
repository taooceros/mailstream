#!/usr/bin/env python3
"""Pulls Mailstream's shared Android signing keystore from a private Linear document.

Usage:
  python3 AndroidPoc/sync-signing-key.py --doc-id <LINEAR_DOC_ID>
  MAILSTREAM_ANDROID_SIGNING_DOC_ID=<LINEAR_DOC_ID> python3 AndroidPoc/sync-signing-key.py

Writes (chmod 600, inside chmod 700 directory):
  ~/.config/mailstream/android-signing.jks
  ~/.config/mailstream/android-signing.env

No secrets, passwords, or private workspace IDs are stored in this script or in git.
Linear authentication matches scripts/linear-poll.py:
  - LINEAR_TOKEN env var, or
  - LINEAR_TOKEN_FILE / ~/.config/linear/token.json (Linux/Windows, renewed via credentials), or
  - macOS login Keychain service `linear-mac-omp` account `access_token`.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

LINEAR_SCOPE = os.environ.get(
    "LINEAR_SCOPE", "read,write,app:assignable,app:mentionable"
)
MAC_RENEW = Path.home() / ".pi/agent/extensions/linear/renew-omp-token.sh"
DOC_QUERY = """
query($id: String!) {
  document(id: $id) {
    id
    title
    content
  }
}
"""


def token_file() -> Path | None:
    explicit = os.environ.get("LINEAR_TOKEN_FILE", "").strip()
    if explicit:
        return Path(explicit).expanduser()
    default_linux = Path.home() / ".config/linear/token.json"
    if default_linux.exists():
        return default_linux
    return None


def credentials_file() -> Path:
    explicit = os.environ.get("LINEAR_CREDENTIALS", "").strip()
    if explicit:
        return Path(explicit).expanduser()
    return Path.home() / ".config/linear/credentials"


def get_token() -> str:
    direct = os.environ.get("LINEAR_TOKEN", "").strip()
    if direct:
        return direct
    tf = token_file()
    if tf is not None:
        return json.loads(tf.read_text(encoding="utf-8"))["access_token"]
    if sys.platform == "darwin":
        return subprocess.run(
            [
                "security",
                "find-generic-password",
                "-s",
                "linear-mac-omp",
                "-a",
                "access_token",
                "-w",
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    raise RuntimeError(
        "No Linear token found: set LINEAR_TOKEN or configure ~/.config/linear/token.json"
    )


def renew_token() -> bool:
    if os.environ.get("LINEAR_TOKEN", "").strip():
        return False
    tf = token_file()
    if tf is not None:
        creds = credentials_file()
        if not creds.exists():
            return False
        client_id, secret = creds.read_text(encoding="utf-8").splitlines()[:2]
        form = urllib.parse.urlencode(
            {
                "grant_type": "client_credentials",
                "client_id": client_id.strip(),
                "client_secret": secret.strip(),
                "scope": LINEAR_SCOPE,
            }
        ).encode()
        with urllib.request.urlopen(
            "https://api.linear.app/oauth/token", form, timeout=60
        ) as resp:
            out = json.load(resp)
        if not out.get("access_token"):
            raise RuntimeError("Linear token renewal returned no access_token")
        tf.parent.mkdir(parents=True, exist_ok=True)
        tf.write_text(json.dumps(out), encoding="utf-8")
        return True
    if sys.platform == "darwin" and MAC_RENEW.exists():
        subprocess.run([str(MAC_RENEW)], check=True, stdout=subprocess.DEVNULL)
        return True
    return False


def linear_graphql(query: str, variables: dict) -> dict:
    body = json.dumps({"query": query, "variables": variables}).encode()

    def post() -> dict:
        req = urllib.request.Request(
            "https://api.linear.app/graphql",
            body,
            {
                "Authorization": f"Bearer {get_token()}",
                "Content-Type": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as err:
            raw = err.read()
        return json.loads(raw)

    out = post()
    if any(
        (e.get("extensions") or {}).get("code") == "AUTHENTICATION_ERROR"
        for e in out.get("errors") or []
    ):
        if renew_token():
            out = post()
    if out.get("errors"):
        raise RuntimeError(f"Linear GraphQL error: {out['errors']}")
    return out


def parse_payload(markdown: str) -> dict:
    blocks = re.findall(r"```json\s*\n(.*?)\n```", markdown, re.DOTALL)
    for block in reversed(blocks):
        try:
            obj = json.loads(block)
        except ValueError:
            continue
        if isinstance(obj, dict) and obj.get("format") == "mailstream-android-signing-v1":
            return obj
    raise RuntimeError(
        "Could not find a mailstream-android-signing-v1 JSON payload block in the Linear document"
    )


def write_private_file(path: Path, data: bytes) -> None:
    tmp = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        os.write(fd, data)
    finally:
        os.close(fd)
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)
    os.chmod(path, 0o600)


def verify_keytool_cert(ks_path: Path, alias: str, password: str, expected_colon: str) -> str:
    env = os.environ.copy()
    env["MS_KS_PASS"] = password
    res = subprocess.run(
        [
            "keytool",
            "-list",
            "-v",
            "-keystore",
            str(ks_path),
            "-storepass:env",
            "MS_KS_PASS",
            "-alias",
            alias,
        ],
        env=env,
        capture_output=True,
        text=True,
    )
    if res.returncode != 0:
        raise RuntimeError(f"keytool verification failed: {res.stderr.strip() or res.stdout.strip()}")
    match = re.search(r"SHA256:\s*([0-9A-Fa-f:]+)", res.stdout)
    if not match:
        raise RuntimeError("keytool output did not contain a SHA256 certificate fingerprint")
    actual = match.group(1).upper()
    if expected_colon and actual != expected_colon.upper():
        raise RuntimeError(
            f"Certificate SHA-256 mismatch: expected {expected_colon.upper()}, got {actual}"
        )
    return actual


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Sync Mailstream's shared Android signing keystore from a private Linear document."
    )
    parser.add_argument(
        "--doc-id",
        default=os.environ.get("MAILSTREAM_ANDROID_SIGNING_DOC_ID", "").strip(),
        help="Linear document UUID holding the signing payload (or set MAILSTREAM_ANDROID_SIGNING_DOC_ID).",
    )
    parser.add_argument(
        "--dir",
        default=os.environ.get(
            "MAILSTREAM_ANDROID_SIGNING_DIR", str(Path.home() / ".config/mailstream")
        ),
        help="Destination directory (default: ~/.config/mailstream).",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite existing android-signing.jks / android-signing.env.",
    )
    args = parser.parse_args()

    if not args.doc_id:
        parser.error("Provide --doc-id <LINEAR_DOC_ID> or set MAILSTREAM_ANDROID_SIGNING_DOC_ID")

    dest_dir = Path(args.dir).expanduser()
    ks_path = dest_dir / "android-signing.jks"
    env_path = dest_dir / "android-signing.env"

    if not args.force and (ks_path.exists() or env_path.exists()):
        print(
            f"error: {ks_path} or {env_path} already exists (pass --force to overwrite)",
            file=sys.stderr,
        )
        return 1

    resp = linear_graphql(DOC_QUERY, {"id": args.doc_id})
    doc = (resp.get("data") or {}).get("document")
    if not doc or not doc.get("content"):
        print(f"error: Linear document {args.doc_id} not found or empty", file=sys.stderr)
        return 1

    payload = parse_payload(doc["content"])
    ks_bytes = base64.b64decode(payload["keystore_base64"])
    actual_ks_sha = hashlib.sha256(ks_bytes).hexdigest()
    expected_ks_sha = payload.get("keystore_sha256", "").lower()
    if expected_ks_sha and actual_ks_sha != expected_ks_sha:
        raise RuntimeError(
            f"Keystore SHA-256 mismatch: expected {expected_ks_sha}, got {actual_ks_sha}"
        )

    alias = payload["alias"]
    ks_pass = payload["keystore_pass"]
    key_pass = payload.get("key_pass", ks_pass)
    if any(ch in ks_pass or ch in key_pass or ch in alias for ch in ('"', "$", "`", "\\", "\n", "\r")):
        raise RuntimeError("Refusing to write signing payload with shell-unsafe characters")

    dest_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(dest_dir, 0o700)
    write_private_file(ks_path, ks_bytes)

    env_text = (
        "# Mailstream shared Android APK signing configuration (chmod 600; NEVER commit to git).\n"
        f'ANDROID_KEYSTORE="{ks_path}"\n'
        f'ANDROID_KEY_ALIAS="{alias}"\n'
        f'ANDROID_KEYSTORE_PASS="{ks_pass}"\n'
        f'ANDROID_KEY_PASS="{key_pass}"\n'
    )
    write_private_file(env_path, env_text.encode("utf-8"))

    cert_sha = verify_keytool_cert(ks_path, alias, ks_pass, payload.get("cert_sha256_colon", ""))

    print(f"Synced Mailstream Android signing keystore from Linear doc {args.doc_id}")
    print(f"  keystore:        {ks_path} (mode 0600, sha256={actual_ks_sha})")
    print(f"  env:             {env_path} (mode 0600)")
    print(f"  alias:           {alias}")
    print(f"  cert SHA-256:    {cert_sha}")
    print(f"  apksigner hex:   {cert_sha.replace(':', '').lower()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
