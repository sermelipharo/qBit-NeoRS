#!/bin/bash
# Builds the Rust core as a static library, generates the Swift bindings and
# packages both into build/QBTCoreFFI.xcframework for the Xcode project.
#
# Usage: scripts/build-core.sh [debug|release]
# Xcode calls it from a pre-build phase with $CONFIGURATION set.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-27.0}"

MODE="${1:-}"
if [[ -z "$MODE" ]]; then
    case "${CONFIGURATION:-Debug}" in
        Release) MODE=release ;;
        *) MODE=debug ;;
    esac
fi

cd "$ROOT"
# Respect CARGO_TARGET_DIR and any .cargo/config.toml target-dir.
TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null || echo "$ROOT/target")}"
PROFILE_FLAG=()
if [[ "$MODE" == "release" ]]; then
    PROFILE_FLAG=(--release)
    LIB="$TARGET_DIR/release/libqbt_ffi.a"
else
    LIB="$TARGET_DIR/debug/libqbt_ffi.a"
fi
# Static C libraries that build scripts link with `static:-bundle` (the
# vendored Lua of the plugins engine) are not inside the Rust archive; list
# them from cargo's build-script messages so they can be merged in below.
build_ffi() {
    cargo build -p qbt-ffi --lib ${PROFILE_FLAG[@]+"${PROFILE_FLAG[@]}"} --message-format=json-render-diagnostics | python3 -c '
import json, os, sys
libs = []
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") != "build-script-executed":
        continue
    paths = [p.split("=", 1)[-1] for p in m.get("linked_paths", [])]
    for lib in m.get("linked_libs", []):
        kind, _, name = lib.rpartition("=")
        if "-bundle" not in kind:
            continue
        for d in paths:
            f = os.path.join(d, "lib" + name + ".a")
            if os.path.isfile(f):
                libs.append(f)
                break
print("\n".join(libs))
'
}
UNBUNDLED="$(build_ffi)"
# The `cc` crate does not rebuild C code when MACOSX_DEPLOYMENT_TARGET
# changes, so a plain `cargo test` in the shared target dir leaves objects
# built for the SDK version: rebuild them for the app's deployment target.
if [[ -n "$UNBUNDLED" ]] && otool -l $UNBUNDLED 2>/dev/null | awk '$1 == "minos" { print $2 }' | grep -qvx "$MACOSX_DEPLOYMENT_TARGET"; then
    cargo clean -q -p mlua-sys ${PROFILE_FLAG[@]+"${PROFILE_FLAG[@]}"}
    UNBUNDLED="$(build_ffi)"
fi

OUT="$ROOT/build"
XCF="$OUT/QBTCoreFFI.xcframework"
GEN="$ROOT/apps/macos/Generated"
STAMP="$OUT/.ffi-stamp-$MODE"

# The cargo target dir may be shared by several worktrees, whose builds
# overwrite the same uplifted archive: snapshot ours right after building.
mkdir -p "$OUT"
SNAPSHOT="$OUT/libqbt_ffi-$MODE.a"
if [[ -n "$UNBUNDLED" ]]; then
    EXTRA=()
    while IFS= read -r f; do EXTRA+=("$f"); done <<< "$UNBUNDLED"
    libtool -static -no_warning_for_no_symbols -o "$SNAPSHOT" "$LIB" "${EXTRA[@]}"
else
    cp "$LIB" "$SNAPSHOT"
fi
LIB="$SNAPSHOT"

# Skip regeneration when the archive hasn't changed since the last run.
if [[ -f "$STAMP" && -d "$XCF" && -f "$GEN/qbt_core.swift" ]] && cmp -s "$LIB" "$XCF/macos-arm64/$(basename "$LIB")"; then
    rm -f "$SNAPSHOT"
    exit 0
fi

rm -rf "$OUT/gen" "$OUT/ffi-headers" "$XCF"
mkdir -p "$OUT/gen" "$OUT/ffi-headers" "$GEN"
cargo run -q -p qbt-ffi --bin uniffi-bindgen -- \
    generate --library "$LIB" --language swift --out-dir "$OUT/gen"
cp "$OUT/gen/qbt_coreFFI.h" "$OUT/ffi-headers/"
cp "$OUT/gen/qbt_coreFFI.modulemap" "$OUT/ffi-headers/module.modulemap"
cp "$OUT/gen/qbt_core.swift" "$GEN/qbt_core.swift"
xcodebuild -create-xcframework -library "$LIB" -headers "$OUT/ffi-headers" -output "$XCF" >/dev/null
# Xcode copies the xcframework into the products directory while planning
# the build, before this pre-build phase runs: mirror the fresh library and
# headers there too, or the app links the previous build of the core.
if [[ -n "${BUILT_PRODUCTS_DIR:-}" ]]; then
    mkdir -p "$BUILT_PRODUCTS_DIR/include"
    cp "$XCF/macos-arm64/$(basename "$LIB")" "$BUILT_PRODUCTS_DIR/"
    cp "$OUT/ffi-headers/"* "$BUILT_PRODUCTS_DIR/include/"
fi
rm -f "$SNAPSHOT"
touch "$STAMP"
