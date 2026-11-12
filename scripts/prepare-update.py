#!/usr/bin/env python3
"""Prepare (never publish) a Sparkle update from an already signed Flutter or GPUI app."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

FEED_ROOT = "https://github.com/taooceros/mailstream/releases/download/updates/"
SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
TEAM = "K7HG6KZNNP"
IDENTIFIER = "dev.mailstream.Mailstream"
# Each app published in the `updates` release, by bundle identifier: its archives' name and its
# own feed (scripts/publish-update.py takes the same table).
PRODUCTS = {
    "dev.mailstream.MailstreamFlutter": ("Mailstream-Flutter", "appcast.xml"),
    "dev.mailstream.MailstreamGPUI": ("Mailstream-GPUI", "appcast-gpui.xml"),
}


def run(*args):
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout


def validate_info(info):
    key = info.get("SUPublicEDKey", "")
    try:
        valid_key = len(base64.b64decode(key, validate=True)) == 32
    except (ValueError, TypeError):
        valid_key = False
    if not valid_key:
        raise ValueError("app needs a real SUPublicEDKey; no signing keys are created by this tool")
    product = PRODUCTS.get(info.get("CFBundleIdentifier"))
    if product is None:
        raise ValueError("only the Flutter and GPUI apps are distributed by this channel")
    if info.get("SUFeedURL") != FEED_ROOT + product[1]:
        raise ValueError("app must use its own feed in the Mailstream updates release, never ad-hoc nightly")
    if info.get("SUVerifyUpdateBeforeExtraction") is not True:
        raise ValueError("archive verification before extraction is required")
    if info.get("SURequireSignedFeed") is not True or info.get("SUSignedFeedFailureExpirationInterval") != 0:
        raise ValueError("signed appcasts without expiration fallback are required")
    if info.get("MSUpdateChannel") not in ("stable", "prerelease"):
        raise ValueError("bundle needs an explicit stable or prerelease update channel")
    version = info.get("CFBundleVersion", "")
    if not re.fullmatch(r"[1-9][0-9]{0,3}(?:\.[0-9]{1,2}){0,2}", version):
        raise ValueError("CFBundleVersion must be incremented for every release (up to 4.2.2 digits)")
    return version


def validate_feed(feed, archive, version, channel):
    root = ET.parse(feed).getroot()
    matches = [item for item in root.findall("./channel/item")
               if item.findtext(f"{{{SPARKLE_NS}}}version") == version]
    if len(matches) != 1:
        raise ValueError("generated appcast must contain exactly this bundle version")
    item_channel = matches[0].findtext(f"{{{SPARKLE_NS}}}channel")
    if item_channel != (None if channel == "stable" else channel):
        raise ValueError("appcast channel does not match the built application's release type")
    enclosure = matches[0].find("enclosure")
    if enclosure is None or enclosure.get("url") != FEED_ROOT + archive.name:
        raise ValueError("appcast URL does not name the prepared archive")
    if enclosure.get("length") != str(archive.stat().st_size):
        raise ValueError("appcast length does not match package bytes")
    signature = enclosure.get(f"{{{SPARKLE_NS}}}edSignature", "")
    if len(base64.b64decode(signature, validate=True)) != 64:
        raise ValueError("appcast has no valid Ed25519 signature")
    return signature


def prepare(app, output, tools, key_file):
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    version = validate_info(info)
    prefix, feed_name = PRODUCTS[info["CFBundleIdentifier"]]
    run("codesign", "--verify", "--deep", "--strict", str(app))
    signature = subprocess.run(["codesign", "-d", "-v", str(app)],
                               check=True, text=True, capture_output=True).stderr
    if f"TeamIdentifier={TEAM}\n" not in signature or f"Identifier={IDENTIFIER}\n" not in signature:
        raise ValueError("app must retain Mailstream's shared signing team and identifier")
    if output.exists():
        raise ValueError("output directory already exists; retain immutable versioned archives")
    output.mkdir(parents=True)
    # All checks happen in a staging directory; a failed command never leaves a publishable feed.
    try:
        with tempfile.TemporaryDirectory(prefix="mailstream-update-") as staging:
            staging = Path(staging)
            archive = staging / f"{prefix}-{version}.zip"
            run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive))
            args = [str(tools / "generate_appcast"), "--maximum-deltas", "0",
                    "--download-url-prefix", FEED_ROOT]
            if info["MSUpdateChannel"] == "prerelease":
                args += ["--channel", "prerelease"]
            if key_file:
                args += ["--ed-key-file", str(key_file)]
            run(*args, str(staging))
            # generate_appcast names the feed after the app's SUFeedURL, which validate_info checked.
            feed = staging / feed_name
            ed_signature = validate_feed(feed, archive, version, info["MSUpdateChannel"])
            # Verify against the PUBLIC key embedded in the app, not just the signing key.
            run("swift", str(Path(__file__).with_name("verify-update.swift")),
                info["SUPublicEDKey"], str(archive), ed_signature)
            verify_feed = [str(tools / "sign_update"), "--verify"]
            if key_file:
                verify_feed += ["--ed-key-file", str(key_file)]
            run(*verify_feed, str(feed))
            digest = hashlib.sha256()
            with archive.open("rb") as file:
                for chunk in iter(lambda: file.read(1024 * 1024), b""):
                    digest.update(chunk)
            manifest = {"bundleVersion": version, "shortVersion": info["CFBundleShortVersionString"],
                        "revision": info.get("MSInstalledRevision"), "archive": archive.name,
                        "bytes": archive.stat().st_size,
                        "sha256": digest.hexdigest(),
                        "channel": info["MSUpdateChannel"],
                        "feedURL": FEED_ROOT + feed_name, "published": False}
            # Move only the validated final artifacts, never keys, cache, apps or extra files.
            archive.rename(output / archive.name)
            feed.rename(output / feed_name)
            (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    except BaseException:
        for child in output.iterdir():
            child.unlink()
        output.rmdir()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--sparkle-bin", type=Path, required=True,
                        help="bin directory from pinned Sparkle 2.10.1 distribution")
    parser.add_argument("--key-file", type=Path,
                        help="optional private seed file outside the repo; otherwise Sparkle uses Keychain")
    args = parser.parse_args()
    try:
        prepare(args.app.resolve(), args.output.resolve(), args.sparkle_bin.resolve(),
                args.key_file.resolve() if args.key_file else None)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"prepare-update: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        return 1
    print(f"Prepared verified artifacts in {args.output}; nothing has been published.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
