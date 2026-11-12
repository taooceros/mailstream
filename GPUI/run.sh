#!/bin/sh
# Builds the GPUI front end with the engine it links and runs it; on the Mac it signs it like the
# app first. The engine reads the sign-ins from the Keychain item macOS pins to Mailstream's
# signature; a binary signed any other way makes macOS ask, and "Always Allow" there re-pins the
# item to that one build. So on the Mac this refuses to start a binary that did not get the app's
# signature. Linux keeps the sign-ins in a file of the user's (Engine/src/keychain.rs) and needs
# no signature; on NixOS run it inside `devenv shell`, which provides the libraries GPUI loads.
# Usage: [CONFIG=release] GPUI/run.sh
#   CONFIG=release builds the engine and the window optimized.
set -eu
cd "$(dirname "$0")/.."
CONFIG=${CONFIG:-debug}
TARGET=${CARGO_TARGET_DIR:-GPUI/target}

if [ "$CONFIG" = release ]; then
  (cd GPUI && ../scripts/capped cargo build --release)
  BINARY=$TARGET/release/mailstream-gpui
else
  (cd GPUI && ../scripts/capped cargo build)
  BINARY=$TARGET/debug/mailstream-gpui
fi

if [ "$(uname)" != Darwin ]; then
  exec "$BINARY"
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
