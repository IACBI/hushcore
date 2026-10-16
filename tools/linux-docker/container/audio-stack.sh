#!/usr/bin/env bash
# Starts and stops a headless PipeWire audio session for the test: dbus, pipewire, wireplumber and
# pipewire-pulse under a private XDG_RUNTIME_DIR. Sourced by entrypoint.sh and e2e.py's restart step.
#
#   audio-stack.sh start | stop | restart
#
# No sound hardware is needed: the test loads null sinks, which PipeWire runs on its own clock.
set -euo pipefail

export XDG_RUNTIME_DIR="${HUSH_XDG_RUNTIME_DIR:-/tmp/hush-xdg}"
export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR}/bus"
export XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
LOG_DIR="${HUSH_LOG_DIR:-/tmp/hush-logs}"

stop_stack() {
    pkill -u "$(id -u)" -x pipewire-pulse 2>/dev/null || true
    pkill -u "$(id -u)" -x wireplumber 2>/dev/null || true
    pkill -u "$(id -u)" -x pipewire 2>/dev/null || true
    pkill -u "$(id -u)" -x dbus-daemon 2>/dev/null || true
    # Wait for the sockets to disappear so a restart does not reuse a dying server.
    for _ in $(seq 1 50); do
        pgrep -u "$(id -u)" -x pipewire >/dev/null 2>&1 || break
        sleep 0.1
    done
    rm -f "${XDG_RUNTIME_DIR}"/pipewire-0 "${XDG_RUNTIME_DIR}"/pipewire-0.lock \
          "${XDG_RUNTIME_DIR}"/pulse/native "${XDG_RUNTIME_DIR}"/bus 2>/dev/null || true
}

start_stack() {
    mkdir -p "$XDG_RUNTIME_DIR" "$LOG_DIR"
    chmod 700 "$XDG_RUNTIME_DIR"
    dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --fork --nopidfile \
        >"$LOG_DIR/dbus.log" 2>&1
    pipewire >"$LOG_DIR/pipewire.log" 2>&1 &
    # WirePlumber needs the PipeWire socket; pipewire-pulse needs both.
    for _ in $(seq 1 100); do
        [ -S "$XDG_RUNTIME_DIR/pipewire-0" ] && break
        sleep 0.05
    done
    wireplumber >"$LOG_DIR/wireplumber.log" 2>&1 &
    pipewire-pulse >"$LOG_DIR/pipewire-pulse.log" 2>&1 &
    for _ in $(seq 1 200); do
        if pactl info 2>/dev/null | grep -q 'Server Name:.*PipeWire'; then
            echo "audio-stack: PipeWire $(pactl info | sed -n 's/^Server Version: //p') ready"
            return 0
        fi
        sleep 0.05
    done
    echo "audio-stack: pipewire-pulse did not come up; logs in $LOG_DIR" >&2
    return 1
}

case "${1:-}" in
    start) start_stack ;;
    stop) stop_stack ;;
    restart) stop_stack; start_stack ;;
    *) echo "usage: $0 start|stop|restart" >&2; exit 2 ;;
esac
