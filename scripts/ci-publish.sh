#!/bin/sh
# CI only (publish-update.yml, publish-gpui-update.yml): signs an already built release app with
# the shared certificate from a throwaway keychain (release-sign.sh, revision GITHUB_SHA) and
# prepares its authenticated Sparkle update in build/update (prepare-update.py) with the pinned
# Sparkle tools (sparkle.sh). The keychain and the key files are deleted when it ends.
# Needs CERTIFICATE (base64 .p12), CERTIFICATE_PASSWORD, UPDATE_KEY and GITHUB_SHA.
# Usage: scripts/ci-publish.sh <app>
set -eu
cd "$(dirname "$0")/.."
app=$1
umask 077
work=$(mktemp -d)
keychain="$work/signing.keychain-db"
password=$(openssl rand -hex 24)
cleanup() { security delete-keychain "$keychain" || true; rm -rf "$work"; }
trap cleanup EXIT
printf '%s' "$CERTIFICATE" | base64 --decode > "$work/signing.p12"
printf '%s' "$UPDATE_KEY" > "$work/update-key"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$work/signing.p12" -P "$CERTIFICATE_PASSWORD" -T /usr/bin/codesign -t cert -f pkcs12 -k "$keychain"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain"
security list-keychains -d user -s "$keychain"
./scripts/release-sign.sh "$app" "$GITHUB_SHA"
sparkle=$(./scripts/sparkle.sh)
python3 scripts/prepare-update.py "$app" build/update --sparkle-bin "$sparkle/bin" --key-file "$work/update-key"
