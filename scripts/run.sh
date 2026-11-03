#!/bin/sh
# Builds, bundles and launches Mailstream, replacing a running instance.
set -eu
cd "$(dirname "$0")/.."
./scripts/bundle.sh
pkill -x Mailstream 2>/dev/null || true
open build/Mailstream.app
