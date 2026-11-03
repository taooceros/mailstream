#!/bin/zsh
# Deploys the push relay and points Gmail's Pub/Sub topic at it. Needs `npx wrangler login` and
# `gcloud auth login` (an account that owns the Google Cloud project). Secrets come from the
# login keychain, service dev.mailstream.build (see README); the relay's URL is stored there as
# push-url for scripts/bundle.sh.
set -euo pipefail
cd "$(dirname "$0")"
SERVICE=dev.mailstream.build
secret() { security find-generic-password -s $SERVICE -a "$1" -w; }

OUTPUT=$(npx --yes wrangler@4 deploy 2>&1) || { print -r -- "$OUTPUT"; exit 1; }
URL=$(print -r -- "$OUTPUT" | grep -oE 'https://[a-z0-9.-]+\.workers\.dev' | head -1)
[[ -n $URL ]] || { print -r -- "$OUTPUT"; echo "No workers.dev URL in wrangler's output." >&2; exit 1; }
for name in APP_TOKEN:push-token GRAPH_CLIENT_STATE:graph-client-state PUBSUB_TOKEN:pubsub-token; do
  secret "${name#*:}" | npx --yes wrangler@4 secret put "${name%%:*}" >/dev/null
done
security add-generic-password -U -s $SERVICE -a push-url -w "$URL"

TOPIC=$(secret gmail-topic)
PROJECT=${${TOPIC#projects/}%%/*}
ENDPOINT="$URL/gmail?token=$(secret pubsub-token)"
if gcloud pubsub subscriptions describe mailstream-push --project "$PROJECT" >/dev/null 2>&1; then
  gcloud pubsub subscriptions modify-push-config mailstream-push --project "$PROJECT" --push-endpoint "$ENDPOINT" >/dev/null
else
  gcloud pubsub subscriptions create mailstream-push --project "$PROJECT" --topic "$TOPIC" \
    --push-endpoint "$ENDPOINT" --ack-deadline 10 --message-retention-duration 10m >/dev/null
fi
echo "Relay at $URL; Gmail pushes reach it through $PROJECT/mailstream-push."
