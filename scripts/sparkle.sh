#!/bin/sh
# Fetches the pinned Sparkle distribution once into build/sparkle-<version> (checked against its
# SHA-256) and prints that folder: Sparkle.framework for Mailstream GPUI (gpui-bundle.sh), bin/
# for signing updates (ci-publish.sh). The one place its version and checksum are written in
# scripts and CI (MS-239); Mailstream Flutter's Xcode project pins the same version as a Swift
# package (Flutter/macos/Runner.xcodeproj and its Package.resolved), so change them together.
# Usage: scripts/sparkle.sh
set -eu
cd "$(dirname "$0")/.."
version=2.10.1
sha256=1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27
sparkle="build/sparkle-$version"

if [ ! -d "$sparkle/Sparkle.framework" ]; then
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  curl --fail --silent --show-error --location --retry 8 --retry-delay 45 --retry-all-errors \
    --output "$work/Sparkle.tar.xz" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz"
  printf '%s  %s\n' "$sha256" "$work/Sparkle.tar.xz" | shasum -a 256 -c - >/dev/null
  mkdir "$work/sparkle"
  tar -xf "$work/Sparkle.tar.xz" -C "$work/sparkle"
  mkdir -p build
  rm -rf "$sparkle"
  mv "$work/sparkle" "$sparkle"
fi
echo "$PWD/$sparkle"
