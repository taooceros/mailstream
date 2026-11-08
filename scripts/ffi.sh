#!/bin/sh
# Builds the engine for FFI front ends: the UniFFI crate in FFI/ with the Rust engine in Engine/
# (libmailstream.dylib on macOS, mailstream.dll on Windows, run there from Git Bash), the
# mailstream command line (Engine/target/<config>/mailstream, which the Flutter app bundles on
# macOS) and, with DART_OUT=<dir>, the Dart bindings into <dir>.
# CONFIG=debug|release (default: debug).
set -eu
cd "$(dirname "$0")/.."
CONFIG="${CONFIG:-debug}"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) LIBRARY=mailstream.dll ;;
  *) LIBRARY=libmailstream.dylib ;;
esac

PROFILE=$([ "$CONFIG" = release ] && echo --release || true)
(cd FFI && cargo build $PROFILE -p mailstream)
(cd Engine && cargo build $PROFILE --bin mailstream)
echo "Built FFI/target/$CONFIG/$LIBRARY and Engine/target/$CONFIG/mailstream"

if [ -n "${DART_OUT:-}" ]; then
  (cd FFI && cargo run -q -p mailstream-bindgen -- "target/$CONFIG/$LIBRARY" --out-dir "$DART_OUT")
fi
