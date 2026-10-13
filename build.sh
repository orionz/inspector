#!/usr/bin/env bash
# Builds the static site into dist/. Usage: ./build.sh [--dev]
set -euo pipefail
cd "$(dirname "$0")"

PROFILE=release
CARGO_FLAGS=(--release)
if [[ "${1:-}" == "--dev" ]]; then
  PROFILE=debug
  CARGO_FLAGS=()
fi

# The build number is the commit count, shown in the app header.
export APP_VERSION="v$(git rev-list --count HEAD 2>/dev/null || echo 0)"
export APP_COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo dev)"

cargo build -p app --target wasm32-unknown-unknown "${CARGO_FLAGS[@]}"

rm -rf dist
mkdir -p dist/pkg dist/samples
wasm-bindgen --target web --no-typescript --out-dir dist/pkg \
  "target/wasm32-unknown-unknown/$PROFILE/automerge-inspector.wasm"

if [[ "$PROFILE" == release ]] && command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --all-features \
    -o dist/pkg/automerge-inspector_bg.wasm dist/pkg/automerge-inspector_bg.wasm \
    || echo "wasm-opt failed; shipping the unoptimised module"
fi

VERSION="$APP_VERSION-$APP_COMMIT-$(date +%s)"
cp web/* dist/
sed -i "s/__VERSION__/$VERSION/g" dist/index.html dist/sw.js
cp corpus/* dist/samples/
touch dist/.nojekyll

echo "built dist/ ($APP_VERSION, $VERSION): $(du -sh dist | cut -f1), wasm $(du -h dist/pkg/automerge-inspector_bg.wasm | cut -f1)"
