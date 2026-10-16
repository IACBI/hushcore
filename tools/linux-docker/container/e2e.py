#!/usr/bin/env python3
"""End-to-end test of the Linux PulseAudio-protocol backend, run inside the Docker test image.

The audio stack is headless: a null sink stands in for the physical microphone (its monitor is the
fake mic), and a pacat process feeds it a 440 Hz tone. The engine is driven over its JSON protocol
and the result is recorded from HushCore Microphone with parec.

Steps (any failure exits non-zero):
  1. Stream: devices, start, on-demand pair, record HushCore Microphone (RMS, frequency, xruns).
  2. Reconnect: unload the fake mic while running, expect reconnecting, reload, expect started.
  3. On-demand cleanup: quit, then the pair this process created is gone.
  4. Persistent config: install with scripts/linux, restart the server, virtual-device exits 0 with
     provider pipewire, no duplicate pair, record through the persistent pair, uninstall (twice).
  5. Selftest: `hushcore-engine selftest 10 shared` with the fake mic as the default source.
"""

import argparse
import json
import math
import os
import queue
import re
import subprocess
import sys
import threading
import time

import numpy as np

RATE = 48000
FAKE_SINK = "hush_fake"
FAKE_MIC = "hush_fake.monitor"
TONE_HZ = 440.0
TONE_AMPLITUDE = 0.5
RECORD_SECONDS = 3.0
SKIP_SECONDS = 0.5

results = []  # (name, passed, detail)


class Fail(Exception):
    pass


def check(cond, name, detail=""):
    results.append((name, bool(cond), detail))
    mark = "PASS" if cond else "FAIL"
    print(f"[{mark}] {name}" + (f": {detail}" if detail else ""), flush=True)
    if not cond:
        raise Fail(name)


def run(args, timeout=60, check_rc=True, binary=False):
    proc = subprocess.run(args, capture_output=True, text=not binary, timeout=timeout)
    if check_rc and proc.returncode != 0:
        raise Fail(f"{' '.join(args)} exited {proc.returncode}: {proc.stderr.strip()}")
    return proc


def pactl(*args):
    return run(["pactl", *args]).stdout


def short(kind):
    rows = []
    for line in pactl("list", "short", kind).splitlines():
        if line.strip():
            rows.append(line.split("\t"))
    return rows


def module_args(row):
    """Argument column of `pactl list short modules`; some modules have none."""
    return row[2] if len(row) > 2 else ""


def hushcore_modules():
    """Modules that create or remap the HushCore pair by name (on-demand or persistent null sink)."""
    return [row for row in short("modules") if "hushcore" in module_args(row).lower()]


def null_sink_module(name):
    for row in short("modules"):
        if len(row) > 2 and row[1] == "module-null-sink" and f"sink_name={name} " in module_args(row) + " ":
            return int(row[0])
    return None


def sink_names():
    return [row[1] for row in short("sinks") if len(row) > 1]


def source_names():
    return [row[1] for row in short("sources") if len(row) > 1]


def load_fake_mic():
    index = int(
        pactl(
            "load-module",
            "module-null-sink",
            f"sink_name={FAKE_SINK}",
            "sink_properties=device.description=FakeMicrophone",  # no spaces: plain value
            f"rate={RATE}",
            "channels=1",
            "format=float32le",
        ).strip()
    )
    return index


def start_fake_mic():
    """Loads the fake mic's null sink, then starts the tone feeder (pacat fails if the sink is missing)."""
    load_fake_mic()
    return Feeder()


def sine_feeder(proc):
    """Writes a 440 Hz tone into pacat's stdin. pacat blocks on the server, so this paces itself."""
    step = 2.0 * math.pi * TONE_HZ / RATE
    chunk = 480
    phase = 0.0
    try:
        while True:
            t = phase + step * np.arange(chunk)
            proc.stdin.write((TONE_AMPLITUDE * np.sin(t)).astype("<f4").tobytes())
            proc.stdin.flush()
            phase = (phase + step * chunk) % (2.0 * math.pi)
    except (BrokenPipeError, OSError, ValueError):
        return


class Feeder:
    def __init__(self):
        self.proc = subprocess.Popen(
            [
                "pacat",
                "--playback",
                f"--device={FAKE_SINK}",
                "--format=float32le",
                f"--rate={RATE}",
                "--channels=1",
                "--latency-msec=20",
                "--client-name=hushcore-fake-mic",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        threading.Thread(target=sine_feeder, args=(self.proc,), daemon=True).start()

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=10)


class Engine:
    """Speaks the JSON-lines protocol of engine/PROTOCOL.md. Events are timestamped on arrival."""

    def __init__(self, path, stderr):
        self.proc = subprocess.Popen(
            [path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
            bufsize=1,
        )
        self.events = queue.Queue()
        self.statuses = []  # (monotonic time, status dict)
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            now = time.monotonic()
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get("event") == "status":
                self.statuses.append((now, event))
            self.events.put((now, event))
        self.events.put((time.monotonic(), None))

    def send(self, command):
        self.proc.stdin.write(json.dumps(command) + "\n")
        self.proc.stdin.flush()

    def wait(self, name, timeout, predicate=None):
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise Fail(f"no '{name}' event within {timeout:.1f} s")
            try:
                _, event = self.events.get(timeout=remaining)
            except queue.Empty:
                continue
            if event is None:
                raise Fail(f"engine exited while waiting for '{name}'")
            if event.get("event") == name and (predicate is None or predicate(event)):
                return event

    def drain(self):
        while True:
            try:
                self.events.get_nowait()
            except queue.Empty:
                return

    def quit(self):
        try:
            self.send({"cmd": "quit"})
        except (BrokenPipeError, OSError):
            pass
        try:
            return self.proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            raise Fail("engine did not exit after quit")

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=10)


def start_command(input_name):
    return {"cmd": "start", "input": input_name, "output": "", "mode": "shared", "monitor_enabled": False}


def devices_event(engine):
    engine.drain()
    engine.send({"cmd": "devices"})
    return engine.wait("devices", 10)


def record_hushcore_mic(seconds):
    """Records HushCore Microphone with parec; returns the raw float32 samples."""
    proc = run(
        [
            "timeout",
            "-s",
            "INT",
            str(seconds),
            "parec",
            "--device=hushcore_mic",
            "--format=float32le",
            f"--rate={RATE}",
            "--channels=1",
            # Without this the first bytes arrive about 2 s late (parec's default record buffer).
            "--latency-msec=20",
            "--raw",
        ],
        timeout=seconds + 20,
        check_rc=False,
        binary=True,
    )
    usable = len(proc.stdout) // 4 * 4
    return np.frombuffer(proc.stdout[:usable], dtype="<f4").astype(np.float64)


def analyze(samples):
    """RMS in dBFS and the strongest frequency after a short settle period."""
    x = samples[int(SKIP_SECONDS * RATE) :]
    if x.size < RATE:
        raise Fail(f"recording too short: {x.size} samples")
    rms = float(np.sqrt(np.mean(x * x)))
    dbfs = 20.0 * math.log10(rms + 1e-12)
    window = np.hanning(x.size)
    spectrum = np.abs(np.fft.rfft(x * window))
    spectrum[0] = 0.0
    peak_hz = float(np.argmax(spectrum)) * RATE / x.size
    return dbfs, peak_hz


def steady_xruns(engine, started_at):
    """xruns counted after the first second of the session, from the status stream."""
    rows = [(t, s) for t, s in engine.statuses if t >= started_at + 1.0]
    if len(rows) < 2:
        raise Fail("too few status events after the first second")
    return int(rows[-1][1]["xruns"]) - int(rows[0][1]["xruns"]), len(rows)


def mean_cpu(engine, started_at):
    values = [s["cpu_load"] for t, s in engine.statuses if t >= started_at + 1.0]
    return sum(values) / len(values) if values else float("nan")


METRICS = {}


def expect_tone(samples, label):
    dbfs, peak = analyze(samples)
    METRICS[f"{label}_rms_dbfs"] = round(dbfs, 2)
    METRICS[f"{label}_peak_hz"] = round(peak, 2)
    check(dbfs > -30.0, f"{label}: RMS above -30 dBFS", f"{dbfs:.1f} dBFS")
    check(abs(peak - TONE_HZ) <= 5.0, f"{label}: dominant frequency 440 +/- 5 Hz", f"{peak:.2f} Hz")


def test_stream(args, engine_log, engines):
    engine = Engine(args.engine, engine_log)
    engines.append(engine)
    dev = devices_event(engine)
    check(not dev["virtual_device"]["installed"], "devices: no virtual pair before start")
    check(dev["virtual_device"]["provider"] == "none", "devices: provider none before start")

    engine.send(start_command(FAKE_MIC))
    started = engine.wait("started", 15)
    check(started["mode"] == "shared", "started: mode is shared", started["mode"])
    check(started["sample_rate"] == RATE, "started: sample rate 48 kHz", str(started["sample_rate"]))
    started_at = time.monotonic()
    METRICS["started_latency_ms"] = round(started["latency_ms"], 2)
    METRICS["input_period_ms"] = started["input_period_ms"]
    METRICS["output_period_ms"] = started["output_period_ms"]

    check(any(sink == "hushcore_sink" for sink in sink_names()), "on-demand: hushcore_sink created")
    check(any(mic == "hushcore_mic" for mic in source_names()), "on-demand: hushcore_mic created")

    dev = devices_event(engine)
    check(dev["virtual_device"]["installed"], "devices: virtual device installed after start")
    check(dev["virtual_device"]["provider"] == "on-demand", "devices: provider on-demand",
          dev["virtual_device"]["provider"])

    samples = record_hushcore_mic(RECORD_SECONDS)
    recent = [s for t, s in engine.statuses if t >= started_at]
    if recent:
        METRICS["engine_input_rms_dbfs_last"] = recent[-1]["input_rms"]
        METRICS["engine_output_rms_dbfs_last"] = recent[-1]["output_rms"]
        METRICS["engine_xruns_last"] = recent[-1]["xruns"]
    expect_tone(samples, "stream")
    xruns, rows = steady_xruns(engine, started_at)
    METRICS["steady_xruns"] = xruns
    METRICS["steady_status_events"] = rows
    METRICS["cpu_load_mean_pct"] = round(mean_cpu(engine, started_at), 2)
    check(xruns == 0, "steady state: xruns stay at 0 after the first second", f"{xruns} over {rows} status events")
    return engine


def test_reconnect(args, engine, feeder_holder):
    index = null_sink_module(FAKE_SINK)
    if index is None:
        raise Fail("fake mic module not found before reconnect test")
    feeder_holder[0].stop()
    pactl("unload-module", str(index))
    engine.wait("reconnecting", 5)
    check(True, "reconnect: reconnecting event after the fake mic was unloaded")

    feeder_holder[0] = start_fake_mic()  # the device is back, which the engine must notice
    t_reload = time.monotonic()
    engine.wait("started", 5)
    check(time.monotonic() - t_reload <= 5.0, "reconnect: started again within 5 s of reload",
          f"{time.monotonic() - t_reload:.2f} s")
    hushcore = [row for row in hushcore_modules() if "sink_name=hushcore_sink" in module_args(row)]
    check(len(hushcore) == 1, "reconnect: no duplicate hushcore_sink module", f"{len(hushcore)} present")
    samples = record_hushcore_mic(2.0)
    expect_tone(samples, "reconnect")


def test_on_demand_cleanup(engine):
    rc = engine.quit()
    check(rc == 0, "quit: engine exits 0", f"rc={rc}")
    check("hushcore_sink" not in sink_names(), "on-demand cleanup: hushcore_sink removed after quit")
    check("hushcore_mic" not in source_names(), "on-demand cleanup: hushcore_mic removed after quit")
    check(not any("sink_name=hushcore_sink" in module_args(row) or "source_name=hushcore_mic" in module_args(row)
                  for row in short("modules")), "on-demand cleanup: no HushCore module left behind")
    check(FAKE_SINK in sink_names(), "on-demand cleanup: fake mic untouched")


def xdg_drop_in():
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config")
    return os.path.join(base, "pipewire", "pipewire.conf.d", "90-hushcore-virtual-mic.conf")


def test_persistent(args, engines, feeder_holder):
    install = os.path.join(args.repo, "scripts", "linux", "install-virtual-mic.sh")
    uninstall = os.path.join(args.repo, "scripts", "linux", "uninstall-virtual-mic.sh")
    proc = run(["bash", install], timeout=120, check_rc=False)
    print(proc.stdout + proc.stderr, flush=True)
    check(proc.returncode == 0, "install: install-virtual-mic.sh exits 0", f"rc={proc.returncode}")
    check(os.path.isfile(xdg_drop_in()), "install: drop-in installed", xdg_drop_in())

    feeder_holder[0].stop()
    run(["bash", os.path.join(args.here, "audio-stack.sh"), "restart"], timeout=120)
    feeder_holder[0] = start_fake_mic()
    check("hushcore_sink" in sink_names(), "persistent: hushcore_sink provided by the PipeWire config")

    proc = run([args.engine, "virtual-device"], timeout=60, check_rc=False)
    status = json.loads(proc.stdout.strip().splitlines()[-1])
    check(proc.returncode == 0, "persistent: virtual-device exits 0", f"rc={proc.returncode}")
    check(status["provider"] == "pipewire" and status["installed"], "persistent: provider pipewire",
          status["provider"])
    check(len([n for n in sink_names() if n == "hushcore_sink"]) == 1, "persistent: exactly one hushcore_sink")
    check(not any("sink_name=hushcore_sink" in module_args(row) for row in short("modules")),
          "persistent: no on-demand module-null-sink created")

    engine = Engine(args.engine, args.engine_log)
    engines.append(engine)
    engine.send(start_command(FAKE_MIC))
    started = engine.wait("started", 15)
    started_at = time.monotonic()
    METRICS["persistent_latency_ms"] = round(started["latency_ms"], 2)
    check(len([n for n in sink_names() if n == "hushcore_sink"]) == 1, "persistent: still one hushcore_sink")
    check(not any("sink_name=hushcore_sink" in module_args(row) for row in short("modules")),
          "persistent: engine did not load a duplicate pair")
    samples = record_hushcore_mic(RECORD_SECONDS)
    expect_tone(samples, "persistent")
    xruns, rows = steady_xruns(engine, started_at)
    METRICS["persistent_steady_xruns"] = xruns
    check(xruns == 0, "persistent: steady state xruns 0", f"{xruns} over {rows} status events")
    rc = engine.quit()
    check(rc == 0, "persistent: engine quit exits 0", f"rc={rc}")
    check("hushcore_sink" in sink_names(), "persistent: config-provided pair survives engine exit")

    for attempt in (1, 2):
        proc = run(["bash", uninstall], timeout=120, check_rc=False)
        print(proc.stdout + proc.stderr, flush=True)
        check(proc.returncode == 0, f"uninstall: run {attempt} exits 0", f"rc={proc.returncode}")
    check(not os.path.exists(xdg_drop_in()), "uninstall: drop-in removed")
    feeder_holder[0].stop()
    run(["bash", os.path.join(args.here, "audio-stack.sh"), "restart"], timeout=120)
    feeder_holder[0] = start_fake_mic()
    check("hushcore_sink" not in sink_names() and "hushcore_mic" not in source_names(),
          "uninstall: no HushCore device after the audio restart")
    check(not any("hushcore" in module_args(row).lower() for row in short("modules")),
          "uninstall: no HushCore module left behind")


def test_selftest(args, feeder_holder):
    feeder_holder[0].stop()
    run(["bash", os.path.join(args.here, "audio-stack.sh"), "restart"], timeout=120)
    feeder_holder[0] = start_fake_mic()
    pactl("set-default-source", FAKE_MIC)
    proc = run([args.engine, "selftest", "10", "shared"], timeout=120, check_rc=False)
    print(proc.stdout + proc.stderr, flush=True)
    check(proc.returncode == 0, "selftest: exits 0", f"rc={proc.returncode}")
    line = next((l for l in proc.stdout.splitlines() if l.startswith("selftest: mode=")), "")
    check("mode=shared" in line, "selftest: reports mode shared")
    fields = dict(re.findall(r"(\w+)=([-\w.]+)", line))
    METRICS["selftest_latency_ms"] = fields.get("latency_ms")
    METRICS["selftest_cpu_load_avg_pct"] = fields.get("cpu_load_avg")
    METRICS["selftest_xruns_total"] = fields.get("xruns")
    check("xruns" in fields, "selftest: xruns reported")
    check(not any("hushcore" in n for n in sink_names()), "selftest: on-demand pair removed on exit")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", required=True)
    parser.add_argument("--repo", required=True)
    args = parser.parse_args()
    args.here = os.path.join(args.repo, "tools", "linux-docker", "container")
    log_dir = os.environ.get("HUSH_LOG_DIR", "/tmp/hush-logs")
    os.makedirs(log_dir, exist_ok=True)
    args.engine_log = open(os.path.join(log_dir, "engine-stderr.log"), "a")

    engines = []
    feeder_holder = [None]
    code = 0
    try:
        feeder_holder[0] = start_fake_mic()
        engine = test_stream(args, args.engine_log, engines)
        test_reconnect(args, engine, feeder_holder)
        test_on_demand_cleanup(engine)
        test_persistent(args, engines, feeder_holder)
        test_selftest(args, feeder_holder)
    except Fail as exc:
        print(f"E2E FAILED: {exc}", file=sys.stderr, flush=True)
        code = 1
    finally:
        for engine in engines:
            engine.kill()
        if feeder_holder[0] is not None:
            feeder_holder[0].stop()
    print("\nmetrics:", json.dumps(METRICS, indent=2), flush=True)
    passed = sum(1 for _, ok, _ in results if ok)
    print(f"checks: {passed}/{len(results)} passed", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
