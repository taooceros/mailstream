#!/usr/bin/env python3
"""Publish an update prepared by prepare-update.py to the `updates` GitHub release.

next-version  print the build number the next update takes: one past both the newest published
              build and pubspec.yaml's (what scripts/install.sh builds), so versions only grow
publish DIR   upload DIR's versioned zip, then replace appcast.xml, after checking that the
              version is newer than every published one; keep the newest KEEP archives

Uses the gh command line (GH_TOKEN in CI). Runs one at a time: the workflow's concurrency group.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent.parent
RELEASE = "updates"
SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
ARCHIVE = re.compile(r"Mailstream-Flutter-([0-9.]+)\.zip")
# Archives a client may still be downloading from an earlier feed.
KEEP = 5


def gh(*args):
    return subprocess.run(["gh", *args], check=True, text=True, capture_output=True).stdout


def version_key(version):
    return tuple(int(part) for part in version.split("."))


def published_versions(feed):
    root = ET.fromstring(feed)
    return [item.findtext(f"{{{SPARKLE_NS}}}version") for item in root.findall("./channel/item")]


def pubspec_build():
    text = (ROOT / "Flutter/pubspec.yaml").read_text()
    return int(re.search(r"^version: *[^+\s]+\+([0-9]+)\s*$", text, re.M).group(1))


def published_feed():
    with tempfile.TemporaryDirectory() as folder:
        gh("release", "download", RELEASE, "--pattern", "appcast.xml", "--dir", folder)
        return (Path(folder) / "appcast.xml").read_bytes()


def next_version(feed, local):
    return max([version_key(v)[0] for v in published_versions(feed)] + [local]) + 1


def stale_archives(names, keep):
    archives = sorted((version_key(m.group(1)), name) for name in names if (m := ARCHIVE.fullmatch(name)))
    return [name for _, name in archives[:-keep]]


def publish(directory):
    manifest = json.loads((directory / "manifest.json").read_text())
    version, archive = manifest["bundleVersion"], directory / manifest["archive"]
    feed = directory / "appcast.xml"
    if ARCHIVE.fullmatch(archive.name) is None or published_versions(feed.read_bytes()) != [version]:
        raise ValueError("prepared feed must list exactly the prepared archive's version")
    newest = max(version_key(v) for v in published_versions(published_feed()))
    if version_key(version) <= newest:
        raise ValueError(f"build {version} is not newer than published build {'.'.join(map(str, newest))}")
    # The immutable archive first (gh refuses an existing name), then the feed that names it.
    gh("release", "upload", RELEASE, str(archive))
    gh("release", "upload", RELEASE, str(feed), "--clobber")
    if published_feed() != feed.read_bytes():
        raise ValueError("published appcast differs from the prepared one")
    assets = json.loads(gh("release", "view", RELEASE, "--json", "assets"))["assets"]
    for name in stale_archives([asset["name"] for asset in assets], KEEP):
        gh("release", "delete-asset", RELEASE, name, "--yes")
    print(f"Published build {version}: {archive.name} and appcast.xml")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("next-version")
    commands.add_parser("publish").add_argument("directory", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "next-version":
            print(next_version(published_feed(), pubspec_build()))
        else:
            publish(args.directory.resolve())
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"publish-update: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
