#!/bin/zsh
# One-time setup for building Mailstream: Google Cloud project, Microsoft app registration, build
# secrets in the login keychain (service dev.mailstream.build) and the push relay. Safe to re-run:
# finished steps are skipped, so only what is still missing is asked for.
set -euo pipefail
cd "$(dirname "$0")/.."
SERVICE=dev.mailstream.build

have() { security find-generic-password -s $SERVICE -a "$1" >/dev/null 2>&1; }
value() { security find-generic-password -s $SERVICE -a "$1" -w; }
store() { security add-generic-password -U -s $SERVICE -a "$1" -w "$2"; }
step() { print -P "\n%B$1%b"; }
ask() {
  local answer=""
  while [[ -z $answer ]]; do read -r "answer?$1: "; done
  print -r -- "$answer"
}
ask_secret() {
  local answer=""
  while [[ -z $answer ]]; do read -rs "answer?$1: "; print >&2; done
  print -r -- "$answer"
}
wait_return() { read -r "?$1 Press Return when done. "; }

step "1/4 Google Cloud"
if [[ -z $(gcloud auth list --filter=status:ACTIVE --format='value(account)' 2>/dev/null) ]]; then
  gcloud auth login
fi
if have gmail-topic; then
  TOPIC=$(value gmail-topic)
  PROJECT=${${TOPIC#projects/}%%/*}
else
  PROJECT=$(ask "Google Cloud project id (an existing one, or a new id to create)")
  TOPIC="projects/$PROJECT/topics/gmail-push"
fi
if ! gcloud projects describe "$PROJECT" >/dev/null 2>&1; then
  gcloud projects create "$PROJECT"
fi
gcloud services enable gmail.googleapis.com pubsub.googleapis.com chat.googleapis.com \
  people.googleapis.com --project "$PROJECT"
if ! gcloud pubsub topics describe gmail-push --project "$PROJECT" >/dev/null 2>&1; then
  gcloud pubsub topics create gmail-push --project "$PROJECT" >/dev/null
fi
gcloud pubsub topics add-iam-policy-binding gmail-push --project "$PROJECT" --role roles/pubsub.publisher \
  --member serviceAccount:gmail-api-push@system.gserviceaccount.com >/dev/null
store gmail-topic "$TOPIC"
echo "Project $PROJECT: APIs enabled, topic gmail-push publishable by Gmail."

if ! have google-client-id || ! have google-client-secret; then
  echo "Google Chat needs an app identity: set a name, an avatar URL and a description (interactive features can stay off)."
  open "https://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat?project=$PROJECT"
  wait_return "Saved?"
  echo "Create the OAuth client: if asked, configure the consent screen first (External), then"
  echo "Create client ▸ Application type: Desktop app. Afterwards publish it (Audience ▸ Publish app)"
  echo "so refresh tokens do not expire after a week."
  open "https://console.cloud.google.com/auth/clients/create?project=$PROJECT"
  store google-client-id "$(ask "Client ID")"
  store google-client-secret "$(ask_secret "Client secret")"
fi

step "2/4 Microsoft"
if have microsoft-client-id && have microsoft-client-secret; then
  echo "App registration found."
else
  echo "New registration: any name; Supported account types: accounts in any organizational directory"
  echo "and personal Microsoft accounts; Redirect URI: Web, http://localhost:49152."
  echo "Then API permissions ▸ Add ▸ Microsoft Graph ▸ Delegated: User.Read, Mail.ReadWrite, Mail.Send,"
  echo "Contacts.Read; and Certificates & secrets ▸ New client secret."
  open "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade"
  store microsoft-client-id "$(ask "Application (client) ID")"
  store microsoft-client-secret "$(ask_secret "Client secret value")"
fi

step "3/4 Relay secrets"
for item in push-token graph-client-state pubsub-token; do
  have $item || store $item "$(openssl rand -hex 32)"
done
echo "push-token, graph-client-state and pubsub-token are set."

step "4/4 Push relay"
if have push-url; then
  echo "Relay at $(value push-url) (run Push/deploy.sh again after changing Push/worker.js)."
else
  npx --yes wrangler@4 whoami 2>/dev/null | grep -q "You are logged in" || npx --yes wrangler@4 login
  Push/deploy.sh
fi

print -P "\n%BDone.%b Build and launch with: CONFIG=debug scripts/run.sh (or scripts/bundle.sh for a release build)."
