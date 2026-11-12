#!/bin/sh
# Builds Mailstream with SwiftPM and assembles build/Mailstream.app.
# CONFIG=debug|release (default: release).
set -eu
cd "$(dirname "$0")/.."
CONFIG="${CONFIG:-release}"
VERSION="0.1.0"

./scripts/turso.sh
./scripts/capped swift build -c "$CONFIG"
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
</dict>
</plist>
PLIST

# OAuth clients and the push relay, read from the login keychain so they are never committed.
./scripts/app-config.sh "$APP/Contents/Info.plist"

# macOS pins the app's Keychain item (OAuth tokens) to its signer; sign.sh signs the app and
# every FFI front end alike so all of them read it silently.
./scripts/sign.sh "$APP"
echo "Built $APP"
