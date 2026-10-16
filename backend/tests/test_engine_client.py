import json
import os
import pathlib
import sys
import tempfile
import time

import pytest

BACKEND_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if BACKEND_DIR not in sys.path:
    sys.path.insert(0, BACKEND_DIR)

import engine_client  # noqa: E402
from engine_client import NativeEngineClient  # noqa: E402

FAKE_ENGINE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake_engine.py")

ENGINE_STATUS_KEYS = {
    "input_rms", "output_rms", "gate_state", "fft_data", "is_running",
    "df_available", "vst_loaded", "vst_failed", "sample_rate",
    "latency_ms", "cpu_load", "clipping", "engine", "xruns",
    "engine_mode", "engine_notice", "reconnecting", "reconnect_reason",
}


@pytest.fixture
def tmp_path():
    # pytest's default temp root is not writable on some Windows profiles
    with tempfile.TemporaryDirectory(prefix="hushcore-test-") as directory:
        yield pathlib.Path(directory)


@pytest.fixture
def command_log(tmp_path, monkeypatch):
    path = tmp_path / "commands.jsonl"
    monkeypatch.setenv("HUSHCORE_FAKE_ENGINE_LOG", str(path))
    return path


@pytest.fixture
def make_client():
    clients = []

    def factory(settings=None, start_timeout=3.0, start_error_grace=0.3):
        if settings is None:
            settings = {
                "input_device": "Fake Mic",
                "output_device": "",
                "monitor_device": "",
                "monitor_enabled": False,
                "gate_release_ms": 150.0,
            }
        client = NativeEngineClient(
            settings,
            [sys.executable, "-u", FAKE_ENGINE],
            start_timeout=start_timeout,
            start_error_grace=start_error_grace,
        )
        clients.append(client)
        return client

    yield factory
    for client in clients:
        client.shutdown()


def wait_for(predicate, timeout=3.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return predicate()


def read_log(path):
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def test_status_before_start_has_full_key_set(make_client):
    client = make_client()
    status = client.get_status()
    assert set(status) == ENGINE_STATUS_KEYS
    assert status["is_running"] is False
    assert status["input_rms"] == -120.0
    assert status["engine"] == "native"
    assert status["xruns"] == 0


def test_start_status_and_stop(make_client):
    client = make_client()
    assert client.start() is True
    assert client.is_running() is True
    assert client.running.is_set() is True

    assert wait_for(lambda: client.get_status()["input_rms"] == -30.0)
    status = client.get_status()
    assert set(status) == ENGINE_STATUS_KEYS
    assert status["is_running"] is True
    assert status["engine"] == "native"
    assert status["sample_rate"] == 48000
    assert status["latency_ms"] == 10.0
    assert status["gate_state"] is True
    assert len(status["fft_data"]) == 64
    assert status["df_available"] is False
    assert status["vst_loaded"] is False
    assert status["vst_failed"] is False

    client.stop()
    assert client.is_running() is False
    assert client.get_status()["is_running"] is False


def test_start_while_running_is_a_noop(make_client, command_log):
    client = make_client()
    assert client.start() is True
    assert client.start() is True
    client.shutdown()
    assert [c["cmd"] for c in read_log(command_log)] == ["start", "quit"]


def test_settings_and_monitor_reach_the_engine(make_client, command_log):
    settings = {
        "input_device": "Fake Mic",
        "output_device": "Fake Speakers",
        "monitor_device": "Fake Speakers",
        "monitor_enabled": False,
        "gate_release_ms": 150.0,
    }
    client = make_client(settings)
    assert client.start() is True

    settings["gate_release_ms"] = 500.0
    client.update_settings(settings)
    client.start_monitoring("Fake Speakers")
    client.stop_monitoring()
    client.stop()
    client.shutdown()

    commands = read_log(command_log)
    assert [c["cmd"] for c in commands] == ["start", "settings", "monitor", "monitor", "stop", "quit"]

    start = commands[0]
    assert start["input"] == "Fake Mic"
    assert start["output"] == "Fake Speakers"
    assert start["mode"] == "shared"
    assert start["settings"]["gate_release_ms"] == 150.0
    assert commands[1]["settings"]["gate_release_ms"] == 500.0
    assert commands[2] == {"cmd": "monitor", "enabled": True, "device": "Fake Speakers"}
    assert commands[3] == {"cmd": "monitor", "enabled": False, "device": "Fake Speakers"}


def test_default_start_requests_shared_mode(make_client, command_log):
    client = make_client()
    assert client.start() is True
    status = client.get_status()
    assert status["engine_mode"] == "shared"
    assert status["engine_notice"] is None
    client.shutdown()
    start = read_log(command_log)[0]
    assert start["cmd"] == "start"
    assert start["mode"] == "shared"


def test_exclusive_request_reports_engine_mode_and_no_notice(make_client, command_log):
    settings = {"input_device": "__exclusive_ok__", "output_device": "", "monitor_device": "",
                "monitor_enabled": False, "audio_mode": "exclusive"}
    client = make_client(settings)
    assert client.start() is True
    status = client.get_status()
    assert status["engine_mode"] == "exclusive"
    assert status["engine_notice"] is None
    client.shutdown()
    assert read_log(command_log)[0]["mode"] == "exclusive"


def test_exclusive_fallback_keeps_start_and_reports_shared_with_notice(make_client, command_log):
    settings = {"input_device": "Fake Mic", "output_device": "", "monitor_device": "",
                "monitor_enabled": False, "audio_mode": "exclusive"}
    client = make_client(settings)
    assert client.start() is True
    status = client.get_status()
    assert status["is_running"] is True
    assert status["engine_mode"] == "shared"
    assert "using shared mode" in status["engine_notice"]
    client.shutdown()
    assert read_log(command_log)[0]["mode"] == "exclusive"


def test_runtime_notice_keeps_engine_running(make_client):
    settings = {"input_device": "__notice__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    assert wait_for(lambda: client.get_status()["engine_notice"] is not None)
    assert client.is_running() is True
    assert client.get_status()["engine_notice"] == "monitor output device lost"
    client.shutdown()


def test_notice_is_cleared_by_the_next_start(make_client):
    settings = {"input_device": "__notice__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    assert wait_for(lambda: client.get_status()["engine_notice"] is not None)
    client.stop()
    settings["input_device"] = "Fake Mic"
    assert client.start() is True
    assert client.get_status()["engine_notice"] is None
    client.shutdown()


def test_error_without_started_fails_after_the_grace_period(make_client):
    settings = {"input_device": "__error__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings, start_timeout=10.0, start_error_grace=0.3)
    began = time.monotonic()
    assert client.start() is False
    assert time.monotonic() - began < 3
    client.shutdown()


def test_stop_when_idle_does_not_launch_the_engine(make_client, command_log):
    client = make_client()
    client.stop()
    assert client._proc is None
    assert read_log(command_log) == []


def test_error_event_fails_start_and_next_start_respawns(make_client):
    settings = {"input_device": "__error__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is False
    assert client.is_running() is False

    settings["input_device"] = "Fake Mic"
    assert client.start() is True
    assert client.is_running() is True
    client.shutdown()


def test_start_timeout_returns_false_and_cleans_up(make_client):
    settings = {"input_device": "__hang__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings, start_timeout=0.5)
    began = time.monotonic()
    assert client.start() is False
    assert time.monotonic() - began < 5
    assert client.is_running() is False
    assert client._proc is None


def test_missing_executable_returns_false(tmp_path):
    client = NativeEngineClient({"input_device": ""}, [str(tmp_path / "missing-hushcore-engine.exe")])
    assert client.start() is False
    assert client.is_running() is False
    client.shutdown()


def test_crash_is_reported_and_next_start_restarts(make_client):
    settings = {"input_device": "__crash__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    assert wait_for(lambda: not client.is_running())
    assert client.get_status()["is_running"] is False

    settings["input_device"] = "Fake Mic"
    assert client.start() is True
    assert wait_for(client.is_running)
    client.shutdown()


def test_stale_status_reports_not_running(make_client, monkeypatch):
    monkeypatch.setattr(engine_client, "STATUS_STALE_S", 0.5)
    settings = {"input_device": "__silent__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    assert client.is_running() is True
    assert wait_for(lambda: not client.is_running(), timeout=3)
    assert client.get_status()["is_running"] is False
    client.shutdown()


def test_xrun_count_is_reported(make_client):
    settings = {"input_device": "__xrun__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    assert wait_for(lambda: client.get_status()["xruns"] > 0)
    client.shutdown()


def test_malformed_engine_line_is_ignored(make_client):
    settings = {"input_device": "__garbage__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    assert client.is_running() is True
    client.shutdown()


def test_shutdown_quits_the_engine_process(make_client):
    client = make_client()
    assert client.start() is True
    proc = client._proc
    client.shutdown()
    assert proc.returncode == 0
    assert client._proc is None
    assert client.is_running() is False


def test_shutdown_terminates_an_engine_that_ignores_quit(make_client):
    settings = {"input_device": "__deaf__", "output_device": "", "monitor_device": "", "monitor_enabled": False}
    client = make_client(settings)
    assert client.start() is True
    proc = client._proc
    began = time.monotonic()
    client.shutdown()
    assert time.monotonic() - began < 5
    assert proc.poll() is not None
    assert proc.returncode != 0
    assert client._proc is None


def test_reconnecting_keeps_engine_running_past_status_staleness(make_client, command_log):
    client = make_client({"input_device": "__reconnect__", "output_device": "", "monitor_device": "", "monitor_enabled": False})
    assert client.start() is True
    assert wait_for(lambda: client.get_status()["reconnecting"])

    # No status events arrive while the engine retries, which is longer than STATUS_STALE_S.
    time.sleep(engine_client.STATUS_STALE_S + 0.3)
    status = client.get_status()
    assert status["is_running"] is True
    assert client.is_running() is True
    assert status["reconnecting"] is True
    assert status["reconnect_reason"] == "audio device lost (fake)"

    # A start while reconnecting is a no-op: the engine is still running and retrying itself.
    assert client.start() is True
    assert [c["cmd"] for c in read_log(command_log)].count("start") == 1

    client.stop()
    assert client.is_running() is False
    status = client.get_status()
    assert status["reconnecting"] is False
    assert status["reconnect_reason"] is None


def test_started_after_reconnecting_clears_the_state(make_client):
    client = make_client({"input_device": "__reconnect_recover__", "output_device": "", "monitor_device": "", "monitor_enabled": False})
    assert client.start() is True
    assert wait_for(lambda: client.get_status()["reconnecting"])
    assert wait_for(lambda: not client.get_status()["reconnecting"] and client.get_status()["input_rms"] == -30.0)
    status = client.get_status()
    assert status["is_running"] is True
    assert status["reconnect_reason"] is None


def test_virtual_device_is_reported_by_the_engine_devices_event(make_client, monkeypatch):
    monkeypatch.setenv("HUSHCORE_FAKE_VIRTUAL", "1")
    client = make_client()
    assert client.virtual_device() is None
    assert client._proc is None  # asking for the virtual device does not launch the engine

    assert client.start() is True
    device = client.virtual_device()
    assert device["installed"] is True
    assert device["provider"] == "windows-driver"
    assert device["sink"] == "HushCore Sink (fake)"
    assert device["microphone"] == "HushCore Microphone (fake)"


def test_virtual_device_not_installed_is_reported_as_such(make_client):
    client = make_client()
    assert client.start() is True
    assert client.virtual_device() == {
        "installed": False,
        "provider": "none",
        "sink": None,
        "microphone": None,
        "detail": "not installed",
    }


def test_get_devices_uses_sounddevice_listing_without_launching_engine(make_client, monkeypatch):
    listing = {"inputs": [{"name": "Mic [MME]"}], "outputs": [{"name": "Speakers [MME]"}]}
    monkeypatch.setattr(engine_client, "list_sounddevice_devices", lambda: listing)
    client = make_client()
    assert client.get_devices() == listing
    assert client._proc is None


def test_get_devices_falls_back_to_engine_when_sounddevice_is_missing(make_client, monkeypatch):
    def unavailable():
        raise OSError("PortAudio library not found")

    monkeypatch.setattr(engine_client, "list_sounddevice_devices", unavailable)
    client = make_client()
    devices = client.get_devices()
    assert [d["name"] for d in devices["inputs"]] == ["Fake Mic"]
    assert [d["name"] for d in devices["outputs"]] == ["Fake Speakers"]
    assert devices["inputs"][0]["id"] == 0
    assert devices["inputs"][0]["max_input_channels"] == 1
    client.shutdown()


def test_missing_sounddevice_engine_failure_raises(tmp_path, monkeypatch):
    def unavailable():
        raise OSError("PortAudio library not found")

    monkeypatch.setattr(engine_client, "list_sounddevice_devices", unavailable)
    client = NativeEngineClient({}, [str(tmp_path / "missing-hushcore-engine.exe")])
    with pytest.raises(RuntimeError):
        client.get_devices()
    client.shutdown()


def test_show_vst_editor_is_unsupported(make_client):
    client = make_client()
    with pytest.raises(RuntimeError):
        client.show_vst_editor()
    assert client.vst_plugin is None
