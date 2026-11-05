#!/bin/sh
# Builds Mailstream with SwiftPM and assembles build/Mailstream.app.
# CONFIG=debug|release (default: release).
set -eu
cd "$(dirname "$0")/.."
CONFIG="${CONFIG:-release}"
VERSION="0.1.0"
SERVICE="dev.mailstream.build"

./scripts/turso.sh
swift build -c "$CONFIG"
BIN="$(swift build -c "$CONFIG" --show-bin-path)/Mailstream"

APP="build/Mailstream.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Mailstream"

if [ ! -f build/AppIcon.icns ] || [ Resources/AppIcon.png -nt build/AppIcon.icns ]; then
  ICONSET="build/AppIcon.iconset"
  rm -rf "$ICONSET" && mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z $size $size Resources/AppIcon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z $double $double Resources/AppIcon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o build/AppIcon.icns
fi
cp build/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

# OAuth clients and the push relay, read from the login keychain so they are never committed.
# A missing item leaves its key out (the feature it enables stays off).
CONFIG_KEYS=""
for pair in MSGoogleClientID=google-client-id MSGoogleClientSecret=google-client-secret \
            MSMicrosoftClientID=microsoft-client-id MSMicrosoftClientSecret=microsoft-client-secret \
            MSPushURL=push-url MSPushToken=push-token MSGraphClientState=graph-client-state \
            MSGmailTopic=gmail-topic; do
  key="${pair%%=*}"
  item="${pair#*=}"
  if value=$(security find-generic-password -s "$SERVICE" -a "$item" -w 2>/dev/null); then
    value=$(printf '%s' "$value" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')
    CONFIG_KEYS="$CONFIG_KEYS  <key>$key</key><string>$value</string>
"
  else
    echo "warning: no \"$item\" item in Keychain service $SERVICE; $key left out" >&2
  fi
done

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>Mailstream</string>
  <key>CFBundleIdentifier</key><string>dev.mailstream.Mailstream</string>
  <key>CFBundleName</key><string>Mailstream</string>
  <key>CFBundleDisplayName</key><string>Mailstream</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>26.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticTermination</key><false/>
$CONFIG_KEYS</dict>
</plist>
PLIST

# macOS pins the app's Keychain item (OAuth tokens) to its signer: with an Apple Development
# certificate (free with any Apple ID in Xcode) that is the team, so rebuilds read it silently;
# signed ad hoc, each new build is a new signer and macOS asks once to allow it.
IDENTITY=$(security find-identity -v -p codesigning |
  sed -n 's/^ *[0-9]*) \([0-9A-F]*\) "Apple Development: .*"$/\1/p' | sed -n 1p)
if [ -n "$IDENTITY" ]; then
  codesign --force --sign "$IDENTITY" "$APP" >/dev/null
else
  echo "note: signed ad hoc; macOS asks once after each rebuild to let it read saved sign-ins" >&2
  codesign --force --sign - "$APP" >/dev/null
fi
echo "Built $APP"
