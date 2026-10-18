#!/bin/bash
# Cutover from qBittorrent 5.x to qBit NeoRS (see docs/MIGRATION.md).
#
#   scripts/migrate-from-qbittorrent.sh            dry run: prints the report, changes nothing
#   scripts/migrate-from-qbittorrent.sh --yes      migrates into the default profile
#   options: --import-gui (merge mode: also import qBittorrent's GUI settings)
#            --skipped-file-data unclaim (see docs)
#
# qBittorrent's files are only read and stay as they are. Before writing, the
# profile's small files (config/, data/BT_backup, data/stats.json) and the
# app's defaults domain are copied to a timestamped backup. Nothing is
# launched: start qBit NeoRS yourself afterwards.
set -euo pipefail

DOMAIN=mk.lex.qbitneors
PROFILE="$HOME/Library/Application Support/qBit NeoRS"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
APPLY=0
IMPORT_GUI=0
EXTRA=()
while [ $# -gt 0 ]; do
    case "$1" in
        --yes) APPLY=1 ;;
        --import-gui) IMPORT_GUI=1 ;;
        --skipped-file-data) EXTRA+=("--skipped-file-data" "$2"); shift ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

die() { echo "error: $*" >&2; exit 1; }

# The daemon binary: $NOX, or built from this checkout.
if [ -z "${NOX:-}" ]; then
    (cd "$REPO" && cargo build -q --release -p qbt-nox)
    TARGET_DIR=$(cd "$REPO" && cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
    NOX="$TARGET_DIR/release/qbit-neors-nox"
fi
[ -x "$NOX" ] || die "qbit-neors-nox not found ($NOX)"

# Both apps must be quit: qBittorrent rewrites its resume data on exit, and
# qBit NeoRS its profile (the tool refuses a profile in use).
if pgrep -x qbittorrent >/dev/null; then die "qBittorrent is running: quit it first"; fi

MODE=()
if [ -f "$PROFILE/config/preferences.json" ]; then
    MODE=(--merge)
    echo "Existing qBit NeoRS profile found: merge mode (torrents already there keep their state; only missing data is imported)."
fi

ARGS=(migrate-from-qbittorrent --profile "$PROFILE" ${MODE[@]+"${MODE[@]}"} ${EXTRA[@]+"${EXTRA[@]}"})

if [ "$APPLY" -ne 1 ]; then
    "$NOX" "${ARGS[@]}" --dry-run
    echo
    echo "Dry run only. Run again with --yes to migrate."
    exit 0
fi

STAMP=$(date +%Y%m%d-%H%M%S)
BACKUP="$PROFILE.before-migration-$STAMP"
if [ -d "$PROFILE" ]; then
    mkdir -p "$BACKUP/data"
    [ -d "$PROFILE/config" ] && cp -Rp "$PROFILE/config" "$BACKUP/"
    [ -d "$PROFILE/data/BT_backup" ] && cp -Rp "$PROFILE/data/BT_backup" "$BACKUP/data/"
    [ -f "$PROFILE/data/stats.json" ] && cp -p "$PROFILE/data/stats.json" "$BACKUP/data/"
    echo "Profile files backed up to: $BACKUP"
fi
mkdir -p "$BACKUP"
if defaults export "$DOMAIN" "$BACKUP/$DOMAIN.plist" 2>/dev/null; then
    echo "Defaults domain $DOMAIN backed up to: $BACKUP/$DOMAIN.plist"
fi

"$NOX" "${ARGS[@]}"

# GUI settings: full migration imports them; merge mode only with --import-gui.
PLIST="$PROFILE/config/gui-preferences.plist"
[ ${#MODE[@]} -gt 0 ] && PLIST="$PROFILE/migration/gui-preferences.plist"
if [ -f "$PLIST" ] && { [ ${#MODE[@]} -eq 0 ] || [ "$IMPORT_GUI" -eq 1 ]; }; then
    WORK=$(mktemp -d)
    if [ -f "$BACKUP/$DOMAIN.plist" ]; then
        cp "$BACKUP/$DOMAIN.plist" "$WORK/merged.plist"
        plutil -convert xml1 "$WORK/merged.plist"
    else
        printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict/></plist>\n' > "$WORK/merged.plist"
    fi
    # Migrated keys replace existing ones; everything else is kept.
    while IFS= read -r key; do
        [ -n "$key" ] && /usr/libexec/PlistBuddy -c "Delete :$key" "$WORK/merged.plist" >/dev/null 2>&1 || true
    done < "$PROFILE/migration/gui-preferences.keys"
    cp "$PLIST" "$WORK/gui.plist"   # PlistBuddy commands can't take paths with spaces
    /usr/libexec/PlistBuddy -c "Merge $WORK/gui.plist" "$WORK/merged.plist" >/dev/null
    defaults import "$DOMAIN" "$WORK/merged.plist"
    rm -rf "$WORK"
    echo "GUI settings imported into $DOMAIN."
fi

echo
echo "Done. Report: $PROFILE/migration/report.txt"
echo "Nothing was started: open qBit NeoRS yourself."
