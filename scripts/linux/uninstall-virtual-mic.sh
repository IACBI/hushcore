#!/usr/bin/env bash
# Removes what install-virtual-mic.sh installed for the current user. Idempotent; no root.
set -euo pipefail

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
PIPEWIRE_DROP_IN="$CONFIG_HOME/pipewire/pipewire.conf.d/90-hushcore-virtual-mic.conf"
PULSE_DIR="$CONFIG_HOME/pulse"
PULSE_INCLUDE="$PULSE_DIR/hushcore-virtual-mic.pa"
PULSE_DEFAULT="$PULSE_DIR/default.pa"
BEGIN_MARK="# >>> hushcore virtual microphone >>>"
END_MARK="# <<< hushcore virtual microphone <<<"

die() {
    echo "uninstall-virtual-mic: $*" >&2
    exit 1
}

if [ "$(id -u)" -eq 0 ]; then
    die "run as your normal user, not root"
fi

pactl_ok() {
    command -v pactl >/dev/null 2>&1 && pactl info >/dev/null 2>&1
}

# Unloads modules that create or remap the pair by name (on-demand or from PulseAudio's include).
unload_pulse_modules() {
    local index args
    while read -r index _ args; do
        case "$args" in
            *sink_name=hushcore_sink*|*source_name=hushcore_mic*)
                pactl unload-module "$index" || true
                ;;
        esac
    done < <(pactl list short modules 2>/dev/null)
}

remove_pulse_include() {
    if [ -f "$PULSE_DEFAULT" ] && grep -qxF "$BEGIN_MARK" "$PULSE_DEFAULT"; then
        local tmp
        tmp="$(mktemp "$PULSE_DIR/default.pa.XXXXXX")"
        awk -v b="$BEGIN_MARK" -v e="$END_MARK" '
            $0 == b { skip = 1; next }
            $0 == e { skip = 0; next }
            !skip
        ' "$PULSE_DEFAULT" >"$tmp"
        mv "$tmp" "$PULSE_DEFAULT"
        echo "uninstall-virtual-mic: removed the include block from $PULSE_DEFAULT"
    fi
    if [ -f "$PULSE_INCLUDE" ]; then
        rm -f "$PULSE_INCLUDE"
        echo "uninstall-virtual-mic: removed $PULSE_INCLUDE"
    fi
}

remove_pipewire_drop_in() {
    if [ -f "$PIPEWIRE_DROP_IN" ]; then
        rm -f "$PIPEWIRE_DROP_IN"
        echo "uninstall-virtual-mic: removed $PIPEWIRE_DROP_IN"
        if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
            systemctl --user restart pipewire pipewire-pulse wireplumber
            echo "uninstall-virtual-mic: restarted the user audio services"
        else
            echo "uninstall-virtual-mic: restart PipeWire (or log out and back in) to drop the device" >&2
        fi
    fi
}

server_info="$(pactl info 2>/dev/null || true)"
if pactl_ok && printf '%s\n' "$server_info" | grep -q 'Server Name:.*PipeWire'; then
    remove_pipewire_drop_in
else
    if pactl_ok; then
        unload_pulse_modules
    fi
    remove_pulse_include
    remove_pipewire_drop_in
fi

if pactl_ok; then
    if pactl list short sinks | awk -F'\t' '$2 == "hushcore_sink" { found = 1 } END { exit !found }' ||
        pactl list short sources | awk -F'\t' '$2 == "hushcore_mic" { found = 1 } END { exit !found }'; then
        echo "uninstall-virtual-mic: HushCore devices are still loaded in this session; they disappear on the next audio restart" >&2
    else
        echo "uninstall-virtual-mic: no HushCore devices are loaded"
    fi
fi
