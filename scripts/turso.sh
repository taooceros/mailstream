#!/bin/sh
# Builds the Turso database engine's SQLite C API (the turso_sqlite3 crate) into
# .turso/lib/libturso_sqlite3.a (turso_sqlite3.lib on Windows, run from Git Bash), which the app
# links. Runs cargo once per pinned version and patch; later runs return at once. Needs a Rust
# toolchain (rustup).
#
# turso-multiprocess.patch adds turso_enable_multiprocess_wal() to the C API, which 0.8.3 only
# offers through its SDK, so several processes (the app and FFI front ends) can share mail.db.
set -eu
cd "$(dirname "$0")/.."
# Rust builds go through rcache (~/projects/rcache), the rustc cache shared across workspaces.
command -v rcache >/dev/null && export RUSTC_WRAPPER="${RUSTC_WRAPPER:-rcache}"
TAG="v0.8.3"
PATCH="scripts/turso-multiprocess.patch"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) BUILT="turso_sqlite3.lib" ;;
  *) BUILT="libturso_sqlite3.a" ;;
esac
LIB=".turso/lib/$BUILT"
STAMP=".turso/lib/$TAG-$( (command -v shasum >/dev/null && shasum "$PATCH" || sha1sum "$PATCH") | cut -c1-12)"
[ -f "$LIB" ] && [ -f "$STAMP" ] && exit 0

SRC=".turso/src-$TAG"
rm -rf "$SRC"
mkdir -p .turso
curl -fsSL "https://github.com/tursodatabase/turso/archive/refs/tags/$TAG.tar.gz" | tar -xz -C .turso
mv ".turso/turso-${TAG#v}" "$SRC"
patch -s -d "$SRC" -p1 < "$PATCH"
# The header the app compiles against must be this version's.
cmp -s "$SRC/bindings/c/include/sqlite3.h" Sources/CTurso/sqlite3.h ||
  { echo "Sources/CTurso/sqlite3.h differs from Turso $TAG's; copy it over" >&2; exit 1; }
# turso_core's build.rs embeds the build time and git commit unless SOURCE_DATE_EPOCH is set; a
# fixed one makes every workspace's build identical, so rcache hits after the first.
(cd "$SRC" && MACOSX_DEPLOYMENT_TARGET=26.0 SOURCE_DATE_EPOCH=0 cargo build --release -p turso_sqlite3)
mkdir -p .turso/lib
rm -f .turso/lib/v*
cp "$SRC/target/release/$BUILT" "$LIB"
touch "$STAMP"
