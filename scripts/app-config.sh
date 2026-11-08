#!/bin/sh
# Adds the OAuth clients and the push relay settings from the login keychain (service
# dev.mailstream.build) to an app's Info.plist, so they are never committed.
# Usage: scripts/app-config.sh <Info.plist>
# A missing item leaves its key out (the feature it enables stays off). Values are never printed:
# they reach PlistBuddy through a private temporary plist, not its command line.
set -eu
plist="$1"
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
      /usr/libexec/PlistBuddy -c "Delete :$key" "$plist" >/dev/null 2>&1 || true
    else
      echo "warning: no \"$item\" item in Keychain service $SERVICE; $key left out" >&2
    fi
  done
  echo '</dict></plist>'
} > "$fragment"
/usr/libexec/PlistBuddy -c "Merge $fragment" "$plist" >/dev/null
