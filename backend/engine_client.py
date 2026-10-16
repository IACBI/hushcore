"""Client for the native Rust audio engine.

The engine is a child process that speaks newline-delimited JSON (see engine/PROTOCOL.md).
NativeEngineClient exposes the same surface as audio_processor.AudioProcessor so that main.py
can run either engine behind one interface.
"""

import atexit
import json
import logging
import math
import subprocess
import sys
import threading
import time

logger = logging.getLogger("hushcore.engine")

START_TIMEOUT_S = 5.0
# After a non-fatal error during start, the engine may still announce started (exclusive falls back
# to shared). Without a started event within this grace the start is treated as failed.
START_ERROR_GRACE_S = 3.0
STOP_TIMEOUT_S = 1.0
DEVICES_TIMEOUT_S = 2.0
STATUS_STALE_S = 1.0
QUIT_GRACE_S = 1.0
TERMINATE_GRACE_S = 1.0

SILENCE_DB = -120.0
FFT_BINS = 64
DEFAULT_SAMPLE_RATE = 48000

_CREATE_NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def _finite_float(value, default):
    try:
        number = float(value)
    except (TypeError, ValueError):
        return default
    return number if math.isfinite(number) else default


def _ui_devices(engine_items, is_input):
    # Shape matches AudioProcessor.get_devices() entries; only used when sounddevice is unavailable.
    devices = []
    for idx, dev in enumerate(engine_items or []):
        name = str(dev.get("name", ""))
        devices.append({
            "id": idx,
            "name": name,
            "raw_name": name,
            "hostapi": "",
            "max_input_channels": 1 if is_input else 0,
            "max_output_channels": 0 if is_input else 1,
            "default_samplerate": _finite_float(dev.get("sample_rate"), DEFAULT_SAMPLE_RATE),
        })
    return devices


def portaudio_names_match_engine():
    """True where PortAudio lists the same endpoint names the native engine resolves.

    On Windows both see WASAPI endpoint names (the engine ignores the " [host API]" suffix). On
    macOS and Linux PortAudio lists Core Audio or ALSA names that the native backends cannot resolve.
    """
    return sys.platform == "win32"


def list_sounddevice_devices():
    """Same listing as AudioProcessor.get_devices(), so the UI sees identical device names."""
    import sounddevice as sd

    devices = sd.query_devices()
    hostapis = sd.query_hostapis()
    input_devs = []
    output_devs = []
    for idx, dev in enumerate(devices):
        api_name = hostapis[dev["hostapi"]]["name"]
        dev_info = {
            "id": idx,
            "name": f"{dev['name']} [{api_name}]",
            "raw_name": dev["name"],
            "hostapi": api_name,
            "max_input_channels": dev["max_input_channels"],
            "max_output_channels": dev["max_output_channels"],
            "default_samplerate": dev["default_samplerate"],
        }
        if dev["max_input_channels"] > 0:
            input_devs.append(dev_info)
        if dev["max_output_channels"] > 0:
            output_devs.append(dev_info)
    return {"inputs": input_devs, "outputs": output_devs}


class _RunningView:
    """Answers is_set() the way threading.Event does, for code that reads processor.running."""

    def __init__(self, client):
        self._client = client

    def is_set(self):
        return self._client.is_running()


class NativeEngineClient:
    engine_name = "native"
    df_available = False
    vst_plugin = None
    vst_failed = False

    def __init__(self, settings, command, *, start_timeout=START_TIMEOUT_S, start_error_grace=START_ERROR_GRACE_S):
        self.settings = settings
        self._command = list(command)
        self._start_timeout = start_timeout
        self._start_error_grace = start_error_grace

        # _op_lock serializes start/stop/shutdown. _write_lock keeps command lines whole.
        # _cond guards the state below and wakes waiters when the reader thread sees an event.
        self._op_lock = threading.RLock()
        self._write_lock = threading.Lock()
        self._cond = threading.Condition()

        self._proc = None
        self._eof = False
        self._running = False
        self._start_result = None
        self._stop_count = 0
        self._devices_reply = None
        self._last_error = None
        self._start_error = None
        self._start_error_at = None
        self._engine_notice = None
        self._started = None
        self._status = None
        self._status_at = 0.0
        self._xruns = 0
        self._reconnecting = False
        self._reconnect_reason = None
        self._virtual_device = None

        self.running = _RunningView(self)
        atexit.register(self.shutdown)

    # ----- public surface shared with AudioProcessor -------------------------------------

    def is_running(self):
        with self._cond:
            return self._is_running_locked()

    def start(self):
        with self._op_lock:
            with self._cond:
                running = self._running
                stale = running and not self._is_running_locked()
            if running and not stale:
                return True
            if stale:
                logger.warning("Native engine stopped reporting status; restarting it")
            try:
                return self._start_locked(restart_stale=stale)
            except Exception:
                logger.exception("Native engine start failed")
                self._teardown()
                return False

    def stop(self):
        with self._op_lock:
            with self._cond:
                if not self._running:
                    return
                proc = self._proc
                count = self._stop_count
            if not self._send({"cmd": "stop"}):
                self._teardown()
                return
            with self._cond:
                self._cond.wait_for(
                    lambda: self._stop_count != count or self._proc is not proc,
                    timeout=STOP_TIMEOUT_S,
                )
                confirmed = self._stop_count != count
                if not confirmed and self._proc is proc:
                    self._running = False
            if not confirmed:
                logger.warning("Native engine did not confirm stop; restarting its process")
                self._teardown()

    def update_settings(self, settings):
        self.settings = settings
        with self._cond:
            running = self._running
        if running:
            self._send({"cmd": "settings", "settings": dict(settings)})

    def start_monitoring(self, device_name):
        with self._cond:
            running = self._running
        if running:
            self._send({"cmd": "monitor", "enabled": True, "device": str(device_name or "")})

    def stop_monitoring(self):
        with self._cond:
            running = self._running
        if running:
            device = str(self.settings.get("monitor_device", "") or "")
            self._send({"cmd": "monitor", "enabled": False, "device": device})

    def get_devices(self):
        if portaudio_names_match_engine():
            try:
                return list_sounddevice_devices()
            except Exception as e:
                logger.info("sounddevice cannot list devices (%s); asking the native engine", e)
        return self._engine_devices()

    def get_status(self):
        with self._cond:
            running = self._is_running_locked()
            status = dict(self._status or {})
            started = dict(self._started or {})
            xruns = self._xruns
            notice = self._engine_notice
            reconnecting = self._reconnecting and running
            reconnect_reason = self._reconnect_reason if reconnecting else None

        sample_rate = status.get("sample_rate") or started.get("sample_rate") or DEFAULT_SAMPLE_RATE
        fft = status.get("fft_data") or [0.0] * FFT_BINS
        return {
            "input_rms": _finite_float(status.get("input_rms"), SILENCE_DB),
            "output_rms": _finite_float(status.get("output_rms"), SILENCE_DB),
            "gate_state": bool(status.get("gate_state", False)),
            "fft_data": [_finite_float(v, 0.0) for v in fft],
            "is_running": running,
            "df_available": False,
            "vst_loaded": False,
            "vst_failed": False,
            "sample_rate": int(_finite_float(sample_rate, DEFAULT_SAMPLE_RATE)),
            "latency_ms": _finite_float(status.get("latency_ms", started.get("latency_ms")), 0.0),
            "cpu_load": min(max(_finite_float(status.get("cpu_load"), 0.0), 0.0), 100.0),
            "clipping": bool(status.get("clipping", False)),
            "engine": "native",
            "xruns": int(xruns),
            "engine_mode": (started.get("mode") if running else None) or "shared",
            "engine_notice": notice,
            "reconnecting": reconnecting,
            "reconnect_reason": reconnect_reason,
        }

    def virtual_device(self):
        """Fresh virtual_device object from the engine while it runs, else the last one reported (or None).

        It is asked for again on every call, so installing or removing the device shows up without
        restarting the engine. A stopped engine is not launched just to answer.
        """
        with self._cond:
            alive = self._proc is not None and not self._eof
        if alive:
            try:
                self._engine_devices()
            except RuntimeError:
                pass
        with self._cond:
            return self._virtual_device

    def show_vst_editor(self):
        raise RuntimeError("No VST3 plugin loaded")

    def shutdown(self):
        with self._op_lock:
            self._teardown()

    # ----- process lifecycle -------------------------------------------------------------

    def _is_running_locked(self):
        # While the engine retries a lost device it sends no status events, but it is still running.
        if self._reconnecting:
            return self._running
        return self._running and (time.monotonic() - self._status_at) <= STATUS_STALE_S

    def _start_locked(self, restart_stale):
        if restart_stale:
            self._teardown()
        if not self._ensure_process():
            return False

        with self._cond:
            self._start_result = None
            self._start_error = None
            self._start_error_at = None
            self._engine_notice = None
        cfg = dict(self.settings)
        command = {
            "cmd": "start",
            "input": str(cfg.get("input_device", "") or ""),
            "output": str(cfg.get("output_device", "") or ""),
            "monitor": str(cfg.get("monitor_device", "") or ""),
            "monitor_enabled": bool(cfg.get("monitor_enabled", False)),
            "mode": "exclusive" if cfg.get("audio_mode") == "exclusive" else "shared",
            "settings": cfg,
        }
        if not self._send(command):
            self._teardown()
            return False

        ok, reason = self._await_start()
        if ok:
            return True
        logger.warning("Native engine did not start: %s", reason)
        self._teardown()
        return False

    def _await_start(self):
        deadline = time.monotonic() + self._start_timeout
        with self._cond:
            while True:
                if self._start_result is not None:
                    return self._start_result
                if self._eof:
                    return False, "engine exited before starting"
                now = time.monotonic()
                if self._start_error_at is not None and now - self._start_error_at >= self._start_error_grace:
                    return False, self._start_error
                if now >= deadline:
                    return False, f"no started event within {self._start_timeout:g} s"
                self._cond.wait(timeout=min(0.05, deadline - now))

    def _ensure_process(self):
        with self._cond:
            proc = self._proc
            usable = proc is not None and not self._eof and proc.poll() is None
        if usable:
            return True
        if proc is not None:
            self._teardown()
        return self._spawn()

    def _spawn(self):
        try:
            proc = subprocess.Popen(
                self._command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
                bufsize=1,
                creationflags=_CREATE_NO_WINDOW,
            )
        except OSError as e:
            logger.warning("Could not launch native engine %s: %s", self._command[0], e)
            return False

        with self._cond:
            self._proc = proc
            self._eof = False
            self._running = False
            self._reconnecting = False
            self._reconnect_reason = None
            self._start_result = None
            self._devices_reply = None
            self._last_error = None
            self._engine_notice = None
            self._status = None
            self._started = None
            self._status_at = 0.0

        threading.Thread(target=self._read_events, args=(proc,), name="hushcore-engine-reader", daemon=True).start()
        threading.Thread(target=self._drain_stderr, args=(proc,), name="hushcore-engine-stderr", daemon=True).start()
        logger.info("Launched native engine (pid %s)", proc.pid)
        return True

    def _teardown(self):
        with self._cond:
            proc, self._proc = self._proc, None
            self._running = False
            self._reconnecting = False
            self._reconnect_reason = None
            self._cond.notify_all()
        if proc is not None:
            self._quit_process(proc)

    def _quit_process(self, proc):
        if proc.poll() is None:
            self._write(proc, {"cmd": "quit"})
        self._close_stdin(proc)
        try:
            proc.wait(timeout=QUIT_GRACE_S)
            return
        except subprocess.TimeoutExpired:
            pass

        logger.warning("Native engine ignored quit; terminating it")
        try:
            proc.terminate()
            proc.wait(timeout=TERMINATE_GRACE_S)
            return
        except subprocess.TimeoutExpired:
            pass
        except OSError:
            return

        logger.warning("Native engine did not exit after terminate; killing it")
        try:
            proc.kill()
            proc.wait(timeout=TERMINATE_GRACE_S)
        except (OSError, subprocess.TimeoutExpired):
            logger.error("Native engine process could not be stopped")

    @staticmethod
    def _close_stdin(proc):
        try:
            proc.stdin.close()
        except (OSError, ValueError):
            pass

    # ----- commands ----------------------------------------------------------------------

    def _send(self, command):
        with self._cond:
            proc = self._proc
        if proc is None:
            return False
        return self._write(proc, command)

    def _write(self, proc, command):
        line = json.dumps(command, separators=(",", ":")) + "\n"
        try:
            with self._write_lock:
                proc.stdin.write(line)
                proc.stdin.flush()
            return True
        except (OSError, ValueError) as e:
            logger.warning("Could not send %s to native engine: %s", command.get("cmd"), e)
            return False

    def _engine_devices(self):
        with self._op_lock:
            if not self._ensure_process():
                raise RuntimeError("Native engine is not available")
            with self._cond:
                self._devices_reply = None
            if not self._send({"cmd": "devices"}):
                raise RuntimeError("Native engine did not accept the devices request")
            with self._cond:
                self._cond.wait_for(lambda: self._devices_reply is not None or self._eof, timeout=DEVICES_TIMEOUT_S)
                reply = self._devices_reply
        if reply is None:
            raise RuntimeError("Native engine did not report devices")
        return {
            "inputs": _ui_devices(reply.get("inputs"), is_input=True),
            "outputs": _ui_devices(reply.get("outputs"), is_input=False),
        }

    # ----- reader thread -----------------------------------------------------------------

    def _read_events(self, proc):
        try:
            for line in proc.stdout:
                self._handle_line(proc, line)
        except (OSError, ValueError):
            pass
        finally:
            try:
                proc.stdout.close()
            except OSError:
                pass

        with self._cond:
            current = self._proc is proc
            if current:
                self._eof = True
                self._running = False
                self._reconnecting = False
                self._cond.notify_all()
        if current:
            try:
                code = proc.wait(timeout=TERMINATE_GRACE_S)
            except subprocess.TimeoutExpired:
                code = None
            logger.warning("Native engine exited unexpectedly (exit code %s); it will restart on the next start", code)

    @staticmethod
    def _drain_stderr(proc):
        try:
            for line in proc.stderr:
                text = line.rstrip()
                if text:
                    logger.info("engine: %s", text)
        except (OSError, ValueError):
            pass
        finally:
            try:
                proc.stderr.close()
            except OSError:
                pass

    def _handle_line(self, proc, line):
        line = line.strip()
        if not line:
            return
        try:
            event = json.loads(line)
        except ValueError:
            logger.warning("Ignoring malformed line from native engine")
            return
        if not isinstance(event, dict):
            return

        kind = event.get("event")
        with self._cond:
            if self._proc is not proc:
                return
            if kind == "status":
                self._status = event
                self._status_at = time.monotonic()
                self._xruns = int(_finite_float(event.get("xruns"), 0))
            elif kind == "started":
                self._started = event
                self._status_at = time.monotonic()
                self._running = True
                self._reconnecting = False
                self._reconnect_reason = None
                self._start_result = (True, None)
                logger.info(
                    "Native engine started: %s Hz, mode %s, latency %s ms",
                    event.get("sample_rate"), event.get("mode"), event.get("latency_ms"),
                )
            elif kind == "reconnecting":
                # Not a stop: the engine keeps the session alive and retries the last start itself.
                reason = str(event.get("reason") or "audio device lost")
                if not self._reconnecting or self._reconnect_reason != reason:
                    logger.warning("Native engine reconnecting (attempt %s): %s", event.get("attempt"), reason)
                self._reconnecting = True
                self._reconnect_reason = reason
            elif kind == "stopped":
                self._running = False
                self._reconnecting = False
                self._reconnect_reason = None
                self._stop_count += 1
            elif kind == "devices":
                self._devices_reply = event
                virtual = event.get("virtual_device")
                if isinstance(virtual, dict):
                    self._virtual_device = virtual
            elif kind == "error":
                message = str(event.get("message", "unknown error"))
                fatal = bool(event.get("fatal", False))
                self._last_error = message
                if fatal:
                    if self._start_result is None:
                        self._start_result = (False, message)
                    self._running = False
                    logger.error("Native engine fatal error: %s", message)
                else:
                    self._engine_notice = message
                    if self._start_result is None:
                        self._start_error = message
                        self._start_error_at = time.monotonic()
                    logger.warning("Native engine notice: %s", message)
            elif kind == "ready":
                logger.info("Native engine ready (version %s)", event.get("version"))
            self._cond.notify_all()
