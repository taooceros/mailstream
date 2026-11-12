#!/usr/bin/env python3
"""Publish an update prepared by prepare-update.py to the `updates` GitHub release.

next-version APP  print the build number APP's next update takes: one past both its newest
                  published build and the local build's (pubspec.yaml's for flutter, 1 for gpui:
                  what scripts/install.sh builds), so versions only grow
publish DIR       upload DIR's versioned zip, then replace its app's feed, after checking that
                  the version is newer than every published one; keep the newest KEEP archives

APP is flutter (appcast.xml, Mailstream-Flutter-N.zip) or gpui (appcast-gpui.xml,
Mailstream-GPUI-N.zip, MS-219); both live in the one `updates` release, each with its own feed.
Uses the gh command line (GH_TOKEN in CI). Runs one at a time per app: the workflows'
concurrency groups.
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



class Product:
    def __init__(self, prefix, feed, local_build):
        self.archive = re.compile(re.escape(prefix) + r"-([0-9.]+)\.zip")
        self.feed = feed
        self.local_build = local_build


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


PRODUCTS = {
    "flutter": Product("Mailstream-Flutter", "appcast.xml", pubspec_build),
    "gpui": Product("Mailstream-GPUI", "appcast-gpui.xml", lambda: 1),
}


def published_feed(product):
    """The product's published feed; an empty one before its first publication."""
    assets = json.loads(gh("release", "view", RELEASE, "--json", "assets"))["assets"]
    if product.feed not in [asset["name"] for asset in assets]:
        return b"<rss><channel/></rss>"
    with tempfile.TemporaryDirectory() as folder:
        gh("release", "download", RELEASE, "--pattern", product.feed, "--dir", folder)
        return (Path(folder) / product.feed).read_bytes()


def next_version(feed, local):
    return max([version_key(v)[0] for v in published_versions(feed)] + [local]) + 1


def stale_archives(names, keep, product):
    archives = sorted((version_key(m.group(1)), name) for name in names if (m := product.archive.fullmatch(name)))
    return [name for _, name in archives[:-keep]]


def publish(directory):
    manifest = json.loads((directory / "manifest.json").read_text())
    version, archive = manifest["bundleVersion"], directory / manifest["archive"]
    matching = [p for p in PRODUCTS.values() if p.archive.fullmatch(archive.name)]
    if len(matching) != 1:
        raise ValueError("prepared archive belongs to no published app")
    product = matching[0]
    feed = directory / product.feed
    if published_versions(feed.read_bytes()) != [version]:
        raise ValueError("prepared feed must list exactly the prepared archive's version")
    newest = max((version_key(v) for v in published_versions(published_feed(product))), default=(0,))
    if version_key(version) <= newest:
        raise ValueError(f"build {version} is not newer than published build {'.'.join(map(str, newest))}")
    # The immutable archive first (gh refuses an existing name), then the feed that names it.
    gh("release", "upload", RELEASE, str(archive))
    gh("release", "upload", RELEASE, str(feed), "--clobber")
    if published_feed(product) != feed.read_bytes():
        raise ValueError("published appcast differs from the prepared one")
    assets = json.loads(gh("release", "view", RELEASE, "--json", "assets"))["assets"]
    for name in stale_archives([asset["name"] for asset in assets], KEEP, product):
        gh("release", "delete-asset", RELEASE, name, "--yes")
    print(f"Published build {version}: {archive.name} and {product.feed}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("next-version").add_argument("app", choices=PRODUCTS)
    commands.add_parser("publish").add_argument("directory", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "next-version":
            product = PRODUCTS[args.app]
            print(next_version(published_feed(product), product.local_build()))
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
