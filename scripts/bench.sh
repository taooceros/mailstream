#!/bin/sh
# Measures a front end's frame rate the same way for every front end (AGENTS.md, "Performance:
# 120 fps"): its release build runs on a private virtual display at BENCH_HZ, scrolls its
# conversation list by itself (MAILSTREAM_BENCH=scroll), logs every frame and quits;
# scripts/frame-stats.swift then summarizes the logs.
# Usage: scripts/bench.sh flutter|gpui [RUNS]
#   Build the release first: Flutter/README.md, or CONFIG=release GPUI/run.sh.
#   BENCH_HZ (120) is the display's refresh rate. Each run first waits up to 2 minutes for the
#   1-minute load average to fall below BENCH_MAX_LOAD (4): builds in other workspaces skew
#   frame times. Logs go to BENCH_DIR (/tmp/mailstream-bench).
set -eu
cd "$(dirname "$0")/.."
FRONTEND=${1:-}
RUNS=${2:-3}
HZ=${BENCH_HZ:-120}
MAX_LOAD=${BENCH_MAX_LOAD:-4}
DIR=${BENCH_DIR:-/tmp/mailstream-bench}
case $FRONTEND in
  flutter) BINARY="Flutter/build/macos/Build/Products/Release/Mailstream Flutter.app/Contents/MacOS/Mailstream Flutter" ;;
  gpui) BINARY=GPUI/target/release/mailstream-gpui ;;
  *)
    echo "usage: scripts/bench.sh flutter|gpui [RUNS]" >&2
    exit 2
    ;;
esac
if [ ! -x "$BINARY" ]; then
  echo "error: no release build at $BINARY" >&2
  exit 1
fi

HELPER=${TMPDIR:-/tmp}/mailstream-vdisplay
if [ ! -x "$HELPER" ] || [ scripts/vdisplay.m -nt "$HELPER" ]; then
  clang -fobjc-arc -framework Foundation -framework CoreGraphics -o "$HELPER" scripts/vdisplay.m
fi
mkdir -p "$DIR"
"$HELPER" 1480 920 "$HZ" > "$DIR/display" &
DISPLAY_PID=$!
APP_PID=
trap 'kill $DISPLAY_PID $APP_PID 2>/dev/null || true' EXIT
trap 'exit 130' INT TERM
for _ in $(seq 50); do
  [ -s "$DIR/display" ] && break
  sleep 0.1
done
if ! read -r _ _ X Y _ _ RATE < "$DIR/display"; then
  echo "error: the virtual display did not come up" >&2
  exit 1
fi
if [ "$RATE" != "$HZ" ]; then
  echo "error: the virtual display runs at $RATE Hz, not $HZ" >&2
  exit 1
fi

load() { sysctl -n vm.loadavg | awk '{ print $2 }'; }
busy() { awk -v load="$(load)" -v max="$MAX_LOAD" 'BEGIN { exit !(load >= max) }'; }

LOGS=
for run in $(seq "$RUNS"); do
  waited=0
  while busy && [ $waited -lt 120 ]; do
    sleep 5
    waited=$((waited + 5))
  done
  LOG="$DIR/$FRONTEND-$run.tsv"
  rm -f "$LOG"
  START_LOAD=$(load)
  echo "run $run/$RUNS (load $START_LOAD)…" >&2
  MAILSTREAM_BENCH=scroll MAILSTREAM_BENCH_OUT="$LOG" "$BINARY" > "$DIR/$FRONTEND-$run.out" 2>&1 &
  APP_PID=$!
  # The same window size on the virtual display for every front end.
  parked=
  for _ in $(seq 100); do
    if parked=$(osascript -e "tell application \"System Events\" to tell (first process whose unix id is $APP_PID)
      if (count windows) = 0 then return \"\"
      set position of window 1 to {$((X + 20)), $((Y + 40))}
      set size of window 1 to {1240, 800}
      return \"parked\"
    end tell" 2> /dev/null) && [ "$parked" = parked ]; then
      break
    fi
    sleep 0.2
  done
  if [ "$parked" != parked ]; then
    echo "error: could not move the window onto the virtual display (System Events needs Accessibility access)" >&2
    exit 1
  fi
  # The app settles, scrolls for about half a minute, writes the log and quits.
  for _ in $(seq 240); do
    kill -0 "$APP_PID" 2> /dev/null || break
    sleep 0.5
  done
  kill "$APP_PID" 2> /dev/null || true
  wait "$APP_PID" || true
  APP_PID=
  if [ ! -s "$LOG" ]; then
    echo "error: run $run wrote no log; see $DIR/$FRONTEND-$run.out" >&2
    exit 1
  fi
  echo "# load=${START_LOAD}→$(load)" >> "$LOG"
  LOGS="$LOGS $LOG"
done
# shellcheck disable=SC2086
swift scripts/frame-stats.swift --hz "$HZ" $LOGS
