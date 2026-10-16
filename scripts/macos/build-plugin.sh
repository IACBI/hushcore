#!/usr/bin/env bash
# Builds the universal HushCoreAudio.driver bundle (ad-hoc signed) and copies it to
# dist/driver/macos/ for scripts/macos/install-plugin.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
MAKE_DIR="$ROOT/driver/macos"
DIST_DIR="$ROOT/dist/driver/macos"
BUNDLE="HushCoreAudio.driver"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "build-plugin: this script runs on macOS only" >&2
  exit 1
fi

make -C "$MAKE_DIR" all

mkdir -p "$DIST_DIR"
rm -rf "$DIST_DIR/$BUNDLE"
cp -R "$MAKE_DIR/build/$BUNDLE" "$DIST_DIR/"
codesign --verify --strict "$DIST_DIR/$BUNDLE"

echo "build-plugin: $DIST_DIR/$BUNDLE"
