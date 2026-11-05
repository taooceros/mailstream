#!/bin/sh
# Opens a Linear issue in an omp session in a new Tern tab, in the jj workspace matching the
# issue's Area label (../mailstream-<area>), else this repo. Linear's "Work on issue → Custom
# script" runs it (~/.linear/coding-tools.json: args ["{{issue.identifier}}"]); it also works by
# hand: scripts/linear-omp.sh MS-12.
set -eu
ISSUE="$1"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TOKEN=$(security find-generic-password -s linear-omp -a access_token -w)
AREA=$(curl -fsS https://api.linear.app/graphql -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"query\":\"{ issue(id: \\\"$ISSUE\\\") { labels { nodes { name parent { name } } } } }\"}" |
  jq -r '[.data.issue.labels.nodes[] | select(.parent.name == "Area") | .name][0] // empty')
DIR="$ROOT"
[ -n "$AREA" ] && [ -d "$ROOT/../mailstream-$AREA" ] && DIR=$(cd "$ROOT/../mailstream-$AREA" && pwd)
exec tern new tab --cwd "$DIR" -- omp "Work on Linear issue $ISSUE."
