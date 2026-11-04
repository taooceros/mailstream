#!/bin/sh
# Builds, bundles and launches Mailstream, replacing a running instance.
# CONFIG=debug|release (default: debug, which rebuilds incrementally in seconds; a release build
# optimizes the whole module in one pass, ~30 s after any edit).
set -eu
cd "$(dirname "$0")/.."
CONFIG="${CONFIG:-debug}" ./scripts/bundle.sh
pkill -x Mailstream 2>/dev/null || true
open build/Mailstream.app
