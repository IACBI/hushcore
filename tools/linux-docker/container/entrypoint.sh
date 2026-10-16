#!/usr/bin/env bash
# Runs inside the Linux test image: build, unit tests, clippy, then the end-to-end audio test.
# Invoked by tools/linux-docker/run.sh with the repository mounted read-only at /src.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO=/src
MANIFEST="$REPO/engine/Cargo.toml"
export HUSH_XDG_RUNTIME_DIR=/tmp/hush-xdg
export HUSH_LOG_DIR=/tmp/hush-logs
# Every client (pactl, parec, the engine) must find the same server socket.
export XDG_RUNTIME_DIR="$HUSH_XDG_RUNTIME_DIR"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$HUSH_XDG_RUNTIME_DIR/bus"
export ENGINE="$CARGO_TARGET_DIR/release/hushcore-engine"

step() { printf '\n==== %s ====\n' "$1"; }

cleanup() { bash "$HERE/audio-stack.sh" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

step "rust toolchain"
rustc --version
cargo --version

step "cargo build --release --locked"
cargo build --release --locked --manifest-path "$MANIFEST"

step "cargo test --release --locked"
cargo test --release --locked --manifest-path "$MANIFEST"

step "cargo clippy --release --locked -- -D warnings"
cargo clippy --release --locked --manifest-path "$MANIFEST" -- -D warnings

step "shellcheck scripts"
shellcheck "$REPO/scripts/linux/install-virtual-mic.sh" "$REPO/scripts/linux/uninstall-virtual-mic.sh" \
    "$HERE/audio-stack.sh" "$HERE/entrypoint.sh" "$REPO/tools/linux-docker/run.sh"

step "audio stack"
bash "$HERE/audio-stack.sh" start

step "end-to-end"
python3 -I "$HERE/e2e.py" --engine "$ENGINE" --repo "$REPO"

step "PASS"
