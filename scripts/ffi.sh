#!/bin/sh
# Builds the engine for FFI front ends: libmailstream.dylib (the UniFFI crate in FFI/, with the
# Rust engine in Engine/), the mailstream command line (Engine/target/<config>/mailstream, which
# the Flutter app bundles) and, with DART_OUT=<dir>, the Dart bindings into <dir>.
# CONFIG=debug|release (default: debug).
set -eu
cd "$(dirname "$0")/.."
CONFIG="${CONFIG:-debug}"
# Rust builds go through rcache (~/projects/rcache), the rustc cache shared across workspaces.
command -v rcache >/dev/null && export RUSTC_WRAPPER="${RUSTC_WRAPPER:-rcache}"

PROFILE=$([ "$CONFIG" = release ] && echo --release || true)
(cd FFI && cargo build $PROFILE -p mailstream)
(cd Engine && cargo build $PROFILE --bin mailstream)
echo "Built FFI/target/$CONFIG/libmailstream.dylib and Engine/target/$CONFIG/mailstream"

if [ -n "${DART_OUT:-}" ]; then
  (cd FFI && cargo run -q -p mailstream-bindgen -- "target/$CONFIG/libmailstream.dylib" --out-dir "$DART_OUT")
fi
