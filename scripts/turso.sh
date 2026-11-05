#!/bin/sh
# Builds the Turso database engine's SQLite C API (the turso_sqlite3 crate) into
# .turso/lib/libturso_sqlite3.a, which the app links. Runs cargo once per pinned version; later
# runs return at once. Needs a Rust toolchain (rustup).
set -eu
cd "$(dirname "$0")/.."
TAG="v0.8.3"
LIB=".turso/lib/libturso_sqlite3.a"
STAMP=".turso/lib/$TAG"
[ -f "$LIB" ] && [ -f "$STAMP" ] && exit 0

SRC=".turso/src-$TAG"
if [ ! -d "$SRC" ]; then
  mkdir -p .turso
  curl -fsSL "https://github.com/tursodatabase/turso/archive/refs/tags/$TAG.tar.gz" | tar -xz -C .turso
  mv ".turso/turso-${TAG#v}" "$SRC"
fi
# The header the app compiles against must be this version's.
cmp -s "$SRC/bindings/c/include/sqlite3.h" Sources/CTurso/sqlite3.h ||
  { echo "Sources/CTurso/sqlite3.h differs from Turso $TAG's; copy it over" >&2; exit 1; }
(cd "$SRC" && MACOSX_DEPLOYMENT_TARGET=26.0 cargo build --release -p turso_sqlite3)
mkdir -p .turso/lib
rm -f .turso/lib/v*
cp "$SRC/target/release/libturso_sqlite3.a" "$LIB"
touch "$STAMP"
