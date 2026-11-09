#!/bin/sh
# Measures a front end's frame rate the same way for every front end (AGENTS.md, "Performance:
# 120 fps"): its release build runs on a private virtual display at BENCH_HZ, plays a scenario
# by itself, logs every frame and quits; scripts/frame-stats.swift then summarizes the logs.
# Usage: scripts/bench.sh flutter|gpui [RUNS]
#   BENCH_SCENARIO picks the scenario (MAILSTREAM_BENCH): scroll (default; every front end)
#   scrolls the conversation list; reader-swipe (Flutter) opens conversations and swipes each
#   reader away with two fingers; calendar-week (Flutter) opens the calendar's week view and
#   scrolls its timeline from top to bottom and back (needs BENCH_HOME, whose seed holds a
#   week of events, fresh for 5 minutes after seeding).
#   MAILSTREAM_READER_RENDERER (Flutter, Mac) opens the reader in blitz (default) or webKit,
#   e.g. BENCH_SCENARIO=reader-swipe MAILSTREAM_READER_RENDERER=webKit; hyper still opens the
#   experimental HyperRender, which is set aside (MS-171).
#   Build the release first: Flutter/README.md, or CONFIG=release GPUI/run.sh.
#   BENCH_HZ (120) is the display's refresh rate. Each run first waits up to 2 minutes for the
#   1-minute load average to fall below BENCH_MAX_LOAD (4): builds in other workspaces skew
#   frame times. Logs go to BENCH_DIR (/tmp/mailstream-bench).
#   BENCH_HOME=<dir> runs the front end on the throwaway home in <dir> (HOME and
#   CFFIXED_USER_HOME, and a sign-ins Keychain item of its own) instead of your data; fill it
#   with the engine benchmark's mail: (cd FFI && cargo run --release --target-dir target/bench
#   --features swift-engine --example bench -- --seed <dir>).
set -eu
cd "$(dirname "$0")/.."
FRONTEND=${1:-}
RUNS=${2:-3}
HZ=${BENCH_HZ:-120}
MAX_LOAD=${BENCH_MAX_LOAD:-4}
DIR=${BENCH_DIR:-/tmp/mailstream-bench}
SCENARIO=${BENCH_SCENARIO:-scroll}
case $FRONTEND in
  flutter) BINARY="Flutter/build/macos/Build/Products/Release/Mailstream Flutter.app/Contents/MacOS/Mailstream Flutter" ;;
  gpui) BINARY=GPUI/target/release/mailstream-gpui ;;
  *)
    echo "usage: scripts/bench.sh flutter|gpui [RUNS]" >&2
    exit 2
    ;;
esac
case $FRONTEND/$SCENARIO in
  */scroll | flutter/reader-swipe | flutter/calendar-week) ;;
  *)
    echo "error: no $SCENARIO scenario in $FRONTEND" >&2
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
  LOG="$DIR/$FRONTEND-$SCENARIO-$run.tsv"
  rm -f "$LOG"
  START_LOAD=$(load)
  echo "run $run/$RUNS (load $START_LOAD)…" >&2
  if [ -n "${BENCH_HOME:-}" ]; then
    HOME="$BENCH_HOME" CFFIXED_USER_HOME="$BENCH_HOME" MAILSTREAM_KEYCHAIN_SERVICE=dev.mailstream.bench.frames \
      MAILSTREAM_BENCH="$SCENARIO" MAILSTREAM_BENCH_OUT="$LOG" "$BINARY" > "$DIR/$FRONTEND-$run.out" 2>&1 &
  else
    MAILSTREAM_BENCH="$SCENARIO" MAILSTREAM_BENCH_OUT="$LOG" "$BINARY" > "$DIR/$FRONTEND-$run.out" 2>&1 &
  fi
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
  # The app settles, plays the scenario for under a minute, writes the log and quits.
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
