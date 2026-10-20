#!/bin/sh
# Runs the Web UI's unit tests with Node's built-in test runner (no
# dependencies; npx fetches the pinned esbuild once): the TypeScript torrent
# name parser against the macOS app's results in
# crates/qbt-core/webui/test/names-fixture.json.
#
# The fixture comes from the app's parser; regenerate it after changing the
# parser, the built-in rules or the corpus:
#
#   TEST_RUNNER_QBT_WRITE_NAME_FIXTURE=1 xcodebuild test -project apps/macos/QBTNeo.xcodeproj \
#       -scheme QBTNeoUnitTests -only-testing:QBTNeoUnitTests/NameParserWebFixtureTests
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
UI="$ROOT/crates/qbt-core/webui"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/webui-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

npx --yes esbuild@0.25.4 "$UI/test/names.test.mjs" --bundle --platform=node --format=esm --target=node20 \
  --log-level=warning --outfile="$TMP/names.test.mjs"
FIXTURE="$UI/test/names-fixture.json" node --test "$TMP/names.test.mjs"
