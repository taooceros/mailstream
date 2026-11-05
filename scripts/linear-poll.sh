#!/bin/bash
# One pass over Linear: claims what an omp session should act on now and prints it as JSON lines,
# which that session hands to subagents. dir is the jj workspace to work in (../mailstream-<area>
# for the issue's Area label, else this repo).
#   {"kind": "issue", "id", "identifier", "dir", "busy"}: an issue to start. Claimed when it is
#     - in Todo, created by a workspace member (not an app, a guest or a GitHub import: the repo
#       is public and the subagents run with full access), not claimed before and not yet
#       commented on by an app (omp); or
#     - labelled `omp`, in any state but started, done or canceled; the label is removed on
#       claim, so adding it again reruns the issue.
#     Agents file their own issues in Backlog and move issues to Todo to ask the user something,
#     so neither is picked up. Issues share their area's workspace: busy is true when that
#     workspace has an In Progress issue, is listed in BUSY (JSON array of dirs, e.g. the caller's
#     running subagents) or got an earlier issue this pass; the caller then hands the issue to the
#     subagent already working nearby code, or gives it a private workspace. Nothing waits.
#   {"kind": "comment", "id", "issue", "issueId", "state", "dir", "createdAt", "body"}: a comment a
#     workspace member (not an app, a guest or a GitHub sync) posted on any team issue in the last
#     7 days, each once. The caller routes it to the issue's subagent or starts one.
# Claims are kept in ~/.local/state/linear-poll/: `launched` (issue ids), `comments` (comment ids).
# DRY_RUN=1 prints without claiming.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TEAM_ID=${TEAM_ID:-9f8fe59f-3788-4542-8c18-4e5c5dd5ddc2} # Mailstream; the id survives key renames
LABEL=${LABEL:-omp}
RENEW="$HOME/.pi/agent/extensions/linear/renew-omp-token.sh"
STATE=${STATE:-$HOME/.local/state/linear-poll}
LAUNCHED="$STATE/launched"
HANDLED="$STATE/comments"
mkdir -p "$STATE" && touch "$LAUNCHED" "$HANDLED"

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
  comments(first: 100, filter: { createdAt: { gt: "-P7D" }, issue: { team: { id: { eq: $team } } } }) {
    nodes { id createdAt body user { app guest }
      issue { id identifier state { name } labels { nodes { name parent { name } } } } } }
}' "$(jq -nc --arg team "$TEAM_ID" --arg label "$LABEL" '{team: $team, label: $label}')")

# Issue lines oldest first, one per free workspace, then comment lines oldest first.
PLAN=$(jq -c --argjson ws "$WORKSPACES" --argjson busy "$BUSY" --arg root "$ROOT" \
  --arg label "$LABEL" --rawfile launched "$LAUNCHED" --rawfile handled "$HANDLED" '
  def dir: ([.labels.nodes[] | select(.parent.name == "Area") | .name][0] // "") as $a
    | if $ws | index($a) then ($root | sub("/[^/]*$"; "")) + "/mailstream-\($a)" else $root end;
  def labelled: [.labels.nodes[] | select(.name == $label) | .id][0];
  def member: . != null and .app != true and .guest != true;
  ($launched | split("\n") | map(split(" ")[0])) as $done
  | ($handled | split("\n")) as $seen
  | [.queued.nodes[] | select(labelled != null or (.state.name == "Todo"
      and (.creator | member)
      and (.id as $id | $done | index($id) | not)
      and ([.comments.nodes[].user.app] | index(true) | not)))] as $todo
  | (reduce ($todo | sort_by(.number))[] as $i (
      {busy: ([.active.nodes[] | dir] + $busy), out: []};
      ($i | dir) as $d
      | .out += [$i + {dir: $d, busy: (.busy | index($d) != null)}] | .busy += [$d])
    | .out[] | {kind: "issue", id, identifier, dir, busy, label: labelled}),
    (.comments.nodes | sort_by(.createdAt)[]
      | select((.user | member) and (.id as $id | $seen | index($id) | not))
      | {kind: "comment", id, issue: .issue.identifier, issueId: .issue.id,
         state: .issue.state.name, dir: (.issue | dir), createdAt, body})' <<<"$DATA")

[ -n "$PLAN" ] || exit 0
if [ -n "${DRY_RUN:-}" ]; then echo "$PLAN"; exit 0; fi
while read -r line; do
  id=$(jq -r .id <<<"$line")
  if [ "$(jq -r .kind <<<"$line")" = comment ]; then
    echo "$id" >>"$HANDLED"
    echo "$line"
    continue
  fi
  label=$(jq -r '.label // empty' <<<"$line")
  echo "$id $(jq -r .identifier <<<"$line")" >>"$LAUNCHED"
  [ -z "$label" ] ||
    gql 'mutation($id: String!, $label: String!) { issueRemoveLabel(id: $id, labelId: $label) { success } }' \
      "$(jq -nc --arg id "$id" --arg label "$label" '{id: $id, label: $label}')" >/dev/null
  jq -c 'del(.label)' <<<"$line"
done <<<"$PLAN"
