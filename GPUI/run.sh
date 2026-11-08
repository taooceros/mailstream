#!/bin/sh
# Builds the GPUI front end with the engine it links, signs it like the app and runs it.
# The engine reads the sign-ins from the Keychain item macOS pins to Mailstream's signature;
# a binary signed any other way makes macOS ask, and "Always Allow" there re-pins the item to
# that one build. So this refuses to start a binary that did not get the app's signature.
# Usage: [CONFIG=release] GPUI/run.sh
#   CONFIG=release builds the engine and the window optimized.
set -eu
cd "$(dirname "$0")/.."
CONFIG=${CONFIG:-debug}

if [ "$CONFIG" = release ]; then
  (cd GPUI && cargo build --release)
  BINARY=GPUI/target/release/mailstream-gpui
else
  (cd GPUI && cargo build)
  BINARY=GPUI/target/debug/mailstream-gpui
fi

./scripts/sign.sh "$BINARY"
SIGNATURE=$(codesign -dv "$BINARY" 2>&1)
case "$SIGNATURE" in
  *"Identifier=dev.mailstream.Mailstream"*"TeamIdentifier="[A-Z0-9]*) ;;
  *)
    echo "error: $BINARY is not signed as dev.mailstream.Mailstream with a team certificate; not starting it" >&2
    echo "$SIGNATURE" >&2
    exit 1
    ;;
esac
exec "$BINARY"
