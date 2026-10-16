"""Stand-in for hushcore-engine that speaks engine/PROTOCOL.md without audio hardware.

Magic input device names select failure modes:
  __error__    start answers with a non-fatal error event
  __hang__     start never answers (exercises the start timeout)
  __crash__    start succeeds, then the process exits with code 3
  __silent__   start succeeds but no status events follow
  __garbage__  an unparsable line is written before started
  __xrun__     each status event increments xruns
  __deaf__     start succeeds, then the process ignores every command and EOF
  __exclusive_ok__  an exclusive start succeeds and reports mode "exclusive"
  __notice__   start succeeds and a non-fatal runtime error follows started

An exclusive start on any other device emits a non-fatal fallback error and then started with
mode "shared", like the real engine does when the device refuses exclusive mode.

Every command received is appended as one JSON line to $HUSHCORE_FAKE_ENGINE_LOG when that
variable is set, so tests can check what the backend sent.
"""

import json
import os
import sys
import threading
import time

LOG_PATH = os.environ.get("HUSHCORE_FAKE_ENGINE_LOG")

DEVICES = {
    "inputs": [{"name": "Fake Mic", "default": True, "min_period_ms": 5.0, "sample_rate": 48000}],
    "outputs": [{"name": "Fake Speakers", "default": True, "min_period_ms": 5.0, "sample_rate": 48000}],
}

_out_lock = threading.Lock()
_state = {"running": False, "silent": False, "xrun": False, "xruns": 0}


def _write(line):
    with _out_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def emit(event, **fields):
    _write(json.dumps({"event": event, **fields}))


def log_command(command):
    if LOG_PATH:
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(json.dumps(command) + "\n")


def status_loop():
    while True:
        time.sleep(1 / 30)
        if not _state["running"] or _state["silent"]:
            continue
        if _state["xrun"]:
            _state["xruns"] += 1
        emit(
            "status",
            input_rms=-30.0,
            output_rms=-33.5,
            gate_state=True,
            fft_data=[0.25] * 64,
            clipping=False,
            cpu_load=2.5,
            latency_ms=10.0,
            sample_rate=48000,
            xruns=_state["xruns"],
        )


def start(command):
    device = command.get("input", "")
    if device == "__error__":
        emit("error", message="no such input device", fatal=False)
        return
    if device == "__hang__":
        return
    if device == "__garbage__":
        _write("this is not json")
    mode = "shared"
    if command.get("mode") == "exclusive":
        if device == "__exclusive_ok__":
            mode = "exclusive"
        else:
            emit("error", message="exclusive mode unavailable (fake device); using shared mode", fatal=False)
    _state["running"] = True
    _state["silent"] = device == "__silent__"
    _state["xrun"] = device == "__xrun__"
    emit("started", sample_rate=48000, mode=mode, input_period_ms=5.0, output_period_ms=5.0, latency_ms=10.0)
    if device == "__notice__":
        emit("error", message="monitor output device lost", fatal=False)
    if device == "__crash__":
        threading.Timer(0.3, lambda: os._exit(3)).start()
    if device == "__deaf__":
        time.sleep(3600)


def main():
    threading.Thread(target=status_loop, daemon=True).start()
    emit("ready", version="fake-0.0.0")
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            command = json.loads(line)
        except ValueError:
            continue
        log_command(command)
        cmd = command.get("cmd")
        if cmd == "devices":
            emit("devices", **DEVICES)
        elif cmd == "start" and not _state["running"]:
            start(command)
        elif cmd == "stop":
            _state["running"] = False
            emit("stopped")
        elif cmd == "quit":
            break
    return 0


if __name__ == "__main__":
    sys.exit(main())
