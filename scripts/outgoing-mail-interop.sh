#!/bin/sh
# Holds the Rust engine's outgoing mail to the Swift engine's (MS-114): for the inputs of
# Tests/Fixtures/outgoing-mail-interop.json the Swift engine writes its rendered MIME messages,
# reply pieces (recipients, subject, quote, attribution, HTML) and ranked contacts to a throwaway
# file, and the Rust engine's test compares its own with them. Nothing is sent and no data of the
# person's is read. Needs the Turso library (scripts/turso.sh) for the Swift side.
set -eu
cd "$(dirname "$0")/.."
command -v rcache >/dev/null && export RUSTC_WRAPPER="${RUSTC_WRAPPER:-rcache}"
./scripts/turso.sh

tmp=${TMPDIR:-/tmp}
work=$(mktemp -d "${tmp%/}/mailstream-outgoing.XXXXXX")
trap 'rm -rf "$work"' EXIT

swift build --build-tests >/dev/null
if ! out=$(MAILSTREAM_OUTGOING_INTEROP="$work/swift.json" swift test --skip-build --filter OutgoingMailInteropTests 2>&1); then
    printf '%s\n' "$out" | tail -40; echo "FAILED: Swift side"; exit 1
fi
test -s "$work/swift.json" || { echo "FAILED: the Swift side wrote nothing"; exit 1; }
cd Engine
MAILSTREAM_OUTGOING_INTEROP="$work/swift.json" \
    cargo test -q --lib mail::interop_tests::renders_plans_and_ranks_what_the_swift_engine_does -- --exact
