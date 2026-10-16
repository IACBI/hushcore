import contextlib
import importlib.util
import json
import os
import sys
import time

import numpy as np
import pytest
import scipy.signal as signal

BACKEND_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if BACKEND_DIR not in sys.path:
    sys.path.insert(0, BACKEND_DIR)

# Loaded under its own name: test_api.py replaces sys.modules["audio_processor"] with a stub.
_spec = importlib.util.spec_from_file_location(
    "hushcore_audio_processor", os.path.join(BACKEND_DIR, "audio_processor.py")
)
ap = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ap)

AudioProcessor = ap.AudioProcessor
DynamicsCompressor = ap.DynamicsCompressor
Equalizer3Band = ap.Equalizer3Band
Limiter = ap.Limiter
NoiseGate = ap.NoiseGate
RingBuffer = ap.RingBuffer
SpectralSubtractedNoiseSuppression = ap.SpectralSubtractedNoiseSuppression
VocalDeEsser = ap.VocalDeEsser
VocalExciter = ap.VocalExciter

from config import DEFAULT_SETTINGS  # noqa: E402

STATUS_KEYS = {
    "input_rms", "output_rms", "gate_state", "fft_data", "is_running", "df_available",
    "vst_loaded", "vst_failed", "sample_rate", "latency_ms", "cpu_load", "clipping",
}


def sine(freq, amp, sr, n_samples):
    t = np.arange(n_samples) / sr
    return (amp * np.sin(2.0 * np.pi * freq * t)).astype(np.float32)


def frames_of(x, n):
    return [x[i:i + n] for i in range(0, len(x) - n + 1, n)]


def rms(x):
    return float(np.sqrt(np.mean(np.square(x, dtype=np.float64))))


def eco_hop(sr):
    block = int(sr * 0.01)
    return (block + 1) // 2


def eco_run(sr, x, strength):
    hop = eco_hop(sr)
    eco = SpectralSubtractedNoiseSuppression(block_size=2 * hop, hop_size=hop, sr=sr)
    n = len(x) // hop * hop
    return np.concatenate([eco.process(x[i:i + hop], strength) for i in range(0, n, hop)])


@pytest.fixture
def no_deepfilter(monkeypatch):
    monkeypatch.setattr(AudioProcessor, "_init_deep_filter", lambda self: None)


@pytest.fixture
def processor(no_deepfilter):
    return AudioProcessor(dict(DEFAULT_SETTINGS))


def test_ring_buffer_drops_oldest_on_overflow():
    rb = RingBuffer(4)
    rb.write(np.array([0, 1, 2], dtype=np.float32))
    rb.write(np.array([3, 4, 5], dtype=np.float32))
    assert rb.available == 4
    assert np.array_equal(rb.read(4), np.array([2, 3, 4, 5], dtype=np.float32))


def test_ring_buffer_wraps_and_zero_fills_underrun():
    rb = RingBuffer(5)
    rb.write(np.array([0, 1, 2, 3], dtype=np.float32))
    assert np.array_equal(rb.read(2), np.array([0, 1], dtype=np.float32))
    rb.write(np.array([4, 5, 6], dtype=np.float32))
    assert np.array_equal(rb.read(5), np.array([2, 3, 4, 5, 6], dtype=np.float32))
    out = np.ones(3, dtype=np.float32)
    assert rb.read_into(out) == 0
    assert np.array_equal(out, np.zeros(3, dtype=np.float32))


@pytest.mark.parametrize("sr", [44100, 48000])
def test_eq_zero_gain_is_identity(sr):
    eq = Equalizer3Band(sr=sr)
    eq.update_gains(0.0, 0.0, 0.0)
    x = np.random.default_rng(0).uniform(-0.5, 0.5, 4800).astype(np.float32)
    assert np.max(np.abs(eq.process(x) - x)) < 1e-5


def test_high_shelf_has_unity_dc_and_full_boost_at_high_frequency():
    sr = 48000
    for gain_db in (6.0, -6.0):
        b, a = ap._high_shelf(sr, 4000.0, 0.707, gain_db)
        _, h = signal.freqz(b, a, worN=[1.0, 23000.0], fs=sr)
        db = 20.0 * np.log10(np.abs(h))
        assert abs(db[0]) < 0.1
        assert db[1] == pytest.approx(gain_db, abs=0.5)


def test_limiter_never_exceeds_threshold_on_loud_sine():
    sr, block = 48000, 480
    threshold = 10.0 ** (-1.0 / 20.0)
    lim = Limiter(sr, block)
    x = sine(1000.0, 1.0, sr, block * 200)
    peaks = [float(np.max(np.abs(lim.process(f, threshold)))) for f in frames_of(x, block)]
    assert max(peaks) <= threshold + 1e-4
    assert max(peaks) > 0.9 * threshold


def test_limiter_recovers_in_about_its_release_time():
    lim = Limiter(48000, 480, release_ms=100.0)
    lim.process(np.full(480, 1.0, dtype=np.float32), 0.5)
    assert lim.gain == pytest.approx(0.5)
    quiet = np.full(480, 0.1, dtype=np.float32)
    for _ in range(10):  # 100 ms
        lim.process(quiet, 0.5)
    assert 0.75 < lim.gain < 0.9


def test_gate_closes_on_silence_and_opens_on_loud_sine():
    sr, block = 48000, 480
    gate = NoiseGate(sr, block)
    silence = np.zeros(block, dtype=np.float32)
    for _ in range(50):
        gate.process(silence, -45.0, 150.0)
    assert not gate.is_open

    loud = sine(1000.0, 0.5, sr, block)
    out = gate.process(loud, -45.0, 150.0)
    assert gate.is_open
    assert rms(out) > 0.9 * rms(loud)


def test_gate_holds_open_for_20ms_after_signal_drops():
    sr, block = 48000, 480
    gate = NoiseGate(sr, block)
    gate.process(sine(1000.0, 0.5, sr, block), -45.0, 150.0)
    silence = np.zeros(block, dtype=np.float32)
    states = []
    for _ in range(3):
        gate.process(silence, -45.0, 150.0)
        states.append(gate.is_open)
    assert states == [True, True, False]


def test_compressor_reduces_loud_tone():
    sr, block = 48000, 480
    comp = DynamicsCompressor(sr, block)
    frames = frames_of(sine(1000.0, 0.5, sr, block * 100), block)
    outs = [comp.process(f) for f in frames]
    reduction_db = 20.0 * np.log10(rms(outs[-1]) / rms(frames[-1]))
    assert reduction_db < -3.0


def test_deesser_bypass_is_exact_passthrough_without_sibilance():
    sr, block = 48000, 480
    de = VocalDeEsser(sr, block)
    de.update_params(-25.0, 0.8)
    x = sine(200.0, 0.3, sr, block)
    assert np.array_equal(de.process(x), x)


@pytest.mark.parametrize("sr", [44100, 48000])
def test_modules_preserve_frame_length(sr):
    block = int(sr * 0.01)
    hop = (block + 1) // 2
    rng = np.random.default_rng(1)

    eco = SpectralSubtractedNoiseSuppression(block_size=2 * hop, hop_size=hop, sr=sr)
    eq = Equalizer3Band(sr)
    eq.update_gains(3.0, -2.0, 4.0)
    comp = DynamicsCompressor(sr, block)
    gate = NoiseGate(sr, block)
    deesser = VocalDeEsser(sr, block)
    deesser.update_params(-25.0, 0.8)
    exciter = VocalExciter(sr)
    exciter.update_params(3000.0, 0.5, 0.5)
    lim = Limiter(sr, block)

    for _ in range(5):
        x = rng.uniform(-0.3, 0.3, block).astype(np.float32)
        assert len(eco.process(x[:hop])) == hop
        stages = {
            "eq": eq.process(x),
            "compressor": comp.process(x),
            "gate": gate.process(x, -45.0, 150.0),
            "deesser": deesser.process(x),
            "exciter": exciter.process(x),
            "limiter": lim.process(x, 0.9),
        }
        for name, out in stages.items():
            assert len(out) == block, name
            assert out.dtype == np.float32, name


@pytest.mark.parametrize("sr", [44100, 48000])
@pytest.mark.parametrize("ns_mode", ["off", "eco", "high"])
def test_processor_frame_length_and_finite(processor, sr, ns_mode):
    processor._configure(sr)
    processor.update_settings(dict(
        DEFAULT_SETTINGS, ns_mode=ns_mode, deesser_enabled=True, exciter_enabled=True,
        eq_low_gain_db=3.0, eq_high_gain_db=-4.0,
    ))
    rng = np.random.default_rng(2)
    for _ in range(5):
        frame = rng.uniform(-0.2, 0.2, processor.block_size).astype(np.float32)
        out = processor._process_frame(frame)
        assert out.shape == (processor.block_size,)
        assert out.dtype == np.float32
        assert np.all(np.isfinite(out))
        assert np.max(np.abs(out)) <= 1.0


def test_update_settings_applies_while_running_without_rebuild(processor):
    rng = np.random.default_rng(3)
    frame = rng.uniform(-0.1, 0.1, processor.block_size).astype(np.float32)
    processor._process_frame(frame)
    eq_before = processor.eq
    processor.update_settings(dict(DEFAULT_SETTINGS, eq_high_gain_db=6.0))
    processor._process_frame(frame)
    assert processor.eq is eq_before
    assert processor.eq.high_gain == pytest.approx(6.0)


class _FakeTensor:
    def __init__(self, arr):
        self.arr = np.asarray(arr)

    def float(self):
        return self

    def unsqueeze(self, dim):
        return _FakeTensor(np.expand_dims(self.arr, dim))

    def detach(self):
        return self

    def cpu(self):
        return self

    def numpy(self):
        return self.arr


class _FakeTorch:
    @staticmethod
    def from_numpy(arr):
        return _FakeTensor(arr)

    @staticmethod
    def no_grad():
        return contextlib.nullcontext()


def test_deep_filter_blend_strength_and_length_guard(processor):
    short_by = 5

    def fake_enhance(model, state, audio):
        n = audio.arr.shape[-1] - short_by
        return _FakeTensor(np.zeros((1, n), dtype=np.float32))

    processor.df_available = True
    processor.torch_module = _FakeTorch
    processor.df_enhance_fn = fake_enhance
    processor.update_settings(dict(
        DEFAULT_SETTINGS, ns_mode="high", gate_enabled=False, eq_enabled=False,
        compressor_enabled=False, ns_high_strength=0.0,
    ))
    rng = np.random.default_rng(4)
    x = rng.uniform(-0.3, 0.3, processor.block_size).astype(np.float32)

    dry = processor._process_frame(x)
    assert np.allclose(dry, x, atol=1e-6)

    processor.update_settings(dict(
        DEFAULT_SETTINGS, ns_mode="high", gate_enabled=False, eq_enabled=False,
        compressor_enabled=False, ns_high_strength=1.0,
    ))
    wet = processor._process_frame(x)
    assert len(wet) == processor.block_size
    assert np.allclose(wet[:-short_by], 0.0, atol=1e-6)
    assert np.allclose(wet[-short_by:], x[-short_by:], atol=1e-6)


def test_get_status_schema_types_and_json(processor):
    initial = processor.get_status()
    assert set(initial) == STATUS_KEYS
    assert all(v is not None for v in initial.values())

    rng = np.random.default_rng(5)
    processor._process_frame(rng.uniform(-0.5, 0.5, processor.block_size).astype(np.float32))
    status = processor.get_status()
    assert set(status) == STATUS_KEYS
    for key in ("input_rms", "output_rms", "latency_ms", "cpu_load"):
        assert type(status[key]) is float, key
    for key in ("gate_state", "is_running", "df_available", "vst_loaded", "vst_failed", "clipping"):
        assert type(status[key]) is bool, key
    assert type(status["sample_rate"]) is int
    assert status["sample_rate"] == processor.sr
    assert len(status["fft_data"]) == 64
    assert all(type(v) is float and 0.0 <= v <= 1.0 for v in status["fft_data"])
    assert 0.0 <= status["cpu_load"] <= 100.0
    assert status["is_running"] is False
    json.dumps(status)


def test_clipping_flag_latches_for_about_one_second(processor):
    processor.update_settings(dict(DEFAULT_SETTINGS, output_gain=4.0, gate_enabled=False, eq_enabled=False,
                                   compressor_enabled=False, ns_mode="off"))
    processor._process_frame(sine(1000.0, 0.5, processor.sr, processor.block_size))
    assert processor.get_status()["clipping"] is True
    processor.update_settings(dict(DEFAULT_SETTINGS, output_gain=0.0, gate_enabled=False, eq_enabled=False,
                                   compressor_enabled=False, ns_mode="off"))
    quiet = np.zeros(processor.block_size, dtype=np.float32)
    for _ in range(int(1.0 / processor.FRAME_SECONDS) - 1):
        processor._process_frame(quiet)
    assert processor.get_status()["clipping"] is True
    processor._process_frame(quiet)
    processor._process_frame(quiet)
    assert processor.get_status()["clipping"] is False


def test_show_vst_editor_without_plugin_raises(processor):
    with pytest.raises(RuntimeError, match="No VST3 plugin loaded"):
        processor.show_vst_editor()


def test_stop_is_idempotent_without_start(processor):
    processor.stop()
    processor.stop()
    assert not processor.running.is_set()
    assert processor.get_status()["is_running"] is False


def test_failed_start_leaves_clean_stopped_state(processor, monkeypatch):
    monkeypatch.setattr(AudioProcessor, "_find_device_idx", lambda self, *a, **k: None)
    monkeypatch.setattr(AudioProcessor, "_negotiate_sample_rate", lambda self, *a, **k: 48000)

    class BrokenInputStream:
        def __init__(self, *args, **kwargs):
            raise RuntimeError("no input device (test)")

    monkeypatch.setattr(ap.sd, "InputStream", BrokenInputStream)

    assert processor.start() is False
    assert not processor.running.is_set()
    assert processor.processor_thread is None
    assert processor.input_stream is None
    assert processor.output_stream is None
    processor.stop()


def test_eco_ns_keeps_steady_tone_and_cuts_noise():
    sr = 48000
    rng = np.random.default_rng(6)
    t = np.arange(sr) / sr
    tone = 0.1 * np.sin(2.0 * np.pi * 440.0 * t)  # -20 dBFS peak
    noise = 0.001 * rng.standard_normal(sr)       # -60 dBFS RMS
    warm = int(0.3 * sr)

    out = eco_run(sr, tone + noise, 0.8)
    basis = np.stack([np.sin(2.0 * np.pi * 440.0 * t[warm:]), np.cos(2.0 * np.pi * 440.0 * t[warm:])], axis=1)
    coef = np.linalg.lstsq(basis, out[warm:len(t)].astype(np.float64), rcond=None)[0]
    tone_energy_ratio = float(np.hypot(*coef) ** 2 / 0.1 ** 2)
    assert tone_energy_ratio >= 0.8

    noise_out = eco_run(sr, noise, 0.8)
    drop_db = 20.0 * np.log10(rms(noise_out[warm:]) / rms(noise[warm:len(noise_out)]))
    assert drop_db <= -6.0


@pytest.mark.parametrize("sr", [44100, 48000])
def test_eco_ns_silence_in_silence_out(sr):
    out = eco_run(sr, np.zeros(sr, dtype=np.float32), 0.8)
    assert np.all(np.isfinite(out))
    assert np.array_equal(out, np.zeros_like(out))


@pytest.mark.parametrize("sr", [44100, 48000])
def test_eco_ns_returns_hop_samples_per_call(sr):
    hop = eco_hop(sr)
    eco = SpectralSubtractedNoiseSuppression(block_size=2 * hop, hop_size=hop, sr=sr)
    rng = np.random.default_rng(7)
    for _ in range(100):
        out = eco.process(rng.uniform(-0.2, 0.2, hop).astype(np.float32), 0.8)
        assert out.shape == (hop,)
        assert out.dtype == np.float32
        assert np.all(np.isfinite(out))


@pytest.mark.parametrize("sr", [44100, 48000])
def test_eco_ns_is_cheap_per_10ms_frame(sr):
    hop = eco_hop(sr)
    eco = SpectralSubtractedNoiseSuppression(block_size=2 * hop, hop_size=hop, sr=sr)
    rng = np.random.default_rng(8)
    frames = rng.uniform(-0.1, 0.1, (300, 2 * hop)).astype(np.float32)
    start = time.perf_counter()
    for frame in frames:
        eco.process(frame[:hop], 0.8)
        eco.process(frame[hop:], 0.8)
    per_frame_ms = (time.perf_counter() - start) * 1000.0 / len(frames)
    assert per_frame_ms < 2.0, per_frame_ms


def test_init_com_is_safe_on_worker_threads():
    import threading

    errors = []

    def run():
        try:
            ap._init_com()
            ap._init_com()
        except Exception as exc:  # pragma: no cover
            errors.append(exc)

    t = threading.Thread(target=run)
    t.start()
    t.join()
    assert not errors
