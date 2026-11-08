#!/bin/sh
# Signs Mailstream and every front end that hosts its engine (a Flutter app, a Rust tool, the
# FFI dylib) with one signature: the Apple Development certificate and the code identifier
# dev.mailstream.Mailstream. macOS pins the Keychain item holding the sign-ins to that pair, so
# each of them reads it without asking. Without a certificate it signs ad hoc, and macOS asks
# once per build.
# Usage: scripts/sign.sh <app bundle, binary or dylib>...
set -eu
IDENTIFIER="dev.mailstream.Mailstream"
IDENTITY=$(security find-identity -v -p codesigning |
  sed -n 's/^ *[0-9]*) \([0-9A-F]*\) "Apple Development: .*"$/\1/p' | sed -n 1p)
if [ -z "$IDENTITY" ]; then
  echo "note: signed ad hoc; macOS asks once after each rebuild to let it read saved sign-ins" >&2
  IDENTITY="-"
fi
for target in "$@"; do
  # Sparkle's installer and relaunch helpers must keep their own identifiers, but share
  # the host's signing identity. Sign inside-out; never use --deep for signing.
  sparkle="$target/Contents/Frameworks/Sparkle.framework"
  if [ -d "$sparkle" ]; then
    for helper in "$sparkle/Versions/Current/XPCServices/Downloader.xpc" \
      "$sparkle/Versions/Current/XPCServices/Installer.xpc" \
      "$sparkle/Versions/Current/Autoupdate" \
      "$sparkle/Versions/Current/Updater.app" "$sparkle"; do
      codesign --force --sign "$IDENTITY" --preserve-metadata=identifier,entitlements "$helper" >/dev/null
    done
  fi
  # Mailstream Flutter carries the mailstream command line beside its executable.
  cli="$target/Contents/MacOS/mailstream"
  if [ -f "$cli" ]; then
    codesign --force --sign "$IDENTITY" --identifier dev.mailstream.cli "$cli" >/dev/null
  fi
  codesign --force --sign "$IDENTITY" --identifier "$IDENTIFIER" "$target" >/dev/null
done
