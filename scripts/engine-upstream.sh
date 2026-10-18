#!/bin/bash
# Lists upstream rqbit changes since the engine's recorded sync point.
#
# Clones (or fetches) https://github.com/ikatson/rqbit into a cache directory
# outside this repository, reads the sync point from engine/README.md
# ("Synced with upstream up to <sha> (<date>)") and prints every upstream
# commit after it that touches a crate the fork keeps, with the paths it
# touches and a classification hint. Commits that only touch dropped crates
# (CLI, desktop app, web UI, media server) are listed separately.
#
# Read-only: it never adds a remote to this repository, never builds or runs
# upstream code, and never pushes anywhere.
#
# Usage: scripts/engine-upstream.sh [--cache <dir>] [--branch <name>] [--prs]
#   --cache   cache directory (default: ${XDG_CACHE_HOME:-~/.cache}/qbt-neo/rqbit-upstream)
#   --branch  upstream branch to compare (default: main)
#   --prs     also list open upstream pull requests that touch kept crates
#             (needs the gh CLI; read-only API calls)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/qbt-neo/rqbit-upstream"
BRANCH=main
PRS=0
UPSTREAM_URL=https://github.com/ikatson/rqbit
while [[ $# -gt 0 ]]; do
    case "$1" in
        --cache) CACHE="$2"; shift 2 ;;
        --branch) BRANCH="$2"; shift 2 ;;
        --prs) PRS=1; shift ;;
        -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

case "$CACHE" in
    "$ROOT"|"$ROOT"/*) echo "the cache must be outside the repository: $CACHE" >&2; exit 2 ;;
esac

SYNC=$(sed -n 's/.*Synced with upstream up to `\{0,1\}\([0-9a-f]\{7,40\}\)`\{0,1\}.*/\1/p' "$ROOT/engine/README.md" | head -1)
[[ -n "$SYNC" ]] || { echo "no 'Synced with upstream up to <sha>' line in engine/README.md" >&2; exit 1; }

if [[ -d "$CACHE/.git" ]]; then
    git -C "$CACHE" fetch --quiet --tags origin "+refs/heads/*:refs/remotes/origin/*"
else
    mkdir -p "$(dirname "$CACHE")"
    git clone --quiet --no-checkout "$UPSTREAM_URL" "$CACHE"
fi
UP="$CACHE"
REF="origin/$BRANCH"
git -C "$UP" cat-file -e "$SYNC^{commit}" 2>/dev/null || { echo "sync point $SYNC not found upstream" >&2; exit 1; }

# Upstream crate directory -> fork directory.
KEPT=(librqbit librqbit_core bencode buffers clone_to_owned dht librqbit_lsd
      peer_binary_protocol sha1w tracker_comms upnp)
KEPT_PATHS=()
for c in "${KEPT[@]}"; do KEPT_PATHS+=("crates/$c"); done

hint() {
    # Classification hint from the conventional-commit prefix and the paths.
    local subject="$1" paths="$2"
    case "$subject" in
        *[Ss]ecurity*|*CVE*|*panic*|*overflow*|*unbounded*|*DoS*|*crash*) echo "security?" ;;
        fix*|*[Ff]ix*|*[Bb]ug*) echo "bug fix" ;;
        perf*|*[Pp]erf*|*[Ss]peed*|*[Ff]aster*|*[Aa]lloc*|*[Mm]emory*) echo "performance" ;;
        feat*|*[Aa]dd*|*[Ss]upport*) echo "feature" ;;
        refactor*|*[Rr]efactor*|*[Rr]ename*|*[Cc]leanup*|*[Cc]lippy*) echo "refactor" ;;
        chore*|*[Bb]ump*|*deps*|*[Rr]elease*|*[Vv]ersion*) echo "deps/release" ;;
        *) case "$paths" in *storage*) echo "storage?" ;; *) echo "?" ;; esac ;;
    esac
}

echo "Upstream: $UPSTREAM_URL ($REF at $(git -C "$UP" rev-parse --short=8 "$REF"), $(git -C "$UP" log -1 --format=%cs "$REF"))"
echo "Sync point: $(git -C "$UP" log -1 --format='%h %cs %s' "$SYNC")"
LATEST_TAG=$(git -C "$UP" describe --tags --abbrev=0 "$REF" 2>/dev/null || true)
[[ -n "$LATEST_TAG" ]] && echo "Latest tag on $BRANCH: $LATEST_TAG ($(git -C "$UP" log -1 --format=%cs "$LATEST_TAG"))"
echo "Tags after the sync point: $(git -C "$UP" tag --contains "$SYNC" | grep -v "^$" | tr '\n' ' ' || true)"
echo

N=$(git -C "$UP" rev-list --count "$SYNC..$REF")
echo "== $N upstream commit(s) since the sync point"
echo
echo "-- touching kept crates (map crates/<name> to engine/<name>):"
COUNT=0
while read -r sha; do
    [[ -n "$sha" ]] || continue
    subject=$(git -C "$UP" log -1 --format=%s "$sha")
    paths=$(git -C "$UP" diff-tree --no-commit-id --name-only -r "$sha" -- "${KEPT_PATHS[@]}" | sed 's|^crates/||' | cut -d/ -f1-3 | sort -u | tr '\n' ' ')
    printf '%s %s [%s] %s\n      %s\n' "$(git -C "$UP" log -1 --format='%h %cs' "$sha")" \
        "$(git -C "$UP" log -1 --format=%an "$sha")" "$(hint "$subject" "$paths")" "$subject" "$paths"
    COUNT=$((COUNT + 1))
done < <(git -C "$UP" rev-list --reverse --no-merges "$SYNC..$REF" -- "${KEPT_PATHS[@]}")
[[ $COUNT -gt 0 ]] || echo "   (none)"
echo
echo "-- other commits (dropped crates, CI, docs; check the Cargo.toml / Cargo.lock bumps):"
OTHER=0
while read -r sha; do
    [[ -n "$sha" ]] || continue
    if [[ -z "$(git -C "$UP" diff-tree --no-commit-id --name-only -r "$sha" -- "${KEPT_PATHS[@]}")" ]]; then
        paths=$(git -C "$UP" diff-tree --no-commit-id --name-only -r "$sha" | cut -d/ -f1-2 | sort -u | head -6 | tr '\n' ' ')
        printf '%s %s\n      %s\n' "$(git -C "$UP" log -1 --format='%h %cs' "$sha")" "$(git -C "$UP" log -1 --format=%s "$sha")" "$paths"
        OTHER=$((OTHER + 1))
    fi
done < <(git -C "$UP" rev-list --reverse --no-merges "$SYNC..$REF")
[[ $OTHER -gt 0 ]] || echo "   (none)"

if [[ $PRS -eq 1 ]]; then
    echo
    echo "-- open upstream pull requests touching kept crates:"
    command -v gh >/dev/null || { echo "   gh not installed"; exit 0; }
    gh pr list -R ikatson/rqbit --state open -L 100 --json number,title,updatedAt,files \
        --jq '.[] | select([.files[].path | test("^crates/(librqbit|librqbit_core|bencode|buffers|clone_to_owned|dht|librqbit_lsd|peer_binary_protocol|sha1w|tracker_comms|upnp)/")] | any) | "#\(.number) \(.updatedAt[0:10]) \(.title)"'
fi

echo
echo "Port with: git -C $UP format-patch -1 <sha> --stdout > \"\$TMPDIR/p.patch\""
echo "           git apply -p2 --directory=engine --reject \"\$TMPDIR/p.patch\"   (in this repository)"
echo "Then commit as \"Port upstream rqbit <shortsha>: <subject>\" and update engine/README.md and docs/ENGINE.md."
