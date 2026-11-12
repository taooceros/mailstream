#!/bin/sh
# Bundles GPUI/target/release/mailstream-gpui (build it first: cd GPUI && cargo build --release)
# as an unsigned "Mailstream GPUI.app" for scripts/install.sh, the nightly and the signed
# updates CI publishes (MS-219): the Apple Intelligence tagger, Sparkle in Frameworks
# (the pinned version scripts/sparkle.sh fetches once into build/) and an Info.plist naming the
# GPUI update feed, the approved public update key and the channel. No OAuth clients or push
# settings: the engine reads those from config.json in the support directory
# (scripts/app-config.sh), since the published updates are public.
# Usage: scripts/gpui-bundle.sh [APP]    (default build/Mailstream GPUI.app)
# BUILD_NUMBER (default 1) is CFBundleVersion: Sparkle installs a published build only when its
# number is larger, so local installs keep 1 and CI takes scripts/publish-update.py gpui
# next-version. UPDATE_PUBLIC_KEY and UPDATE_CHANNEL override the key and the prerelease channel.
set -eu
cd "$(dirname "$0")/.."
app=${1:-build/Mailstream GPUI.app}
build=${BUILD_NUMBER:-1}
# The approved Ed25519 public key, the same as Mailstream Flutter's (Flutter/macos/Runner/Configs/AppInfo.xcconfig).
key=${UPDATE_PUBLIC_KEY:-eIwn7DyQN0GOIdjlzo6AKRm5GVCOorozSzfbC+4XG4s=}
channel=${UPDATE_CHANNEL:-prerelease}
sparkle=$(./scripts/sparkle.sh)

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks"
cp GPUI/target/release/mailstream-gpui "$app/Contents/MacOS/"
python3 scripts/apple-tagger.py "$app/Contents/MacOS/mailstream-apple-tagger"
ditto "$sparkle/Sparkle.framework" "$app/Contents/Frameworks/Sparkle.framework"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Mailstream GPUI</string>
  <key>CFBundleExecutable</key><string>mailstream-gpui</string>
  <key>CFBundleIdentifier</key><string>dev.mailstream.MailstreamGPUI</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>$build</string>
  <key>LSMinimumSystemVersion</key><string>26.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>MSUpdateChannel</key><string>$channel</string>
  <key>SUFeedURL</key><string>https://github.com/taooceros/mailstream/releases/download/updates/appcast-gpui.xml</string>
  <key>SUPublicEDKey</key><string>$key</string>
  <key>SUVerifyUpdateBeforeExtraction</key><true/>
  <key>SURequireSignedFeed</key><true/>
  <key>SUSignedFeedFailureExpirationInterval</key><integer>0</integer>
  <key>SUEnableAutomaticChecks</key><true/>
  <key>SUAutomaticallyUpdate</key><true/>
  <key>SUScheduledCheckInterval</key><integer>3600</integer>
  <key>SUEnableSystemProfiling</key><false/>
</dict></plist>
PLIST
