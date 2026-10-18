#!/bin/bash
# Launches the built app with the given arguments, waits, and prints its
# physical memory footprint (what Activity Monitor calls "Memory").
#
# Usage: scripts/measure-footprint.sh [--app <path>] [--wait <seconds>] [-- app args...]
# Example: scripts/measure-footprint.sh --wait 20 -- --demo-large
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/build/DerivedData/Build/Products/Debug/qBit NeoRS.app"
WAIT=15
while [[ $# -gt 0 ]]; do
    case "$1" in
        --app) APP="$2"; shift 2 ;;
        --wait) WAIT="$2"; shift 2 ;;
        --) shift; break ;;
        *) break ;;
    esac
done
BIN="$APP/Contents/MacOS/qBit NeoRS"
[[ -x "$BIN" ]] || { echo "app binary not found: $BIN" >&2; exit 1; }

"$BIN" "$@" >/dev/null 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for ((i = 0; i < WAIT; i++)); do
    sleep 1
    kill -0 $PID 2>/dev/null || { echo "app exited early" >&2; exit 1; }
done
vmmap --summary "$PID" 2>/dev/null | grep -E "Physical footprint" || true
ps -o rss=,vsz= -p "$PID" | awk '{printf "RSS: %.1f MiB\n", $1/1024}'
