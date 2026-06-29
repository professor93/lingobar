#!/usr/bin/env bash
#
# Production build: a size-optimized .dmg copied into ./release/, then
# all generated build files (src-tauri/target + dist) are cleaned up.
#
# Usage:
#   npm run release              # universal (Intel + Apple Silicon)
#   npm run release -- silicon   # Apple Silicon only (aarch64)
#   npm run release -- intel     # Intel only (x86_64)
#
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-universal}" in
  silicon | aarch64 | arm64) TARGET="aarch64-apple-darwin" ;;
  intel | x86_64)            TARGET="x86_64-apple-darwin" ;;
  *)                         TARGET="universal-apple-darwin" ;;
esac

OUT="release"
echo "▸ Building LingoBar ($TARGET, size-optimized release profile)…"
npm run tauri build -- --target "$TARGET"

BUNDLE="src-tauri/target/$TARGET/release/bundle"
mkdir -p "$OUT"
echo "▸ Collecting artifacts into ./$OUT/"
find "$BUNDLE" -name "*.dmg" -print -exec cp -f {} "$OUT/" \;

echo "▸ Cleaning generated build files (cargo clean + dist)…"
cargo clean --manifest-path src-tauri/Cargo.toml
rm -rf dist

echo "✓ Done. Artifacts in ./$OUT/:"
ls -lh "$OUT"
