#!/bin/sh
# Builds the engine for FFI front ends: libMailstreamKit.dylib (Swift), libmailstream.dylib (the
# UniFFI crate in FFI/, linking it) and, with DART_OUT=<dir>, the Dart bindings into <dir>.
# CONFIG=debug|release (default: debug).
set -eu
cd "$(dirname "$0")/.."
CONFIG="${CONFIG:-debug}"
# Rust builds go through rcache (~/projects/rcache), the rustc cache shared across workspaces.
command -v rcache >/dev/null && export RUSTC_WRAPPER="${RUSTC_WRAPPER:-rcache}"

./scripts/turso.sh
swift build -c "$CONFIG" --product MailstreamKit
PROFILE=$([ "$CONFIG" = release ] && echo --release || true)
(cd FFI && MAILSTREAM_KIT_CONFIG="$CONFIG" cargo build $PROFILE -p mailstream)
echo "Built $(swift build -c "$CONFIG" --show-bin-path)/libMailstreamKit.dylib and FFI/target/$CONFIG/libmailstream.dylib"

if [ -n "${DART_OUT:-}" ]; then
  (cd FFI && cargo run -q -p mailstream-bindgen -- "target/$CONFIG/libmailstream.dylib" --out-dir "$DART_OUT")
fi
