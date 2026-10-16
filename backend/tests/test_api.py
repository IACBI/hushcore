import contextlib
import importlib
import json
import logging
import os
import pathlib
import re
import sys
import tempfile
import threading
import types

import pytest
from fastapi.testclient import TestClient
from starlette.websockets import WebSocketDisconnect

BACKEND_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if BACKEND_DIR not in sys.path:
    sys.path.insert(0, BACKEND_DIR)

STATUS_KEYS = {
    "input_rms", "output_rms", "gate_state", "fft_data", "is_running",
    "df_available", "vst_loaded", "vst_failed", "sample_rate",
    "latency_ms", "cpu_load", "clipping",
}
# The WebSocket frame adds the active engine name, its xrun count and the reconnect state to the engine's status.
WS_KEYS = STATUS_KEYS | {"engine", "xruns", "engine_mode", "engine_notice", "reconnecting", "reconnect_reason"}

FAKE_ENGINE = os.path.join(BACKEND_DIR, "tests", "fake_engine.py")


class FakeAudioProcessor:
    """Stands in for the real audio engine so tests need no audio hardware."""

    def __init__(self, settings):
        self.settings = settings
        self.running = threading.Event()
        self.calls = []
        self.vst_plugin = None
        self.vst_failed = False
        self.df_available = False
        self.input_rms = 0.0
        self.output_rms = 0.0
        self.gate_state = False
        self.fft_data = [0.0] * 64

    def get_status(self):
        return {key: 0 for key in STATUS_KEYS} | {"is_running": self.running.is_set()}

    def get_devices(self):
        return {"inputs": [], "outputs": []}

    def update_settings(self, settings):
        self.settings = settings
        self.calls.append("update_settings")

    def start(self):
        self.running.set()
        self.calls.append("start")
        return True

    def stop(self):
        self.running.clear()
        self.calls.append("stop")

    def start_monitoring(self, device):
        self.calls.append("start_monitoring")

    def stop_monitoring(self):
        self.calls.append("stop_monitoring")


_fake_module = types.ModuleType("audio_processor")
_fake_module.AudioProcessor = FakeAudioProcessor
sys.modules["audio_processor"] = _fake_module

import config  # noqa: E402
import presets  # noqa: E402
import main as main_module  # noqa: E402


@pytest.fixture
def tmp_path():
    # pytest's default temp root is not writable on some Windows profiles
    with tempfile.TemporaryDirectory(prefix="hushcore-test-") as directory:
        yield pathlib.Path(directory)


@pytest.fixture
def app_env(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "SETTINGS_FILE", str(tmp_path / "settings.json"))
    monkeypatch.setattr(presets, "USER_PRESETS_FILE", str(tmp_path / "user_presets.json"))
    # Tests run the Python stub unless they opt into the native engine, so a real
    # hushcore-engine.exe on this machine is never launched by accident.
    monkeypatch.setenv("HUSHCORE_ENGINE", "python")
    # Re-execute main so settings, processor and API_KEY start fresh per test
    module = importlib.reload(main_module)
    # Reload resets ALLOWED_HOSTS; TestClient always sends Host: testserver
    module.ALLOWED_HOSTS.add("testserver")
    return module


@pytest.fixture
def api(app_env):
    client = TestClient(app_env.app)
    client.headers.update({"X-API-Key": app_env.API_KEY})
    return client


@pytest.fixture
def native_api(app_env, monkeypatch):
    """App with the native engine available, backed by fake_engine.py instead of the Rust binary."""
    monkeypatch.setattr(app_env, "_native_command", lambda: [sys.executable, "-u", FAKE_ENGINE])
    yield app_env
    current = app_env.processor
    if app_env._engine_name(current) == "native":
        current.shutdown()


def test_api_requires_key(app_env):
    anon = TestClient(app_env.app)
    assert anon.get("/api/settings").status_code == 401
    assert anon.get("/api/settings", headers={"X-API-Key": "wrong"}).status_code == 401
    assert anon.get("/api/presets").status_code == 401
    assert anon.get("/api/nope").status_code == 401


def test_api_accepts_valid_key(app_env):
    anon = TestClient(app_env.app)
    resp = anon.get("/api/settings", headers={"X-API-Key": app_env.API_KEY})
    assert resp.status_code == 200


def test_health_is_public(app_env):
    anon = TestClient(app_env.app)
    resp = anon.get("/api/health")
    assert resp.status_code == 200
    assert resp.json() == {"status": "ok", "version": "0.0.99"}


def test_api_path_does_not_fall_through_to_index(api):
    assert api.get("/api/nope").status_code == 404


def test_numeric_setting_is_clamped(api, app_env):
    resp = api.post("/api/settings", json={"key": "input_gain", "value": 99})
    assert resp.status_code == 200
    assert resp.json()["settings"]["input_gain"] == 4.0
    assert app_env.settings["input_gain"] == 4.0


def test_numeric_string_is_coerced_and_clamped(api):
    resp = api.post("/api/settings", json={"key": "gate_threshold_db", "value": "-200"})
    assert resp.status_code == 200
    assert resp.json()["settings"]["gate_threshold_db"] == -80.0


@pytest.mark.parametrize("value", ["nan", "inf", "-inf", True, [1, 2]])
def test_non_finite_or_non_numeric_values_rejected(api, value):
    resp = api.post("/api/settings", json={"key": "exciter_mix", "value": value})
    # Lists fail the request model (422) before reaching the validator
    assert resp.status_code in (400, 422)


def test_unknown_key_rejected(api):
    resp = api.post("/api/settings", json={"key": "not_a_setting", "value": 1})
    assert resp.status_code == 400


def test_bad_enum_rejected_and_valid_enum_accepted(api):
    assert api.post("/api/settings", json={"key": "ns_mode", "value": "loud"}).status_code == 400
    assert api.post("/api/settings", json={"key": "ns_mode", "value": "high"}).status_code == 200
    resp = api.post("/api/settings", json={"key": "buffer_size", "value": 128})
    assert resp.status_code == 200
    assert resp.json()["settings"]["buffer_size"] == "128"
    assert api.post("/api/settings", json={"key": "buffer_size", "value": "1024"}).status_code == 400


def test_device_change_restarts_engine(api, app_env):
    resp = api.post("/api/settings", json={"key": "input_device", "value": "Mic"})
    assert resp.status_code == 200
    assert app_env.processor.calls == ["stop", "update_settings", "start"]


def test_preset_apply_does_not_restart_engine(api, app_env):
    resp = api.post("/api/presets/apply", json={"name": "broadcaster"})
    assert resp.status_code == 200
    assert "stop" not in app_env.processor.calls
    assert "start" not in app_env.processor.calls
    assert resp.json()["settings"]["eq_low_gain_db"] == 4.5
    assert resp.json()["settings"]["input_device"] == ""


def test_builtin_presets_listed(api):
    body = api.get("/api/presets").json()
    assert set(body["builtin"]) == {"off", "voice", "gaming", "broadcaster"}
    assert body["user"] == {}


def test_user_preset_save_apply_delete(api, app_env):
    api.post("/api/settings", json={"key": "gate_threshold_db", "value": -33.0})
    api.post("/api/settings", json={"key": "input_gain", "value": 2.0})

    resp = api.post("/api/presets", json={"name": "My Mix_1"})
    assert resp.status_code == 200
    saved = presets.get_user_presets()["My Mix_1"]
    assert saved["gate_threshold_db"] == -33.0
    assert "input_gain" not in saved
    assert "input_device" not in saved

    api.post("/api/presets/apply", json={"name": "off"})
    assert app_env.settings["gate_enabled"] is False

    resp = api.post("/api/presets/apply", json={"name": "My Mix_1"})
    assert resp.status_code == 200
    assert app_env.settings["gate_threshold_db"] == -33.0
    assert app_env.settings["input_gain"] == 2.0

    assert "My Mix_1" in api.get("/api/presets").json()["user"]
    assert api.delete("/api/presets/My Mix_1").status_code == 200
    assert api.delete("/api/presets/My Mix_1").status_code == 404
    assert api.post("/api/presets/apply", json={"name": "My Mix_1"}).status_code == 404


@pytest.mark.parametrize("name", ["", "   ", "Voice", "gaming ", "a/b", "x" * 33, "dot.name"])
def test_invalid_preset_names_rejected(api, name):
    assert api.post("/api/presets", json={"name": name}).status_code == 400


def test_delete_builtin_preset_is_not_found(api):
    assert api.delete("/api/presets/voice").status_code == 404


def test_settings_round_trip_is_atomic(app_env, tmp_path):
    data = config.load_settings()
    data["compressor_ratio"] = 7.5
    data["ns_mode"] = "high"
    assert config.save_settings(data)

    assert config.load_settings() == data
    with open(config.SETTINGS_FILE, "r", encoding="utf-8") as f:
        assert json.load(f)["compressor_ratio"] == 7.5
    leftovers = [name for name in os.listdir(tmp_path) if name.startswith(".tmp-")]
    assert leftovers == []


def test_hand_edited_settings_are_sanitized(tmp_path, monkeypatch):
    path = tmp_path / "settings.json"
    path.write_text(
        '{"input_gain": 50, "gate_threshold_db": NaN, "ns_mode": "loud",'
        ' "exciter_frequency": 100, "bogus_key": 1, "gate_enabled": "no"}',
        encoding="utf-8",
    )
    monkeypatch.setattr(config, "SETTINGS_FILE", str(path))
    loaded = config.load_settings()
    defaults = config.DEFAULT_SETTINGS
    assert loaded["input_gain"] == 4.0
    assert loaded["gate_threshold_db"] == defaults["gate_threshold_db"]
    assert loaded["ns_mode"] == defaults["ns_mode"]
    assert loaded["exciter_frequency"] == 1000.0
    assert loaded["gate_enabled"] is False
    assert "bogus_key" not in loaded


def test_corrupt_settings_file_falls_back_to_defaults(tmp_path, monkeypatch):
    path = tmp_path / "settings.json"
    path.write_text("{not json", encoding="utf-8")
    monkeypatch.setattr(config, "SETTINGS_FILE", str(path))
    assert config.load_settings() == config.DEFAULT_SETTINGS


def _auth_message(token):
    return json.dumps({"action": "auth", "token": token})


def test_websocket_without_auth_message_is_closed(app_env, monkeypatch):
    monkeypatch.setattr(app_env, "AUTH_TIMEOUT_S", 0.2)
    anon = TestClient(app_env.app)
    with anon.websocket_connect("/ws") as ws:
        with pytest.raises(WebSocketDisconnect) as exc:
            ws.receive_text()
    assert exc.value.code == 4001


def test_websocket_wrong_token_is_closed_with_4001(app_env):
    anon = TestClient(app_env.app)
    with anon.websocket_connect("/ws") as ws:
        ws.send_text(_auth_message("wrong"))
        with pytest.raises(WebSocketDisconnect) as exc:
            ws.receive_text()
    assert exc.value.code == 4001


def test_websocket_rejects_non_auth_first_message(app_env):
    anon = TestClient(app_env.app)
    with anon.websocket_connect("/ws") as ws:
        ws.send_text(json.dumps({"action": "update_setting", "key": "gate_release_ms", "value": 5000}))
        with pytest.raises(WebSocketDisconnect) as exc:
            ws.receive_text()
    assert exc.value.code == 4001


def test_websocket_key_in_query_string_is_ignored(app_env, monkeypatch):
    monkeypatch.setattr(app_env, "AUTH_TIMEOUT_S", 0.2)
    anon = TestClient(app_env.app)
    with anon.websocket_connect(f"/ws?token={app_env.API_KEY}") as ws:
        with pytest.raises(WebSocketDisconnect) as exc:
            ws.receive_text()
    assert exc.value.code == 4001


def test_websocket_auth_ok_then_metrics_frame(api, app_env):
    with api.websocket_connect("/ws") as ws:
        ws.send_text(_auth_message(app_env.API_KEY))
        assert json.loads(ws.receive_text()) == {"action": "auth_ok"}
        metrics = json.loads(ws.receive_text())
        assert set(metrics) == WS_KEYS
        assert metrics["engine"] == "python"
        assert metrics["xruns"] == 0
        assert metrics["engine_mode"] == "shared"
        assert metrics["engine_notice"] is None


def test_websocket_survives_bad_messages_and_applies_settings(api, app_env):
    with api.websocket_connect("/ws") as ws:
        ws.send_text(_auth_message(app_env.API_KEY))
        assert json.loads(ws.receive_text()) == {"action": "auth_ok"}

        ws.send_text("{not json")
        ws.send_text("[1, 2, 3]")
        ws.send_text(json.dumps({"action": "unknown"}))
        ws.send_text(json.dumps({"action": "update_setting", "key": "gate_release_ms", "value": 5000}))

        metrics = json.loads(ws.receive_text())
        assert set(metrics) == WS_KEYS

        for _ in range(500):
            if app_env.settings["gate_release_ms"] == 1000.0:
                break
            json.loads(ws.receive_text())
        assert app_env.settings["gate_release_ms"] == 1000.0


def test_foreign_host_header_is_forbidden(app_env):
    foreign = TestClient(app_env.app, base_url="http://evil.example:8000")
    assert foreign.get("/api/health").status_code == 403


def test_loopback_host_header_is_allowed(app_env):
    loopback = TestClient(app_env.app, base_url=f"http://127.0.0.1:{app_env.PORT}")
    assert loopback.get("/api/health").status_code == 200


def test_foreign_origin_is_forbidden(app_env):
    anon = TestClient(app_env.app)
    resp = anon.get("/api/health", headers={"Origin": "http://evil.example"})
    assert resp.status_code == 403


def test_foreign_origin_websocket_is_refused(app_env):
    anon = TestClient(app_env.app)
    with pytest.raises(WebSocketDisconnect):
        with anon.websocket_connect("/ws", headers={"Origin": "http://evil.example"}):
            pass


def test_index_is_no_store_and_injects_key(app_env, tmp_path, monkeypatch):
    (tmp_path / "index.html").write_text("<html><head></head><body></body></html>", encoding="utf-8")
    monkeypatch.setattr(app_env, "static_path", str(tmp_path))
    anon = TestClient(app_env.app)
    resp = anon.get("/")
    assert resp.status_code == 200
    assert resp.headers["cache-control"] == "no-store"
    assert f'window.HUSHCORE_API_KEY = "{app_env.API_KEY}";' in resp.text


def test_engine_endpoint_reports_python_without_native_binary(api, app_env, monkeypatch):
    monkeypatch.setattr(app_env, "_native_command", lambda: None)
    assert api.get("/api/engine").json() == {
        "active": "python", "mode": "python", "available": ["python"], "native_path": None,
    }


def test_engine_post_rejects_unknown_choice(api):
    assert api.post("/api/engine", json={"engine": "turbo"}).status_code == 400


def test_engine_post_native_without_binary_is_rejected(api, app_env, monkeypatch):
    monkeypatch.setattr(app_env, "_native_command", lambda: None)
    assert api.post("/api/engine", json={"engine": "native"}).status_code == 400
    assert api.get("/api/engine").json()["active"] == "python"


def test_engine_switch_while_stopped_and_websocket_reports_engine(native_api, api, app_env):
    resp = api.post("/api/engine", json={"engine": "native"})
    assert resp.status_code == 200
    assert resp.json()["active"] == "native"

    body = api.get("/api/engine").json()
    assert body["active"] == "native"
    assert body["mode"] == "native"
    assert body["available"] == ["native", "python"]
    assert body["native_path"] == sys.executable

    assert api.post("/api/start").status_code == 200
    with api.websocket_connect("/ws") as ws:
        ws.send_text(_auth_message(app_env.API_KEY))
        assert json.loads(ws.receive_text()) == {"action": "auth_ok"}
        metrics = json.loads(ws.receive_text())
    assert set(metrics) == WS_KEYS
    assert metrics["engine"] == "native"
    assert metrics["is_running"] is True
    assert metrics["xruns"] == 0

    assert api.post("/api/stop").status_code == 200
    back = api.post("/api/engine", json={"engine": "python"}).json()
    assert back["active"] == "python"
    assert api.get("/api/engine").json()["mode"] == "python"


def test_native_start_failure_falls_back_to_python(native_api, api, app_env, caplog):
    assert api.post("/api/engine", json={"engine": "native"}).json()["active"] == "native"
    with caplog.at_level(logging.WARNING):
        resp = api.post("/api/settings", json={"key": "input_device", "value": "__error__"})
    assert resp.status_code == 200
    assert api.get("/api/engine").json()["active"] == "python"
    assert "falling back to the Python engine" in caplog.text


def test_enabling_vst_moves_running_native_engine_to_python_and_back(native_api, api, app_env):
    assert api.post("/api/engine", json={"engine": "native"}).json()["active"] == "native"
    assert api.post("/api/start").status_code == 200
    native = app_env.processor
    assert native.is_running() is True

    assert api.post("/api/settings", json={"key": "vst_enabled", "value": True}).status_code == 200
    assert api.get("/api/engine").json()["active"] == "python"
    assert native.is_running() is False
    assert app_env.processor.running.is_set() is True

    assert api.post("/api/settings", json={"key": "vst_enabled", "value": False}).status_code == 200
    assert api.get("/api/engine").json()["active"] == "native"
    assert app_env.processor.is_running() is True


def test_audio_mode_defaults_to_shared(api):
    assert api.get("/api/settings").json()["audio_mode"] == "shared"


def test_audio_mode_accepts_only_shared_or_exclusive(api):
    assert api.post("/api/settings", json={"key": "audio_mode", "value": "turbo"}).status_code == 400
    assert api.post("/api/settings", json={"key": "audio_mode", "value": 1}).status_code == 400
    resp = api.post("/api/settings", json={"key": "audio_mode", "value": "exclusive"})
    assert resp.status_code == 200
    assert resp.json()["settings"]["audio_mode"] == "exclusive"


def test_audio_mode_is_not_carried_by_presets(app_env):
    assert "audio_mode" not in presets.DSP_KEYS
    assert "audio_mode" not in presets.snapshot_dsp(app_env.settings)


def test_audio_mode_change_while_stopped_does_not_start_engine(api, app_env):
    assert api.post("/api/settings", json={"key": "audio_mode", "value": "exclusive"}).status_code == 200
    assert "start" not in app_env.processor.calls


def test_audio_mode_change_while_running_restarts_pipeline(api, app_env):
    assert api.post("/api/start").status_code == 200
    app_env.processor.calls.clear()
    assert api.post("/api/settings", json={"key": "audio_mode", "value": "exclusive"}).status_code == 200
    assert app_env.processor.calls == ["stop", "update_settings", "start"]


def test_native_exclusive_fallback_is_reported_on_websocket(native_api, api, app_env):
    assert api.post("/api/settings", json={"key": "audio_mode", "value": "exclusive"}).status_code == 200
    assert api.post("/api/engine", json={"engine": "native"}).json()["active"] == "native"
    assert api.post("/api/start").status_code == 200
    with api.websocket_connect("/ws") as ws:
        ws.send_text(_auth_message(app_env.API_KEY))
        assert json.loads(ws.receive_text()) == {"action": "auth_ok"}
        metrics = json.loads(ws.receive_text())
    assert set(metrics) == WS_KEYS
    assert metrics["engine"] == "native"
    assert metrics["engine_mode"] == "shared"
    assert "using shared mode" in metrics["engine_notice"]
    assert api.post("/api/stop").status_code == 200


def test_virtual_device_endpoint_requires_key(app_env):
    anon = TestClient(app_env.app)
    assert anon.get("/api/virtual-device").status_code == 401


def test_virtual_device_python_source_reports_installed_from_device_list(api, app_env, monkeypatch):
    listing = {
        "inputs": [{"name": "Mic [MME]"}, {"name": "HushCore Microphone [Windows WASAPI]"}],
        "outputs": [{"name": "Speakers [MME]"}, {"name": "HushCore Sink [Windows WASAPI]"}],
    }
    monkeypatch.setattr(app_env.processor, "get_devices", lambda: listing)

    body = api.get("/api/virtual-device").json()
    assert body["installed"] is True
    assert body["provider"] == body["platform"]
    assert body["sink"] == "HushCore Sink [Windows WASAPI]"
    assert body["microphone"] == "HushCore Microphone [Windows WASAPI]"
    assert set(body) == {
        "installed", "provider", "sink", "microphone", "detail", "platform", "install_command", "uninstall_command",
    }

    devices = api.get("/api/devices").json()
    assert devices["inputs"] == listing["inputs"]
    assert devices["virtual_device"] == body


def test_virtual_device_missing_reports_none_and_install_command(api, app_env, monkeypatch):
    monkeypatch.setattr(app_env.processor, "get_devices", lambda: {"inputs": [{"name": "Mic"}], "outputs": [{"name": "Speakers"}]})
    body = api.get("/api/virtual-device").json()
    assert body["installed"] is False
    assert body["provider"] == "none"
    assert body["sink"] is None and body["microphone"] is None
    assert body["platform"] == app_env.virtual_device.platform_name()
    assert "install" in body["install_command"]
    assert "uninstall" in body["uninstall_command"]


def test_virtual_device_native_source_is_the_engine_report(native_api, api, app_env, monkeypatch):
    monkeypatch.setenv("HUSHCORE_FAKE_VIRTUAL", "1")
    assert api.post("/api/engine", json={"engine": "native"}).json()["active"] == "native"
    assert api.post("/api/start").status_code == 200
    try:
        body = api.get("/api/virtual-device").json()
    finally:
        api.post("/api/stop")
    # The fake engine reports provider "windows-driver"; the Python fallback would say the platform name.
    assert body["installed"] is True
    assert body["provider"] == "windows-driver"
    assert body["sink"] == "HushCore Sink (fake)"
    assert body["microphone"] == "HushCore Microphone (fake)"


def test_reconnecting_fields_reach_the_websocket_frame(api, app_env):
    assert api.post("/api/start").status_code == 200
    with api.websocket_connect("/ws") as ws:
        ws.send_text(_auth_message(app_env.API_KEY))
        assert json.loads(ws.receive_text()) == {"action": "auth_ok"}
        metrics = json.loads(ws.receive_text())
    assert set(metrics) == WS_KEYS
    assert metrics["reconnecting"] is False
    assert metrics["reconnect_reason"] is None


@pytest.mark.parametrize(("platform", "exe_name"), [
    ("win32", "hushcore-engine.exe"),
    ("linux", "hushcore-engine"),
    ("darwin", "hushcore-engine"),
])
def test_native_binary_name_follows_the_platform(app_env, tmp_path, monkeypatch, platform, exe_name):
    monkeypatch.setattr(sys, "platform", platform)
    assert app_env._native_exe_name() == exe_name
    assert all(os.path.basename(path) == exe_name for path in app_env._native_candidates())

    binary = tmp_path / exe_name
    binary.write_bytes(b"")
    monkeypatch.setattr(app_env, "_native_candidates", lambda: (str(binary),))
    assert app_env._native_command() == [str(binary)]


SECURITY_HEADER_VALUES = {
    "x-content-type-options": "nosniff",
    "cross-origin-resource-policy": "same-origin",
    "referrer-policy": "no-referrer",
}


def test_security_headers_are_sent_on_api_and_auth_errors(app_env):
    anon = TestClient(app_env.app)
    for resp in (anon.get("/api/health"), anon.get("/api/settings")):
        for name, value in SECURITY_HEADER_VALUES.items():
            assert resp.headers[name] == value


def test_index_has_csp_with_a_nonce_for_the_injected_key_script(app_env, tmp_path, monkeypatch):
    (tmp_path / "index.html").write_text("<html><head></head><body></body></html>", encoding="utf-8")
    monkeypatch.setattr(app_env, "static_path", str(tmp_path))
    resp = TestClient(app_env.app).get("/")
    csp = resp.headers["content-security-policy"]
    assert "default-src 'self'" in csp
    assert "connect-src 'self' ws://127.0.0.1:* ws://localhost:*" in csp
    assert "object-src 'none'" in csp
    nonce = re.search(r"script-src 'self' 'nonce-([A-Za-z0-9_-]+)'", csp).group(1)
    assert f'<script nonce="{nonce}">window.HUSHCORE_API_KEY' in resp.text


def test_built_dashboard_loads_under_the_csp(app_env):
    if not os.path.exists(os.path.join(app_env.static_path, "index.html")):
        pytest.skip("dashboard not built (run npm run build in frontend)")
    client = TestClient(app_env.app)
    resp = client.get("/")
    assert resp.status_code == 200
    csp = resp.headers["content-security-policy"]
    nonce = re.search(r"'nonce-([A-Za-z0-9_-]+)'", csp).group(1)

    # Every inline script in the page must carry the nonce; external scripts come from 'self'.
    for tag in re.findall(r"<script\b[^>]*>", resp.text):
        if " src=" not in tag:
            assert f'nonce="{nonce}"' in tag, tag

    assets = re.findall(r'(?:src|href)="(/assets/[^"]+)"', resp.text)
    assert any(a.endswith(".js") for a in assets)
    assert any(a.endswith(".css") for a in assets)
    for path in assets:
        asset = client.get(path)
        assert asset.status_code == 200, path
        assert asset.headers["x-content-type-options"] == "nosniff"


def test_unauthenticated_websockets_are_capped_with_1013(app_env):
    client = TestClient(app_env.app)
    with contextlib.ExitStack() as held:
        for _ in range(app_env.WS_UNAUTHENTICATED_LIMIT):
            held.enter_context(client.websocket_connect("/ws"))
        with pytest.raises(WebSocketDisconnect) as excinfo:
            with client.websocket_connect("/ws") as ws:
                ws.receive_text()
        assert excinfo.value.code == 1013
