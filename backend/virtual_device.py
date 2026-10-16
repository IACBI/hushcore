"""Status and naming rules of HushCore's own virtual microphone.

The engine writes to HushCore Sink and applications read HushCore Microphone. Both endpoints are
created by the platform component installed from scripts/, so no third-party virtual audio
software is involved.
"""

import sys

SINK_NAME = "HushCore Sink"
MICROPHONE_NAME = "HushCore Microphone"

_INSTALL_COMMANDS = {
    # Releases ship a test-signed driver until it is attestation-signed (docs/SIGNING.md).
    "windows": r"powershell -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1 -AllowTestSigned",
    "macos": "sudo scripts/macos/install-plugin.sh",
    "linux": "scripts/linux/install-virtual-mic.sh",
}
_UNINSTALL_COMMANDS = {
    "windows": r"powershell -ExecutionPolicy Bypass -File scripts\windows\uninstall-driver.ps1",
    "macos": "sudo scripts/macos/uninstall-plugin.sh",
    "linux": "scripts/linux/uninstall-virtual-mic.sh",
}


def platform_name():
    if sys.platform == "win32":
        return "windows"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform.startswith("linux"):
        return "linux"
    return sys.platform


def is_sink(name):
    return _contains(name, SINK_NAME)


def is_microphone(name):
    return _contains(name, MICROPHONE_NAME)


def _contains(name, target):
    return target.lower() in str(name or "").lower()


def _first_name(items, target):
    for item in items:
        if _contains(item.get("name"), target):
            return item["name"]
    return None


def _status(installed, provider, sink, microphone, detail):
    platform = platform_name()
    return {
        "installed": installed,
        "provider": provider,
        "sink": sink,
        "microphone": microphone,
        "detail": detail,
        "platform": platform,
        "install_command": _INSTALL_COMMANDS.get(platform),
        "uninstall_command": _UNINSTALL_COMMANDS.get(platform),
    }


def status_from_devices(devices):
    """Status computed from a listing shaped like AudioProcessor.get_devices()."""
    sink = _first_name(devices.get("outputs", []), SINK_NAME)
    microphone = _first_name(devices.get("inputs", []), MICROPHONE_NAME)
    if sink and microphone:
        return _status(True, platform_name(), sink, microphone, f"{SINK_NAME} and {MICROPHONE_NAME} are installed.")
    if microphone:
        detail = f"{MICROPHONE_NAME} is visible but {SINK_NAME} is not."
    elif sink:
        detail = f"{SINK_NAME} is visible but {MICROPHONE_NAME} is not."
    else:
        detail = f"{MICROPHONE_NAME} is not installed."
    return _status(False, "none", sink, microphone, detail)


def status_from_engine(reply):
    """Status from the virtual_device object of the native engine's devices event (PROTOCOL.md)."""
    installed = reply.get("installed") is True
    sink = reply.get("sink") or None
    microphone = reply.get("microphone") or None
    provider = str(reply.get("provider") or "none") if installed else "none"
    return _status(installed, provider, sink, microphone, str(reply.get("detail") or ""))
