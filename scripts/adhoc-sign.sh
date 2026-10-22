#!/bin/bash
# Re-signs an ad-hoc release build so it can load Sparkle.framework.
#
# The hardened runtime's library validation only loads frameworks signed by
# the app's own team, and ad-hoc code has no team, so dyld refuses the
# embedded Sparkle.framework. Release downloads therefore add
# com.apple.security.cs.disable-library-validation to the app's entitlements
# (the rest of the hardened runtime stays on: no DYLD_* variables, no
# unsigned executable memory). Builds signed with a team don't need it.
#
# Usage: scripts/adhoc-sign.sh "path/to/qBit NeoRS.app"
set -euo pipefail

APP="${1:?usage: $0 path/to/App.app}"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/adhoc-sign.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# Inside out: Sparkle's helpers, the framework, the widget extension, the app.
SPARKLE="$APP/Contents/Frameworks/Sparkle.framework"
if [[ -d "$SPARKLE" ]]; then
    for item in "$SPARKLE"/Versions/B/XPCServices/*.xpc "$SPARKLE/Versions/B/Autoupdate" "$SPARKLE/Versions/B/Updater.app"; do
        [[ -e "$item" ]] && codesign --force --sign - --options runtime --preserve-metadata=entitlements "$item"
    done
    codesign --force --sign - --options runtime "$SPARKLE"
fi
for appex in "$APP"/Contents/PlugIns/*.appex; do
    [[ -e "$appex" ]] && codesign --force --sign - --options runtime --preserve-metadata=entitlements "$appex"
done

codesign -d --entitlements :- "$APP" > "$WORK/entitlements.plist" 2>/dev/null
[[ -s "$WORK/entitlements.plist" ]] || plutil -create xml1 "$WORK/entitlements.plist"
# PlistBuddy, not plutil: plutil reads the dots in the key as a key path.
/usr/libexec/PlistBuddy -c "Delete :com.apple.security.cs.disable-library-validation" "$WORK/entitlements.plist" 2>/dev/null || true
/usr/libexec/PlistBuddy -c "Add :com.apple.security.cs.disable-library-validation bool true" "$WORK/entitlements.plist"
codesign --force --sign - --options runtime --entitlements "$WORK/entitlements.plist" "$APP"
codesign --verify --deep --strict "$APP"
