#!/bin/sh
# Install affected, already-installed front ends from a clean private checkout at main.
# Usage: scripts/install.sh native|flutter|gpui [native|flutter|gpui ...]
# Launch and inspect each installed app with computer helpers before completing the issue.
set -eu
cd "$(dirname "$0")/.."

fail() { echo "install: $*" >&2; exit 1; }
if [ "$#" -eq 0 ]; then
  fail "choose affected installed front ends: native, flutter, gpui (see AGENTS.md)"
fi
for frontend in "$@"; do
  case "$frontend" in
    native) name=Mailstream ;;
    flutter) name='Mailstream Flutter' ;;
    gpui) name='Mailstream GPUI' ;;
    *) fail "unknown front end: $frontend" ;;
  esac
  [ -d "/Applications/$name.app" ] || fail "/Applications/$name.app is not installed"
done
revision=$(jj log --no-graph -r main -T commit_id)
[ -n "$(jj log --no-graph -r 'main & (@ | @-)' -T commit_id)" ] ||
  fail "create a clean private checkout at current main (ovnfs new --rev main); do not deploy an area workspace"
[ -z "$(jj diff --from main --to @ --name-only)" ] || fail "checkout differs from main"
# sign.sh may fall back to ad hoc for development/CI, but installed releases must not.
security find-identity -v -p codesigning | /usr/bin/grep -q '"Apple Development: ' ||
  fail "Apple Development signing identity is required"

lock=/tmp/mailstream-release-install.lock
mkdir "$lock" 2>/dev/null || fail "another release deployment holds $lock; coordinate before retrying"
trap 'rmdir "$lock"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

for frontend in "$@"; do
  case "$frontend" in
    native)
      name=Mailstream
      executable=Mailstream
      CONFIG=release ./scripts/bundle.sh
      app="build/$name.app"
      ;;
    flutter)
      name='Mailstream Flutter'
      executable='Mailstream Flutter'
      CONFIG=release DART_OUT="$PWD/Flutter/packages/mailstream/lib/src" ./scripts/ffi.sh
      (cd Flutter && flutter pub get && flutter build macos --release)
      app="Flutter/build/macos/Build/Products/Release/$name.app"
      ;;
    gpui)
      name='Mailstream GPUI'
      executable=mailstream-gpui
      (cd GPUI && cargo build --release)
      app="build/$name.app"
      mkdir -p "$app/Contents/MacOS"
      cp GPUI/target/release/mailstream-gpui "$app/Contents/MacOS/"
      cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Mailstream GPUI</string>
  <key>CFBundleExecutable</key><string>mailstream-gpui</string>
  <key>CFBundleIdentifier</key><string>dev.mailstream.MailstreamGPUI</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>26.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
      ;;
  esac
  /usr/libexec/PlistBuddy -c "Add :MSInstalledRevision string $revision" "$app/Contents/Info.plist"
  ./scripts/sign.sh "$app"
  codesign --verify --deep --strict "$app"
  codesign -d -v "$app" 2>&1 | /usr/bin/grep -q '^TeamIdentifier=K7HG6KZNNP$' ||
    fail "release must use the shared team K7HG6KZNNP"
  [ "$(jj log --no-graph -r main -T commit_id)" = "$revision" ] ||
    fail "main advanced during build; rebuild at current main before installing"

  destination="/Applications/$name.app"
  staging="/Applications/.$name-install-$$.app"
  previous="$PWD/build/previous-$name.app"
  [ ! -e "$staging" ] && [ ! -e "$previous" ] || fail "staging/backup path already exists"
  ditto "$app" "$staging"
  codesign --verify --deep --strict "$staging"
  # Match only this installed executable, never Debug, /tmp or another agent's build.
  pids=$(pgrep -f "^/Applications/$name[.]app/Contents/MacOS/$executable( |$)" || true)
  if [ -n "$pids" ]; then
    kill -TERM $pids
    for pid in $pids; do
      remaining=15
      while kill -0 "$pid" 2>/dev/null; do
        [ "$remaining" -gt 0 ] || fail "installed process $pid has not exited; not forcing termination"
        sleep 1
        remaining=$((remaining - 1))
      done
    done
  fi
  mkdir -p build
  mv "$destination" "$previous"
  if ! mv "$staging" "$destination"; then
    mv "$previous" "$destination"
    fail "installation failed; previous bundle restored"
  fi
  codesign --verify --deep --strict "$destination"
  echo "Installed $destination at $revision (release, shared signature)."
  # The mailstream command line ships in Mailstream Flutter; a ~/.local/bin/mailstream link into
  # an installed Mailstream app moves to it.
  link="$HOME/.local/bin/mailstream"
  if [ "$frontend" = flutter ] && [ -L "$link" ] &&
    case "$(readlink "$link")" in /Applications/Mailstream*.app/*) true ;; *) false ;; esac; then
    ln -sfn "$destination/Contents/MacOS/mailstream" "$link"
    echo "$link -> $destination/Contents/MacOS/mailstream"
  fi
  echo "Launch this /Applications copy with computer helpers and verify its UI/engine."
  echo "Previous bundle retained at $previous until smoke verification succeeds."
done
