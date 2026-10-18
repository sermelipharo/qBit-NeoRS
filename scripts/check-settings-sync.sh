#!/bin/bash
# Checks that the Settings code (apps/macos/QBTNeo/Features/Preferences) is the
# same in qBit NeoRS and its sibling qBittorrent Neo, apart from the product
# name. Usage: scripts/check-settings-sync.sh [path to the qbittorrent_neo repo]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SIBLING="${1:-$ROOT/../qbittorrent_neo}"
DIR=apps/macos/QBTNeo/Features/Preferences
[[ -d "$SIBLING/$DIR" ]] || { echo "sibling repo not found: $SIBLING" >&2; exit 2; }

status=0
files="$( (cd "$ROOT/$DIR" && find . -name '*.swift'; cd "$SIBLING/$DIR" && find . -name '*.swift') | sort -u)"
for file in $files; do
    ours="$ROOT/$DIR/$file"
    theirs="$SIBLING/$DIR/$file"
    if [[ ! -f "$ours" || ! -f "$theirs" ]]; then
        echo "only in one repo: $file"
        status=1
        continue
    fi
    if ! diff -q <(sed 's/qBittorrent Neo/qBit NeoRS/g' "$theirs") "$ours" >/dev/null; then
        echo "differs: $file"
        diff <(sed 's/qBittorrent Neo/qBit NeoRS/g' "$theirs") "$ours" | head -20
        status=1
    fi
done
[[ $status -eq 0 ]] && echo "Settings code is in sync."
exit $status
