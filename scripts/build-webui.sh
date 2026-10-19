#!/bin/sh
# Builds the bundled Web UI: crates/qbt-core/webui/src -> crates/qbt-core/webui/www.
#
# The minified output is committed, so cargo builds never need Node; run this
# after changing anything under webui/src and commit both. Needs Node (npx
# fetches the pinned esbuild/TypeScript once into the npm cache).
#
#   scripts/build-webui.sh           build
#   scripts/build-webui.sh --check   type-check with tsc first
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
UI="$ROOT/crates/qbt-core/webui"
SRC="$UI/src"
OUT="$UI/www"
ESBUILD="esbuild@0.25.4"
TYPESCRIPT="typescript@5.9.3"

if [ "${1:-}" = "--check" ]; then
  npx --yes -p "$TYPESCRIPT" tsc -p "$UI/tsconfig.json"
fi

rm -rf "$OUT"
mkdir -p "$OUT/public" "$OUT/private"

JS_FLAGS="--bundle --minify --format=iife --target=es2022,safari17,chrome120,firefox120 --legal-comments=none --log-level=warning"
npx --yes "$ESBUILD" "$SRC/main.ts" $JS_FLAGS --outfile="$OUT/private/app.js"
npx --yes "$ESBUILD" "$SRC/login.ts" $JS_FLAGS --outfile="$OUT/public/login.js"
# light-dark() needs Safari 17.5 / Chrome 123 / Firefox 120; no lowering.
npx --yes "$ESBUILD" "$SRC/app.css" --bundle --minify --target=safari17.5,chrome123,firefox120 --log-level=warning --outfile="$OUT/public/app.css"
cp "$SRC/icon.svg" "$OUT/public/icon.svg"
printf 'User-agent: *\nDisallow: /\n' > "$OUT/public/robots.txt"

# Content hashes as cache busters (static files are cached for 12 hours).
hash() { shasum -a 256 "$1" | cut -c1-12; }
JS=$(hash "$OUT/private/app.js")
LOGIN=$(hash "$OUT/public/login.js")
CSS=$(hash "$OUT/public/app.css")
sed -e "s/__JS__/$JS/" -e "s/__CSS__/$CSS/" "$SRC/index.html" > "$OUT/private/index.html"
sed -e "s/__LOGIN__/$LOGIN/" -e "s/__CSS__/$CSS/" "$SRC/login.html" > "$OUT/public/index.html"

total_raw=0
total_gz=0
for f in private/index.html private/app.js public/index.html public/login.js public/app.css public/icon.svg; do
  raw=$(wc -c < "$OUT/$f" | tr -d ' ')
  gz=$(gzip -9c "$OUT/$f" | wc -c | tr -d ' ')
  total_raw=$((total_raw + raw))
  total_gz=$((total_gz + gz))
  printf '%-20s %7s B  %6s B gzip\n' "$f" "$raw" "$gz"
done
printf '%-20s %7s B  %6s B gzip\n' "total" "$total_raw" "$total_gz"
