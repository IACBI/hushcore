import importlib.util
import os
import sys
import types

import pytest

BACKEND_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if BACKEND_DIR not in sys.path:
    sys.path.insert(0, BACKEND_DIR)

# Loaded under its own name: test_api.py replaces sys.modules["audio_processor"] with a stub.
_spec = importlib.util.spec_from_file_location(
    "hushcore_devices_audio_processor", os.path.join(BACKEND_DIR, "audio_processor.py")
)
ap = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ap)

import virtual_device  # noqa: E402


def dev(name, inputs=0, outputs=0, hostapi=0):
    return {
        "name": name,
        "hostapi": hostapi,
        "max_input_channels": inputs,
        "max_output_channels": outputs,
        "default_samplerate": 48000.0,
    }


@pytest.fixture
def use_devices(monkeypatch):
    """Replaces the sounddevice listing and default device pair seen by AudioProcessor."""

    def install(devices, default_in, default_out):
        fake = types.SimpleNamespace(
            query_devices=lambda: devices,
            query_hostapis=lambda: [{"name": "Windows WASAPI"}],
            default=types.SimpleNamespace(device=[default_in, default_out]),
        )
        monkeypatch.setattr(ap, "sd", fake)

    return install


def pick(name, is_input):
    # _find_device_idx never reads instance state, so no processor needs to be built.
    return ap.AudioProcessor._find_device_idx(None, name, is_input=is_input)


def test_empty_output_prefers_hushcore_sink_over_the_default(use_devices):
    use_devices(
        [dev("Speakers", outputs=2), dev("Headphones", outputs=2), dev("hushcore sink (Virtual)", outputs=2)],
        default_in=-1,
        default_out=0,
    )
    assert pick("", is_input=False) == 2


def test_empty_output_without_hushcore_uses_the_system_default(use_devices):
    use_devices([dev("Speakers", outputs=2), dev("Headphones", outputs=2)], default_in=-1, default_out=1)
    assert pick("", is_input=False) == 1


def test_empty_output_ignores_other_virtual_devices(use_devices):
    use_devices(
        [dev("Speakers", outputs=2), dev("Some Other Virtual Device", outputs=2)],
        default_in=-1,
        default_out=0,
    )
    assert pick("", is_input=False) == 0


def test_empty_input_uses_the_system_default_when_it_is_not_hushcore_microphone(use_devices):
    use_devices([dev("Mic", inputs=1), dev("Webcam", inputs=1)], default_in=1, default_out=-1)
    assert pick("", is_input=True) == 1


def test_empty_input_never_defaults_to_hushcore_microphone(use_devices):
    use_devices(
        [dev("Mic", inputs=1), dev("HushCore Microphone (Virtual)", inputs=1)],
        default_in=1,
        default_out=-1,
    )
    assert pick("", is_input=True) == 0


def test_empty_input_without_a_default_skips_hushcore_microphone(use_devices):
    use_devices(
        [dev("HushCore Microphone (Virtual)", inputs=1), dev("Mic", inputs=1)],
        default_in=-1,
        default_out=-1,
    )
    assert pick("", is_input=True) == 1


def test_only_hushcore_microphone_as_input_is_an_error(use_devices):
    use_devices([dev("HushCore Microphone (Virtual)", inputs=1)], default_in=0, default_out=-1)
    with pytest.raises(RuntimeError):
        pick("", is_input=True)


def test_explicit_device_names_are_still_matched_as_chosen(use_devices):
    use_devices(
        [dev("Speakers", outputs=2), dev("HushCore Sink (Virtual)", outputs=2), dev("HushCore Microphone", inputs=1)],
        default_in=-1,
        default_out=0,
    )
    assert pick("HushCore Sink", is_input=False) == 1
    assert pick("HushCore Microphone", is_input=True) == 2


def test_status_is_installed_only_with_both_endpoints():
    devices = {
        "inputs": [{"name": "HushCore Microphone [Windows WASAPI]"}],
        "outputs": [{"name": "HushCore Sink [Windows WASAPI]"}],
    }
    status = virtual_device.status_from_devices(devices)
    assert status["installed"] is True
    assert status["provider"] == virtual_device.platform_name()
    assert status["sink"] == "HushCore Sink [Windows WASAPI]"
    assert status["microphone"] == "HushCore Microphone [Windows WASAPI]"


def test_status_with_only_the_sink_is_not_installed():
    status = virtual_device.status_from_devices({"inputs": [], "outputs": [{"name": "HushCore Sink"}]})
    assert status["installed"] is False
    assert status["provider"] == "none"
    assert status["microphone"] is None
    assert "HushCore Microphone" in status["detail"]


def test_status_from_engine_reply_keeps_the_engine_provider():
    reply = {
        "installed": True,
        "provider": "macos-plugin",
        "sink": "HushCore Sink",
        "microphone": "HushCore Microphone",
        "detail": "ok",
    }
    status = virtual_device.status_from_engine(reply)
    assert status["installed"] is True
    assert status["provider"] == "macos-plugin"
    assert status["detail"] == "ok"

    missing = virtual_device.status_from_engine({"installed": False, "provider": "none", "sink": None, "microphone": None})
    assert missing["installed"] is False
    assert missing["provider"] == "none"
    assert missing["microphone"] is None


@pytest.mark.parametrize(("platform", "name", "install", "uninstall"), [
    (
        "win32", "windows",
        r"powershell -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1",
        r"powershell -ExecutionPolicy Bypass -File scripts\windows\uninstall-driver.ps1",
    ),
    ("darwin", "macos", "sudo scripts/macos/install-plugin.sh", "sudo scripts/macos/uninstall-plugin.sh"),
    ("linux", "linux", "scripts/linux/install-virtual-mic.sh", "scripts/linux/uninstall-virtual-mic.sh"),
])
def test_platform_selects_the_install_commands(monkeypatch, platform, name, install, uninstall):
    monkeypatch.setattr(sys, "platform", platform)
    status = virtual_device.status_from_devices({"inputs": [], "outputs": []})
    assert status["platform"] == name
    assert status["install_command"] == install
    assert status["uninstall_command"] == uninstall


def test_unknown_platform_has_no_install_command(monkeypatch):
    monkeypatch.setattr(sys, "platform", "freebsd13")
    status = virtual_device.status_from_devices({"inputs": [], "outputs": []})
    assert status["platform"] == "freebsd13"
    assert status["install_command"] is None
    assert status["uninstall_command"] is None
