#!/usr/bin/env bash
# Installs HushCoreAudio.driver into the system HAL plug-in folder, restarts coreaudiod and
# waits until both HushCore devices are visible. Idempotent: re-running replaces the bundle.
#
# Usage: sudo scripts/macos/install-plugin.sh [path/to/HushCoreAudio.driver]
set -euo pipefail

readonly PLUGIN_NAME="HushCoreAudio.driver"
readonly HAL_DIR="/Library/Audio/Plug-Ins/HAL"
readonly WAIT_SECONDS=20
readonly DEVICE_NAMES=("HushCore Sink" "HushCore Microphone")

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "install-plugin: this script runs on macOS only" >&2
  exit 1
fi

if [[ "$EUID" -ne 0 ]]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "install-plugin: must run as root (sudo scripts/macos/install-plugin.sh)" >&2
    exit 1
  fi
  echo "install-plugin: re-running with sudo"
  exec sudo -- bash "$SCRIPT_DIR/install-plugin.sh" "$@"
fi

INVOKER="${SUDO_USER:-root}"
SRC="${1:-$ROOT/dist/driver/macos/$PLUGIN_NAME}"
DEST="$HAL_DIR/$PLUGIN_NAME"

if [[ ! -d "$SRC" ]]; then
  echo "install-plugin: bundle not found: $SRC (run scripts/macos/build-plugin.sh first)" >&2
  exit 1
fi

# Fail closed: coreaudiod loads whatever is installed here, so the source must not be
# modifiable by anyone other than its owner, and must not contain symbolic links.
if [[ -n "$(find "$SRC" -type l -print -quit)" ]]; then
  echo "install-plugin: refusing $SRC: it contains symbolic links" >&2
  exit 1
fi
if [[ -n "$(find "$SRC" \( -perm -020 -o -perm -002 \) -print -quit)" ]]; then
  echo "install-plugin: refusing $SRC: it has group- or world-writable entries" >&2
  exit 1
fi
if [[ -n "$(find "$SRC" ! -user root ! -user "$INVOKER" -print -quit)" ]]; then
  echo "install-plugin: refusing $SRC: entries are not owned by root or $INVOKER" >&2
  exit 1
fi

# Stage inside the root-owned HAL folder (same volume, so the final move is a rename),
# verify the staged copy, and only then move it into place.
mkdir -p "$HAL_DIR"
STAGE="$(mktemp -d "$HAL_DIR/.hushcore-stage.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
chown root:wheel "$STAGE"
chmod 700 "$STAGE"
cp -R "$SRC" "$STAGE/$PLUGIN_NAME"
chown -R root:wheel "$STAGE/$PLUGIN_NAME"
chmod -R u=rwX,go=rX "$STAGE/$PLUGIN_NAME"
if ! codesign --verify --deep --strict "$STAGE/$PLUGIN_NAME"; then
  echo "install-plugin: staged bundle failed codesign verification; nothing installed" >&2
  exit 1
fi

rm -rf "$DEST"
mv "$STAGE/$PLUGIN_NAME" "$DEST"
echo "install-plugin: installed bundle at $DEST"

# coreaudiod loads HAL plug-ins once; a kickstart makes it rescan the folder.
if ! launchctl kickstart -kp system/com.apple.audio.coreaudiod >/dev/null 2>&1; then
  killall coreaudiod >/dev/null 2>&1 || true
fi

has_device() {
  system_profiler SPAudioDataType 2>/dev/null | grep -Fq "$1:"
}

deadline=$((SECONDS + WAIT_SECONDS))
while true; do
  missing=()
  for name in "${DEVICE_NAMES[@]}"; do
    if ! has_device "$name"; then
      missing+=("$name")
    fi
  done
  if [[ ${#missing[@]} -eq 0 ]]; then
    break
  fi
  if (( SECONDS >= deadline )); then
    echo "install-plugin: not visible after ${WAIT_SECONDS}s: ${missing[*]}" >&2
    echo "install-plugin: inspect with: log show --last 5m --predicate 'process == \"coreaudiod\"'" >&2
    exit 1
  fi
  sleep 1
done

echo "install-plugin: ${DEVICE_NAMES[*]} visible"
