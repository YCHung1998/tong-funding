#!/bin/bash
# Launches the app, captures ONLY its window (never the desktop), then quits it.
# Usage: tools/window_shot.sh <out.png> [binary] [seconds-to-wait]
set -euo pipefail
OUT="${1:?usage: window_shot.sh out.png [binary] [wait]}"
BIN="${2:-./target/debug/tong-funding}"
WAIT="${3:-3}"
HERE="$(cd "$(dirname "$0")" && pwd)"
"$BIN" >/dev/null 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
sleep "$WAIT"
WID="$(swift "$HERE/window_id.swift" "$PID")"
if [ "$WID" = "0" ]; then echo "no on-screen window for pid $PID" >&2; exit 2; fi
screencapture -x -o -l"$WID" "$OUT"
echo "captured window $WID -> $OUT"
