#!/bin/sh
# Checks that the Swift and the Rust engine read each other's mail.db (MS-113): each writes
# Tests/Fixtures/mail-db-interop.json into a throwaway home through its own cache layer, then
# reads the home the other one wrote. Only throwaway homes are touched. Needs the Turso library
# (scripts/turso.sh) for the Swift side.
set -eu
cd "$(dirname "$0")/.."
# The runs below move HOME to throwaway folders; the toolchain stays where it is.
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
./scripts/turso.sh

tmp=${TMPDIR:-/tmp}
work=$(mktemp -d "${tmp%/}/mailstream-interop.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/swift" "$work/rust"

./scripts/capped swift build --build-tests >/dev/null
(cd Engine && cargo test --no-run -q 2>/dev/null)

run() { # label, command...: prints the command's verdict line; stops with its output if it failed
    label=$1; shift
    if ! out=$("$@" 2>&1); then
        printf '%s\n' "$out" | tail -40; echo "FAILED: $label"; exit 1
    fi
    printf '%s: %s\n' "$label" "$(printf '%s\n' "$out" | grep -E 'readsAndWritesWhatTheRustEngineDoes\(\) passed|^test result' | tail -1)"
}
swift_run() { # mode home
    HOME="$2" CFFIXED_USER_HOME="$2" MAILSTREAM_INTEROP="$1" swift test --skip-build --filter MailDatabaseInteropTests
}
rust_run() { # mode home
    cd Engine && HOME="$2" CFFIXED_USER_HOME="$2" MAILSTREAM_INTEROP="$1" \
        cargo test -q --lib cache::tests::reads_and_writes_what_the_swift_engine_does -- --exact
}

run "Swift writes" swift_run write "$work/swift"
run "Rust writes" rust_run write "$work/rust"
run "Swift reads Rust's mail.db" swift_run read "$work/rust"
run "Rust reads Swift's mail.db" rust_run read "$work/swift"
