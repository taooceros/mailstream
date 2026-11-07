#!/bin/bash
# One pass over Linear (`watch`: one a minute until one has output): claims what an omp session
# should act on now and prints it as JSON lines, which that session hands to subagents, each in
# its own omp checkout (`isolated: true`).
#   {"kind": "issue", "id", "identifier", "resume", "unblockedBy"}: an issue to
#     start. Claimed when it is
#     - in Todo, created by a workspace member (not an app, a guest or a GitHub import: the repo
#       is public and the subagents run with full access), not claimed before and not yet
#       commented on by an app (omp); or
#     - claimed before, out of In Progress and no longer blocked: every issue blocking it (Linear
#       "blocked by") is In Review, Done, Canceled or a duplicate. Claimed once per set of
#       blockers, so blocking it again by another issue resumes it again; or
#     - labelled `omp`, in any state but started, done or canceled; the label is removed on
#       claim, so adding it again reruns the issue.
#     The first two skip issues with a blocker still open. resume is true for an issue claimed
#     before (the subagent carries on from the issue's comments); unblockedBy lists the
#     identifiers of its resolved blockers. Agents file their own issues in Backlog and move
#     issues to Todo to ask the user something, so neither is picked up unless it was claimed
#     before and gets unblocked.
#   {"kind": "comment", "id", "issue", "issueId", "state", "createdAt", "body"}: a comment a
#     workspace member (not an app, a guest or a GitHub sync) posted on any team issue in the last
#     7 days, each once. The caller routes it to the issue's subagent or starts one.
#   {"kind": "stale", "id", "identifier", "agent", "claimedAt"}: an In Progress issue
#     labelled `claimed` whose owner, the agent named in its latest start comment
#     (`[omp @ <checkout>] start (<agent id>): …`, posted at claimedAt), is not in RUNNING, and
#     whose claim is at least 10 minutes old. agent is null when no start comment names one. Each
#     start comment is reported once: whoever takes the issue over posts a new one. Needs the
#     agent ids running now, as the caller sees them, as a JSON array: in RUNNING_FILE (re-read
#     each pass; wins when the file exists) or RUNNING; without either nothing is reported stale.
#     Issues out of In Progress whose owner is not running lose `claimed`.
# Claims are kept in ~/.local/state/linear-poll/: `launched` (issue ids), `comments` (comment
# ids), `unblocked` (issue id and the blocker ids it was claimed with), `stale` (issue id and the
# start comment reported).
# DRY_RUN=1 prints without claiming (unclaim lines included). `linear-poll.sh seen comment|issue
# <id>` puts omp's 👀 on a comment or issue once the caller has handed it to a subagent, so the
# user sees it was picked up.
# `linear-poll.sh watch` runs a pass every INTERVAL seconds (default 60) and exits after the first
# pass that prints lines, with that pass's lines and status; passes with nothing to print (unclaims
# included) keep it looping silently. A failed pass (network, Linear error, token renewal) is
# logged to stderr and retried next interval; after MAX_FAILURES (default 10) failures in a row it
# exits 1. The caller starts it once as a background job with no deadline, waits for its result,
# handles the lines and starts it again, and rewrites RUNNING_FILE whenever its agents start or
# finish. With DRY_RUN=1 it exits after the first plan holding a line other than unclaim.
# The caller hands each issue to a new subagent spawned with `isolated: true`; a finished
# isolated agent can't be resumed, and the new one picks up from the issue's comments. A user
# comment on an issue whose agent is still running goes to that agent (`write agent://<id>`),
# otherwise to a new one. An agent that is running gets more work only on its own issue. A
# stale issue goes to a new agent, which posts its own start comment.
set -euo pipefail
TEAM_ID=${TEAM_ID:-9f8fe59f-3788-4542-8c18-4e5c5dd5ddc2} # Mailstream; the id survives key renames
LABEL=${LABEL:-omp}
CLAIMED=${CLAIMED:-claimed}
RENEW="$HOME/.pi/agent/extensions/linear/renew-omp-token.sh"
STATE=${STATE:-$HOME/.local/state/linear-poll}
LAUNCHED="$STATE/launched"
HANDLED="$STATE/comments"
UNBLOCKED="$STATE/unblocked"
STALE="$STATE/stale"
mkdir -p "$STATE" && touch "$LAUNCHED" "$HANDLED" "$UNBLOCKED" "$STALE"

log() { echo "$(date '+%F %T') $*" >&2; }

# gql QUERY VARIABLES_JSON -> .data; renews the omp app token once on an authentication error.
gql() {
  local body out renewed=
  body=$(jq -nc --arg q "$1" --argjson v "$2" '{query: $q, variables: $v}')
  while :; do
    out=$(curl -sS --max-time 60 https://api.linear.app/graphql \
      -H "Authorization: Bearer $(security find-generic-password -s linear-omp -a access_token -w)" \
      -H 'Content-Type: application/json' -d "$body") || { log "linear request failed"; return 1; }
    jq -e 'type == "object"' <<<"$out" >/dev/null 2>&1 || { log "linear: unreadable response: ${out:0:200}"; return 1; }
    if [ -z "$renewed" ] && jq -e 'any(.errors[]?; .extensions.code == "AUTHENTICATION_ERROR")' <<<"$out" >/dev/null; then
      log "token rejected; renewing"
      "$RENEW" >/dev/null || { log "token renewal failed"; return 1; }
      renewed=1
      continue
    fi
    if jq -e '.errors' <<<"$out" >/dev/null; then
      log "linear error: $(jq -c '[.errors[].message]' <<<"$out")" >&2
      return 1
    fi
    jq -c '.data' <<<"$out"
    return 0
  done
}

if [ "${1:-}" = seen ]; then
  case "${2:-}" in comment) field=commentId ;; issue) field=issueId ;; *) echo "usage: $0 seen comment|issue <id>" >&2; exit 2 ;; esac
  gql "mutation(\$id: String!) { reactionCreate(input: { $field: \$id, emoji: \"eyes\" }) { success } }" \
    "$(jq -nc --arg id "$3" '{id: $id}')" >/dev/null
  exit 0
fi

if [ "${1:-}" = watch ]; then
  failures=0
  while :; do
    out=$("$0") && status=0 || status=$?
    # Real passes print only what the caller handles; a dry run also shows unclaim lines.
    if [ -n "$(grep -v '^{"kind":"unclaim"' <<<"$out")" ]; then
      echo "$out"
      exit "$status"
    fi
    if [ "$status" = 0 ]; then
      failures=0
    else
      failures=$((failures + 1))
      if [ "$failures" -ge "${MAX_FAILURES:-10}" ]; then log "pass failed $failures times in a row; giving up"; exit 1; fi
      log "pass failed (exit $status); retrying in ${INTERVAL:-60}s"
    fi
    sleep "${INTERVAL:-60}"
  done
fi

RUNNING=${RUNNING:-null}
if [ -n "${RUNNING_FILE:-}" ] && [ -e "$RUNNING_FILE" ]; then
  RUNNING=$(jq -ce 'arrays' "$RUNNING_FILE") || { log "RUNNING_FILE $RUNNING_FILE is not a JSON array"; exit 1; }
fi

DATA=$(gql 'query($team: ID!, $label: String!, $claimed: String!) {
  queued: issues(first: 100, filter: {
    team: { id: { eq: $team } }, state: { type: { in: ["triage", "backlog", "unstarted"] } },
    or: [{ labels: { some: { name: { eq: $label } } } }, { state: { name: { eq: "Todo" } } },
      { hasBlockedByRelations: { eq: true } }] }) {
    nodes { id identifier number state { name } creator { app guest }
      labels { nodes { id name } } comments(first: 100) { nodes { user { app } } }
      inverseRelations(first: 50) { nodes { type issue { id identifier state { name type } } } } } }
  claimed: issues(first: 100, filter: {
    team: { id: { eq: $team } }, labels: { some: { name: { eq: $claimed } } } }) {
    nodes { id identifier state { name } labels { nodes { id name } }
      comments(first: 100) { nodes { id createdAt body user { app } } } } }
  comments(first: 100, filter: { createdAt: { gt: "-P7D" }, issue: { team: { id: { eq: $team } } } }) {
    nodes { id createdAt body user { app guest }
      issue { id identifier state { name } } } }
}' "$(jq -nc --arg team "$TEAM_ID" --arg label "$LABEL" --arg claimed "$CLAIMED" \
  '{team: $team, label: $label, claimed: $claimed}')")

# Issue lines oldest first, then stale and comment lines oldest first.
PLAN=$(jq -c --argjson running "$RUNNING" --arg label "$LABEL" --arg claimedLabel "$CLAIMED" \
  --rawfile launched "$LAUNCHED" --rawfile handled "$HANDLED" --rawfile unblocked "$UNBLOCKED" \
  --rawfile stale "$STALE" --arg now "$(date -u +%FT%TZ)" '
  def labelled: [.labels.nodes[] | select(.name == $label) | .id][0];
  def member: . != null and .app != true and .guest != true;
  def blockers: [.inverseRelations.nodes[] | select(.type == "blocks") | .issue];
  def resolved: .state.type == "completed" or .state.type == "canceled"
    or .state.type == "duplicate" or .state.name == "In Review";
  ($launched | split("\n") | map(split(" ")[0])) as $done
  | ($handled | split("\n")) as $seen
  | ($unblocked | split("\n")) as $freed
  | ($stale | split("\n")) as $reported
  | ($now | fromdateiso8601) as $t
  | [.queued.nodes[]
      | (.id as $id | $done | index($id) != null) as $claimed
      | (blockers | all(resolved)) as $free
      | (if (blockers | length) > 0 and $free
          then "\(.id) \([blockers[].id] | sort | join(","))" else null end) as $key
      | select(labelled != null or ($free and (
          (.state.name == "Todo" and (.creator | member) and ($claimed | not)
            and ([.comments.nodes[].user.app] | index(true) | not))
          or ($claimed and $key != null and ($key as $k | $freed | index($k) | not)))))
      | . + {resume: $claimed, unblockedBy: [blockers[] | select(resolved) | .identifier],
          unblockKey: $key}] as $todo
  | ($todo | sort_by(.number)[]
    | {kind: "issue", id, identifier, resume, unblockedBy, label: labelled, unblockKey}),
    (if $running == null then empty else .claimed.nodes[]
      | ([.comments.nodes[] | select(.user.app == true)
          | select(.body | test("^\\[omp @ [^\\]]*\\] start \\("))] | sort_by(.createdAt) | last)
          as $start
      | ([$start.body // "" | capture("^\\[omp @ [^\\]]*\\] start \\((?<a>[^)]+)\\)").a][0])
          as $agent
      | select($running | index($agent) | not)
      | [.labels.nodes[] | select(.name == $claimedLabel) | .id][0] as $labelId
      | if .state.name != "In Progress" then {kind: "unclaim", id, label: $labelId}
        else "\(.id) \($start.id // "none")" as $key
          | select(($reported | index($key) | not) and ($start == null
              or $t - ($start.createdAt | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601) >= 600))
          | {kind: "stale", id, identifier, agent: $agent,
             claimedAt: $start.createdAt, staleKey: $key} end end),
    (.comments.nodes | sort_by(.createdAt)[]
      | select((.user | member) and (.id as $id | $seen | index($id) | not))
      | {kind: "comment", id, issue: .issue.identifier, issueId: .issue.id,
         state: .issue.state.name, createdAt, body})' <<<"$DATA")

[ -n "$PLAN" ] || exit 0
if [ -n "${DRY_RUN:-}" ]; then echo "$PLAN"; exit 0; fi
while read -r line; do
  id=$(jq -r .id <<<"$line")
  case "$(jq -r .kind <<<"$line")" in
    comment) echo "$id" >>"$HANDLED"; echo "$line"; continue ;;
    stale) jq -r .staleKey <<<"$line" >>"$STALE"; jq -c 'del(.staleKey)' <<<"$line"; continue ;;
    unclaim)
      gql 'mutation($id: String!, $label: String!) { issueRemoveLabel(id: $id, labelId: $label) { success } }' \
        "$(jq -c '{id, label}' <<<"$line")" >/dev/null
      continue ;;
  esac
  label=$(jq -r '.label // empty' <<<"$line")
  key=$(jq -r '.unblockKey // empty' <<<"$line")
  [ "$(jq -r .resume <<<"$line")" = true ] || echo "$id $(jq -r .identifier <<<"$line")" >>"$LAUNCHED"
  [ -z "$key" ] || echo "$key" >>"$UNBLOCKED"
  [ -z "$label" ] ||
    gql 'mutation($id: String!, $label: String!) { issueRemoveLabel(id: $id, labelId: $label) { success } }' \
      "$(jq -nc --arg id "$id" --arg label "$label" '{id: $id, label: $label}')" >/dev/null
  jq -c 'del(.label, .unblockKey)' <<<"$line"
done <<<"$PLAN"
