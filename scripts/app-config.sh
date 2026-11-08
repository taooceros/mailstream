#!/bin/sh
# Adds the OAuth clients and the push relay settings from the login keychain (service
# dev.mailstream.build) to an app's Info.plist, so they are never committed. Given a .json path
# instead, it writes them as that file (private to the user): Mailstream Flutter's engine reads
# them from config.json in ~/Library/Application Support/Mailstream, since the signed updates CI
# publishes are public and carry none of them (Engine/src/bundle.rs).
# Usage: scripts/app-config.sh <Info.plist | config.json>
# A missing item leaves its key out (the feature it enables stays off). Values are never printed:
# they reach PlistBuddy/plutil through a private temporary plist, not their command line.
set -eu
target="$1"
case "$target" in *.json) json=1 ;; *) json= ;; esac
SERVICE="dev.mailstream.build"

umask 077
fragment=$(mktemp -t mailstream-app-config)
trap 'rm -f "$fragment"' EXIT
{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
  echo '<plist version="1.0"><dict>'
  for pair in MSGoogleClientID=google-client-id MSGoogleClientSecret=google-client-secret \
              MSMicrosoftClientID=microsoft-client-id MSMicrosoftClientSecret=microsoft-client-secret \
              MSPushURL=push-url MSPushToken=push-token MSGraphClientState=graph-client-state \
              MSGmailTopic=gmail-topic; do
    key="${pair%%=*}"
    item="${pair#*=}"
    if value=$(security find-generic-password -s "$SERVICE" -a "$item" -w 2>/dev/null); then
      value=$(printf '%s' "$value" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')
      printf '  <key>%s</key><string>%s</string>\n' "$key" "$value"
      [ -n "$json" ] || /usr/libexec/PlistBuddy -c "Delete :$key" "$target" >/dev/null 2>&1 || true
    else
      echo "warning: no \"$item\" item in Keychain service $SERVICE; $key left out" >&2
    fi
  done
  echo '</dict></plist>'
} > "$fragment"
if [ -n "$json" ]; then
  mkdir -p "$(dirname "$target")"
  plutil -convert json -o "$target.new" "$fragment"
  mv -f "$target.new" "$target"
else
  /usr/libexec/PlistBuddy -c "Merge $fragment" "$target" >/dev/null
fi
