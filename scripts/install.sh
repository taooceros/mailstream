#!/bin/sh
# Install affected, already-installed front ends from a clean private checkout at main.
# Usage: scripts/install.sh native|gpui [native|gpui ...]
# flutter is refused: the Flutter front end is paused (MS-300); /Applications/Mailstream Flutter.app
# stays as last installed.
# Then check each installed app: revision, signature, launch (AGENTS.md, Release delivery).
# On Linux it runs scripts/install-linux.py (gpui into ~/.local/opt; MS-224).
set -eu
cd "$(dirname "$0")/.."
[ "$(uname)" = Linux ] && exec python3 scripts/install-linux.py "$@"

fail() { echo "install: $*" >&2; exit 1; }
if [ "$#" -eq 0 ]; then
  fail "choose affected installed front ends: native, gpui (see AGENTS.md)"
fi
for frontend in "$@"; do
  case "$frontend" in
    native) name=Mailstream ;;
    flutter) fail "Flutter is paused (MS-300): no Flutter installs; Mailstream Flutter.app stays as it is" ;;
    gpui) name='Mailstream GPUI' ;;
    *) fail "unknown front end: $frontend" ;;
  esac
  [ -d "/Applications/$name.app" ] || fail "/Applications/$name.app is not installed"
done
revision=$(jj log --no-graph -r main -T commit_id)
[ -n "$(jj log --no-graph -r 'main & (@ | @-)' -T commit_id)" ] ||
  fail "create a clean private checkout at current main (scripts/checkout.py NAME); do not deploy an area workspace"
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
    gpui)
      name='Mailstream GPUI'
      executable=mailstream-gpui
      ./scripts/capped cargo build --release -p mailstream-gpui -p mailstream-engine --bin mailstream
      ./scripts/gpui-bundle.sh
      app="build/$name.app"
      ;;
  esac
  # An interrupted build can exit 0 and leave a bundle without its executable.
  [ -x "$app/Contents/MacOS/$executable" ] || fail "$app has no executable $executable; build failed"
  # The engine reads OAuth clients and push settings from the host's Info.plist; bundle.sh
  # already added them to Mailstream.app. Mailstream GPUI reads them from the support directory's
  # config.json instead, as the public signed updates it installs from CI must.
  case "$frontend" in
    gpui) ./scripts/app-config.sh "$HOME/Library/Application Support/Mailstream/config.json" ;;
  esac
  # MSInstalledRevision, signature and the shared team, as CI's signed updates get them.
  ./scripts/release-sign.sh "$app" "$revision"
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
  # The mailstream command line ships in Mailstream GPUI; a ~/.local/bin/mailstream link into
  # an installed Mailstream app moves to it.
  link="$HOME/.local/bin/mailstream"
  if [ "$frontend" = gpui ] && [ -L "$link" ] &&
    case "$(readlink "$link")" in /Applications/Mailstream*.app/*) true ;; *) false ;; esac; then
    ln -sfn "$destination/Contents/MacOS/mailstream" "$link"
    echo "$link -> $destination/Contents/MacOS/mailstream"
  fi
  echo "Check this /Applications copy: MSInstalledRevision, codesign, launch (AGENTS.md)."
  echo "Previous bundle retained at $previous until those checks pass."
done
