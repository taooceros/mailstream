#!/bin/sh
# Stamps a release bundle with the commit it was built from and signs it the one way every
# release takes, for scripts/install.sh and the signed updates CI publishes (ci-publish.sh;
# MS-239): Info.plist's MSInstalledRevision, then sign.sh, then a strict signature check and the
# shared team K7HG6KZNNP (sign.sh falls back to ad hoc without the certificate; a release must
# not).
# Usage: scripts/release-sign.sh <app> <revision>
set -eu
cd "$(dirname "$0")/.."
app=$1
revision=$2
plist="$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :MSInstalledRevision $revision" "$plist" 2>/dev/null ||
  /usr/libexec/PlistBuddy -c "Add :MSInstalledRevision string $revision" "$plist"
./scripts/sign.sh "$app"
codesign --verify --deep --strict "$app"
codesign -d -v "$app" 2>&1 | /usr/bin/grep -q '^TeamIdentifier=K7HG6KZNNP$' || {
  echo "release-sign: $app is not signed by the shared team K7HG6KZNNP" >&2
  exit 1
}
