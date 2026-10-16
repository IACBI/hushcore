#!/usr/bin/env bash
# Removes HushCoreAudio.driver from the system HAL plug-in folder and restarts coreaudiod.
# Idempotent: exits 0 when the bundle is already absent.
#
# Usage: sudo scripts/macos/uninstall-plugin.sh
set -euo pipefail

readonly PLUGIN_NAME="HushCoreAudio.driver"
readonly HAL_DIR="/Library/Audio/Plug-Ins/HAL"
readonly WAIT_SECONDS=20
readonly DEVICE_NAMES=("HushCore Sink" "HushCore Microphone")

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "uninstall-plugin: this script runs on macOS only" >&2
  exit 1
fi

if [[ "$EUID" -ne 0 ]]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "uninstall-plugin: must run as root (sudo scripts/macos/uninstall-plugin.sh)" >&2
    exit 1
  fi
  echo "uninstall-plugin: re-running with sudo"
  exec sudo -- bash "$SCRIPT_DIR/uninstall-plugin.sh" "$@"
fi

DEST="$HAL_DIR/$PLUGIN_NAME"

if [[ ! -e "$DEST" ]]; then
  echo "uninstall-plugin: $DEST is not installed"
  exit 0
fi

rm -rf "$DEST"
echo "uninstall-plugin: removed $DEST"

if ! launchctl kickstart -kp system/com.apple.audio.coreaudiod >/dev/null 2>&1; then
  killall coreaudiod >/dev/null 2>&1 || true
fi

has_device() {
  system_profiler SPAudioDataType 2>/dev/null | grep -Fq "$1:"
}

deadline=$((SECONDS + WAIT_SECONDS))
while true; do
  present=()
  for name in "${DEVICE_NAMES[@]}"; do
    if has_device "$name"; then
      present+=("$name")
    fi
  done
  if [[ ${#present[@]} -eq 0 ]]; then
    break
  fi
  if (( SECONDS >= deadline )); then
    echo "uninstall-plugin: still visible after ${WAIT_SECONDS}s: ${present[*]}" >&2
    exit 1
  fi
  sleep 1
done

echo "uninstall-plugin: HushCore devices are gone"
