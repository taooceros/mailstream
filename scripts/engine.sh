#!/bin/sh
# Builds the embedded himalaya engine (Engine/) into Engine/target/release/libmailstream_engine.a,
# which SwiftPM links. Always release: a debug engine is slow at TLS and MIME parsing.
set -eu
# From Engine/ itself: cargo reads Engine/.cargo/config.toml (the deployment target) by working directory.
cd "$(dirname "$0")/../Engine"
cargo build --release --locked
