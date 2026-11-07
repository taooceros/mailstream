#!/bin/sh
# Opens a Linear issue in an omp session in a new Tern tab at this repo; the session opens its
# own omp checkout to work in (AGENTS.md, Version control). Linear's "Work on issue → Custom
# script" runs it (~/.linear/coding-tools.json: args ["{{issue.identifier}}"]); it also works by
# hand: scripts/linear-omp.sh MS-12.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
exec tern new tab --cwd "$ROOT" -- omp "Work on Linear issue $1."
