#!/bin/bash
# One pass over Linear: claims the issues that should be worked now and prints one JSON line per
# issue, {"id", "identifier", "dir"}, dir being the jj workspace to work it in (../mailstream-<area>
# for its Area label, else this repo). An omp session polls this and hands each line to a subagent.
# An issue is claimed when it is
#   - in Todo, created by a workspace member (not an app, a guest or a GitHub import: the repo is
#     public and the subagents run with full access), not claimed before and not yet commented on
#     by an app (omp); or
#   - labelled `omp`, in any state but started, done or canceled; the label is removed on claim,
#     so adding it again reruns the issue (e.g. after answering an agent's question).
# Agents file their own issues in Backlog and move issues to Todo to ask the user something, so
# neither is picked up. Issues share their area's workspace: a workspace with an In Progress issue,
# one listed in BUSY (JSON array of dirs, e.g. the caller's running subagents) or one claimed this
# pass waits. Claimed issue ids are kept in ~/.local/state/linear-poll/launched. DRY_RUN=1 prints
# without claiming.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TEAM_ID=${TEAM_ID:-9f8fe59f-3788-4542-8c18-4e5c5dd5ddc2} # Mailstream; the id survives key renames
LABEL=${LABEL:-omp}
RENEW="$HOME/.pi/agent/extensions/linear/renew-omp-token.sh"
LAUNCHED=${LAUNCHED:-$HOME/.local/state/linear-poll/launched}
mkdir -p "$(dirname "$LAUNCHED")" && touch "$LAUNCHED"

log() { echo "$(date '+%F %T') $*" >&2; }

# gql QUERY VARIABLES_JSON -> .data; renews the omp app token once on an authentication error.
gql() {
  local body out renewed=
  body=$(jq -nc --arg q "$1" --argjson v "$2" '{query: $q, variables: $v}')
  while :; do
    out=$(curl -sS https://api.linear.app/graphql \
      -H "Authorization: Bearer $(security find-generic-password -s linear-omp -a access_token -w)" \
      -H 'Content-Type: application/json' -d "$body")
    if [ -z "$renewed" ] && jq -e 'any(.errors[]?; .extensions.code == "AUTHENTICATION_ERROR")' <<<"$out" >/dev/null; then
      log "token rejected; renewing"
      "$RENEW" >/dev/null
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

# Areas with their own workspace (../mailstream-<area>); every other issue runs in this repo.
WORKSPACES=$(for d in "$ROOT"/../mailstream-*/; do [ -d "$d.jj" ] && basename "$d"; done |
  sed 's/^mailstream-//' | jq -Rnc '[inputs]')
BUSY=${BUSY:-[]}

DATA=$(gql 'query($team: ID!, $label: String!) {
  queued: issues(first: 100, filter: {
    team: { id: { eq: $team } }, state: { type: { in: ["triage", "backlog", "unstarted"] } },
    or: [{ labels: { some: { name: { eq: $label } } } }, { state: { name: { eq: "Todo" } } }] }) {
    nodes { id identifier number state { name } creator { app guest }
      labels { nodes { id name parent { name } } } comments(first: 100) { nodes { user { app } } } } }
  active: issues(first: 100, filter: {
    team: { id: { eq: $team } }, state: { name: { eq: "In Progress" } } }) {
    nodes { labels { nodes { name parent { name } } } } }
}' "$(jq -nc --arg team "$TEAM_ID" --arg label "$LABEL" '{team: $team, label: $label}')")

# JSON lines {id, identifier, dir, label (omp label id or null)}, oldest first, one per free workspace.
PLAN=$(jq -c --argjson ws "$WORKSPACES" --argjson busy "$BUSY" --arg root "$ROOT" \
  --arg label "$LABEL" --rawfile launched "$LAUNCHED" '
  def dir: ([.labels.nodes[] | select(.parent.name == "Area") | .name][0] // "") as $a
    | if $ws | index($a) then ($root | sub("/[^/]*$"; "")) + "/mailstream-\($a)" else $root end;
  def labelled: [.labels.nodes[] | select(.name == $label) | .id][0];
  ($launched | split("\n") | map(split(" ")[0])) as $done
  | [.queued.nodes[] | select(labelled != null or (.state.name == "Todo"
      and .creator != null and .creator.app != true and .creator.guest != true
      and (.id as $id | $done | index($id) | not)
      and ([.comments.nodes[].user.app] | index(true) | not)))] as $todo
  | reduce ($todo | sort_by(.number))[] as $i (
    {busy: ([.active.nodes[] | dir] + $busy), out: []};
    ($i | dir) as $d
    | if (.busy | index($d)) then . else .busy += [$d] | .out += [$i + {dir: $d}] end)
  | .out[] | {id, identifier, dir, label: labelled}' <<<"$DATA")

[ -n "$PLAN" ] || exit 0
if [ -n "${DRY_RUN:-}" ]; then echo "$PLAN"; exit 0; fi
while read -r line; do
  id=$(jq -r .id <<<"$line")
  label=$(jq -r '.label // empty' <<<"$line")
  echo "$id $(jq -r .identifier <<<"$line")" >>"$LAUNCHED"
  [ -z "$label" ] ||
    gql 'mutation($id: String!, $label: String!) { issueRemoveLabel(id: $id, labelId: $label) { success } }' \
      "$(jq -nc --arg id "$id" --arg label "$label" '{id: $id, label: $label}')" >/dev/null
  jq -c 'del(.label)' <<<"$line"
done <<<"$PLAN"
