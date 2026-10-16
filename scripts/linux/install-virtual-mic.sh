#!/usr/bin/env bash
# Installs HushCore's virtual microphone for the current user. No root: the device lives in the user
# session. Idempotent; uninstall with uninstall-virtual-mic.sh. See driver/linux/README.md.
#
#   PipeWire:   drop-in config, then restart the user audio services.
#   PulseAudio: module file included from ~/.config/pulse/default.pa, and the modules loaded now.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIVER_DIR="$SCRIPT_DIR/../../driver/linux"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
PIPEWIRE_DROP_IN="$CONFIG_HOME/pipewire/pipewire.conf.d/90-hushcore-virtual-mic.conf"
PULSE_DIR="$CONFIG_HOME/pulse"
PULSE_INCLUDE="$PULSE_DIR/hushcore-virtual-mic.pa"
PULSE_DEFAULT="$PULSE_DIR/default.pa"
BEGIN_MARK="# >>> hushcore virtual microphone >>>"
END_MARK="# <<< hushcore virtual microphone <<<"

die() {
    echo "install-virtual-mic: $*" >&2
    exit 1
}

if [ "$(id -u)" -eq 0 ]; then
    die "run as your normal user, not root: the virtual microphone belongs to your audio session"
fi
command -v pactl >/dev/null 2>&1 || die "pactl not found; install pulseaudio-utils"

# Prints "pipewire" or "pulseaudio". Runs in a subshell, so it reports failure by status, not by exit.
server_kind() {
    local info
    info="$(pactl info 2>/dev/null || true)"
    if [ -z "$info" ]; then
        echo "no PulseAudio or PipeWire server is reachable; log in to your desktop session first" >&2
        return 1
    fi
    if printf '%s\n' "$info" | grep -q 'Server Name:.*PipeWire'; then
        echo pipewire
    else
        echo pulseaudio
    fi
}

# Restarts the user services if systemd manages them. Returns 1 when it cannot, so the caller
# tells the user to restart by hand.
restart_pipewire() {
    if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
        systemctl --user restart pipewire pipewire-pulse wireplumber
        return 0
    fi
    return 1
}

device_present() {
    pactl list short "$1" 2>/dev/null | awk -F'\t' -v n="$2" '$2 == n { found = 1 } END { exit !found }'
}

# Waits until both endpoints are listed. Prints what it found.
verify_pair() {
    local tries=0
    while [ "$tries" -lt 50 ]; do
        if device_present sinks hushcore_sink && device_present sources hushcore_mic; then
            echo "install-virtual-mic: verified HushCore Sink and HushCore Microphone are present"
            return 0
        fi
        tries=$((tries + 1))
        sleep 0.1
    done
    return 1
}

install_pipewire() {
    mkdir -p "$(dirname "$PIPEWIRE_DROP_IN")"
    install -m 0644 "$DRIVER_DIR/pipewire/90-hushcore-virtual-mic.conf" "$PIPEWIRE_DROP_IN"
    echo "install-virtual-mic: installed $PIPEWIRE_DROP_IN"
    if restart_pipewire; then
        verify_pair || die "PipeWire restarted, but HushCore Sink or HushCore Microphone did not appear"
    else
        echo "install-virtual-mic: systemd user services not available here; restart PipeWire" \
             "(log out and back in, or restart pipewire, wireplumber and pipewire-pulse) and check with:" \
             "pactl list short sinks | grep hushcore_sink"
    fi
}

install_pulseaudio() {
    mkdir -p "$PULSE_DIR"
    install -m 0644 "$DRIVER_DIR/pulseaudio/hushcore-virtual-mic.pa" "$PULSE_INCLUDE"
    if [ ! -f "$PULSE_DEFAULT" ]; then
        [ -f /etc/pulse/default.pa ] || die "/etc/pulse/default.pa is missing; cannot build $PULSE_DEFAULT"
        printf '.include /etc/pulse/default.pa\n' >"$PULSE_DEFAULT"
    fi
    if ! grep -qxF "$BEGIN_MARK" "$PULSE_DEFAULT"; then
        printf '\n%s\n.include %s\n%s\n' "$BEGIN_MARK" "$PULSE_INCLUDE" "$END_MARK" >>"$PULSE_DEFAULT"
    fi
    echo "install-virtual-mic: included $PULSE_INCLUDE from $PULSE_DEFAULT"

    # Load the modules now as well; the include only takes effect at the next server start.
    if ! device_present sinks hushcore_sink; then
        pactl load-module module-null-sink sink_name=hushcore_sink \
            'sink_properties="device.description=\"HushCore Sink\""' >/dev/null
    fi
    if ! device_present sources hushcore_mic; then
        pactl load-module module-remap-source source_name=hushcore_mic master=hushcore_sink.monitor \
            'source_properties="device.description=\"HushCore Microphone\""' >/dev/null
    fi
    verify_pair || die "the modules loaded, but HushCore Sink or HushCore Microphone is not listed"
}

kind="$(server_kind)" || exit 1
case "$kind" in
    pipewire) install_pipewire ;;
    pulseaudio) install_pulseaudio ;;
esac
