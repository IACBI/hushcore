import ctypes
import logging
import math
import sys
import threading
import time

import numpy as np
import scipy.ndimage as ndi
import scipy.signal as signal
import sounddevice as sd

from config import DEFAULT_SETTINGS

try:
    from pedalboard import VST3Plugin
except ImportError:
    VST3Plugin = None

logger = logging.getLogger("hushcore.audio")

_EPS = 1e-12
_SILENCE_DB = -120.0


def _init_com():
    """Initialise COM (MTA) on the calling thread.

    PortAudio's WASAPI backend only opens streams on threads that already have COM
    initialised. The web server calls start/stop from short-lived pool threads, where
    opening a WASAPI stream otherwise fails with a bogus WDM-KS host error.
    """
    if sys.platform != "win32":
        return
    try:
        ctypes.windll.ole32.CoInitializeEx(None, 0)
    except Exception:
        logger.debug("CoInitializeEx failed", exc_info=True)


def _rms_db(x):
    if x.size == 0:
        return _SILENCE_DB
    return float(20.0 * np.log10(np.sqrt(np.mean(x * x) + _EPS)))


def _fit_length(y, reference):
    # Used when an external model returns a different length; the missing tail falls back
    # to the reference so a wet/dry blend still passes audio through there.
    n = reference.shape[0]
    if y.shape[0] >= n:
        return y[:n]
    return np.concatenate((y, reference[y.shape[0]:]))


def _normalize(b, a):
    a0 = a[0]
    return np.asarray(b, dtype=np.float64) / a0, np.asarray(a, dtype=np.float64) / a0


# RBJ Audio EQ Cookbook biquads. Coefficients are float64: near-DC poles lose accuracy in float32.

def _low_shelf(sr, f0, q, gain_db):
    A = 10.0 ** (float(gain_db) / 40.0)
    w0 = 2.0 * np.pi * f0 / sr
    cs = np.cos(w0)
    alpha = np.sin(w0) / 2.0 * np.sqrt((A + 1.0 / A) * (1.0 / q - 1.0) + 2.0)
    sa = 2.0 * np.sqrt(A) * alpha
    b = [A * ((A + 1.0) - (A - 1.0) * cs + sa),
         2.0 * A * ((A - 1.0) - (A + 1.0) * cs),
         A * ((A + 1.0) - (A - 1.0) * cs - sa)]
    a = [(A + 1.0) + (A - 1.0) * cs + sa,
         -2.0 * ((A - 1.0) + (A + 1.0) * cs),
         (A + 1.0) + (A - 1.0) * cs - sa]
    return _normalize(b, a)


def _high_shelf(sr, f0, q, gain_db):
    A = 10.0 ** (float(gain_db) / 40.0)
    w0 = 2.0 * np.pi * f0 / sr
    cs = np.cos(w0)
    alpha = np.sin(w0) / 2.0 * np.sqrt((A + 1.0 / A) * (1.0 / q - 1.0) + 2.0)
    sa = 2.0 * np.sqrt(A) * alpha
    b = [A * ((A + 1.0) + (A - 1.0) * cs + sa),
         -2.0 * A * ((A - 1.0) + (A + 1.0) * cs),
         A * ((A + 1.0) + (A - 1.0) * cs - sa)]
    a = [(A + 1.0) - (A - 1.0) * cs + sa,
         2.0 * ((A - 1.0) - (A + 1.0) * cs),
         (A + 1.0) - (A - 1.0) * cs - sa]
    return _normalize(b, a)


def _peaking(sr, f0, q, gain_db):
    A = 10.0 ** (float(gain_db) / 40.0)
    w0 = 2.0 * np.pi * f0 / sr
    cs = np.cos(w0)
    alpha = np.sin(w0) / (2.0 * q)
    b = [1.0 + alpha * A, -2.0 * cs, 1.0 - alpha * A]
    a = [1.0 + alpha / A, -2.0 * cs, 1.0 - alpha / A]
    return _normalize(b, a)


def _bandpass(sr, f0, q):
    w0 = 2.0 * np.pi * f0 / sr
    cs = np.cos(w0)
    alpha = np.sin(w0) / (2.0 * q)
    return _normalize([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cs, 1.0 - alpha])


def _highpass(sr, f0, q):
    f0 = min(max(f0, 10.0), 0.45 * sr)
    w0 = 2.0 * np.pi * f0 / sr
    cs = np.cos(w0)
    alpha = np.sin(w0) / (2.0 * q)
    return _normalize([(1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0],
                      [1.0 + alpha, -2.0 * cs, 1.0 - alpha])


class RingBuffer:
    """Fixed-capacity float32 FIFO. Overflow drops the oldest samples so latency stays bounded."""

    def __init__(self, capacity):
        self._buf = np.zeros(capacity, dtype=np.float32)
        self._cap = int(capacity)
        self._head = 0
        self._size = 0
        self._lock = threading.Lock()

    @property
    def available(self):
        return self._size

    def clear(self):
        with self._lock:
            self._head = 0
            self._size = 0

    def write(self, data):
        n = len(data)
        if n > self._cap:
            data = data[n - self._cap:]
            n = self._cap
        if n == 0:
            return
        with self._lock:
            overflow = self._size + n - self._cap
            if overflow > 0:
                self._head = (self._head + overflow) % self._cap
                self._size -= overflow
            tail = (self._head + self._size) % self._cap
            first = min(n, self._cap - tail)
            self._buf[tail:tail + first] = data[:first]
            if n > first:
                self._buf[:n - first] = data[first:]
            self._size += n

    def read(self, n):
        out = np.zeros(n, dtype=np.float32)
        self.read_into(out)
        return out

    def read_into(self, out):
        """Fill out with the oldest samples, zero-fill any shortfall, return samples taken."""
        n = len(out)
        with self._lock:
            k = min(n, self._size)
            first = min(k, self._cap - self._head)
            out[:first] = self._buf[self._head:self._head + first]
            if k > first:
                out[first:k] = self._buf[:k - first]
            out[k:] = 0.0
            self._head = (self._head + k) % self._cap
            self._size -= k
        return k


class SpectralSubtractedNoiseSuppression:
    """Spectral subtraction with minimum-statistics noise tracking.

    The noise floor is the running minimum of a time-smoothed power spectrum over about
    1.5 s, scaled by BIAS because the minimum of a noisy periodogram sits below its mean.
    A minimum tracker alone would take a steady tone for noise, so bins that stand well
    above their spectral neighbourhood floor use that floor instead.
    """

    # Steady-state mean/min ratio of the tracked floor on white noise. The minimum of fewer
    # frames sits closer to the mean, so BIAS_CURVE scales it down while the window fills.
    BIAS = 2.75
    BIAS_CURVE_S = (0.0, 0.05, 0.2, 0.5, 1.5)
    BIAS_CURVE = (0.35, 0.53, 0.70, 0.80, 1.0)
    OVER_SUBTRACTION = 1.5
    FLOOR_DB = 20.0
    WINDOW_S = 1.5
    SUBWINDOW_S = 0.25
    WARMUP_S = 0.3
    SETTLE_S = 0.1
    POWER_SMOOTHING = 0.8
    TONAL_RATIO = 10.0
    NEIGHBOUR_HALF_BINS = 12
    TONAL_DILATE_BINS = 2
    GAIN_ATTACK = 0.3   # weight on the previous gain when the gain is falling (fast)
    GAIN_RELEASE = 0.7  # weight on the previous gain when the gain is rising (slow)
    _EPS = 1e-12

    def __init__(self, block_size=480, hop_size=240, sr=48000):
        self.block_size = block_size
        self.hop_size = hop_size
        self.sr = sr

        self.input_buffer = np.zeros(block_size, dtype=np.float32)
        self.output_buffer = np.zeros(block_size, dtype=np.float32)
        # A sine window sums to a constant under 50% overlap-add when used for analysis and
        # synthesis. A Hann window applied twice leaves an amplitude ripple at the hop rate.
        self.window = np.sin(np.pi * (np.arange(block_size) + 0.5) / block_size).astype(np.float32)

        n_bins = block_size // 2 + 1
        self._n_sub = max(1, int(round(self.WINDOW_S / self.SUBWINDOW_S)))
        self._sub_len = max(1, int(round(self.SUBWINDOW_S * sr / hop_size)))
        self._warmup_frames = max(1, int(round(self.WARMUP_S * sr / hop_size)))
        self._settle_frames = max(1, int(round(self.SETTLE_S * sr / hop_size)))
        self._frames = 0
        self._pending_fill = max(1, -(-block_size // hop_size) - 1)
        self._power = np.zeros(n_bins)
        self._sub_min = np.full((self._n_sub, n_bins), np.inf)
        self._cur_min = np.full(n_bins, np.inf)
        self._sub_idx = 0
        self._sub_count = 0
        self._gain = np.ones(n_bins)

    def _update_noise(self, power):
        if self._frames == 0:
            self._power[:] = power
        else:
            self._power *= self.POWER_SMOOTHING
            self._power += (1.0 - self.POWER_SMOOTHING) * power
        if self._frames < self._settle_frames:
            # The smoother is still converging; its early values are noisier, so their minima read low.
            return np.zeros_like(power)

        np.minimum(self._cur_min, self._power, out=self._cur_min)
        self._sub_count += 1
        if self._sub_count >= self._sub_len:
            self._sub_min[self._sub_idx] = self._cur_min
            self._sub_idx = (self._sub_idx + 1) % self._n_sub
            self._cur_min.fill(np.inf)
            self._sub_count = 0
        return np.minimum(self._sub_min.min(axis=0), self._cur_min)

    def _suppress(self, spec, s):
        power = spec.real ** 2 + spec.imag ** 2

        tracked_s = max(0, self._frames - self._settle_frames) * self.hop_size / self.sr
        bias = self.BIAS * float(np.interp(tracked_s, self.BIAS_CURVE_S, self.BIAS_CURVE))
        noise = bias * self._update_noise(power)
        half = self.NEIGHBOUR_HALF_BINS
        floor_nb = ndi.minimum_filter1d(noise, size=2 * half + 1, mode="nearest")
        tonal = self._power > self.TONAL_RATIO * floor_nb
        tonal = ndi.binary_dilation(tonal, structure=np.ones(2 * self.TONAL_DILATE_BINS + 1, dtype=bool))
        noise_eff = np.where(tonal, floor_nb, noise)

        # Power-domain over-subtraction. strength=0 gives gain 1 everywhere, i.e. exact pass-through.
        over = self.OVER_SUBTRACTION * s
        floor_pow = 10.0 ** (-self.FLOOR_DB * s / 10.0)
        gain_pow = 1.0 - over * noise_eff / (power + self._EPS)
        np.clip(gain_pow, floor_pow, 1.0, out=gain_pow)

        smoothed = gain_pow.copy()
        smoothed[1:-1] = 0.25 * gain_pow[:-2] + 0.5 * gain_pow[1:-1] + 0.25 * gain_pow[2:]
        weight = np.where(smoothed < self._gain, self.GAIN_ATTACK, self.GAIN_RELEASE)
        self._gain = weight * self._gain + (1.0 - weight) * smoothed

        # Raised-cosine ramp so suppression fades in while the noise estimate is still settling.
        ramp = 0.5 * (1.0 - math.cos(math.pi * min(self._frames / self._warmup_frames, 1.0)))
        self._frames += 1
        applied = 1.0 - ramp * (1.0 - self._gain)
        return spec * np.sqrt(applied)

    def process(self, chunk, strength=0.8):
        self.input_buffer[:-self.hop_size] = self.input_buffer[self.hop_size:]
        self.input_buffer[-self.hop_size:] = chunk

        win_input = self.input_buffer * self.window

        spec = np.fft.rfft(win_input)

        if self._pending_fill > 0:
            # The window still holds zeros from stream start; a partial window would pull the
            # minimum tracker low for the whole 1.5 s, so this hop passes through untouched.
            self._pending_fill -= 1
            clean_fft = spec
        else:
            clean_fft = self._suppress(spec, min(max(float(strength), 0.0), 1.0))

        clean_win = np.fft.irfft(clean_fft, n=self.block_size).astype(np.float32)

        self.output_buffer += clean_win * self.window

        out_chunk = self.output_buffer[:self.hop_size].copy()

        self.output_buffer[:-self.hop_size] = self.output_buffer[self.hop_size:]
        self.output_buffer[-self.hop_size:] = 0.0

        return out_chunk


class Equalizer3Band:
    def __init__(self, sr=48000):
        self.sr = sr
        self.low_gain = 0.0
        self.mid_gain = 0.0
        self.high_gain = 0.0
        self._design()
        self.reset_state()

    def reset_state(self):
        self.zi_low = np.zeros(2)
        self.zi_mid = np.zeros(2)
        self.zi_high = np.zeros(2)

    def update_gains(self, low, mid, high):
        if low != self.low_gain or mid != self.mid_gain or high != self.high_gain:
            self.low_gain = low
            self.mid_gain = mid
            self.high_gain = high
            self._design()

    def _design(self):
        self.b_low, self.a_low = _low_shelf(self.sr, 200.0, 0.707, self.low_gain)
        self.b_mid, self.a_mid = _peaking(self.sr, 1000.0, 0.707, self.mid_gain)
        self.b_high, self.a_high = _high_shelf(self.sr, 4000.0, 0.707, self.high_gain)

    def process(self, data):
        y, self.zi_low = signal.lfilter(self.b_low, self.a_low, data, zi=self.zi_low)
        y, self.zi_mid = signal.lfilter(self.b_mid, self.a_mid, y, zi=self.zi_mid)
        y, self.zi_high = signal.lfilter(self.b_high, self.a_high, y, zi=self.zi_high)
        return y.astype(np.float32)


class DynamicsCompressor:
    def __init__(self, sr=48000, block_size=480):
        self.sr = sr
        self._frame_ms = 1000.0 * block_size / sr
        self.threshold_db = -18.0
        self.ratio = 3.0
        self.attack_ms = 10.0
        self.release_ms = 100.0
        self.env_db = -60.0

    def process(self, data):
        rms_db = _rms_db(data)

        if rms_db > self.env_db:
            alpha = math.exp(-self._frame_ms / self.attack_ms)
        else:
            alpha = math.exp(-self._frame_ms / self.release_ms)
        self.env_db = alpha * self.env_db + (1.0 - alpha) * rms_db

        gain_db = 0.0
        if self.env_db > self.threshold_db:
            excess_db = self.env_db - self.threshold_db
            gain_db = excess_db * (1.0 / max(self.ratio, 1.0) - 1.0)

        return (data * (10.0 ** (gain_db / 20.0))).astype(np.float32)


class NoiseGate:
    ATTACK_MS = 2.0
    HOLD_MS = 20.0

    def __init__(self, sr=48000, block_size=480):
        self._frame_ms = 1000.0 * block_size / sr
        self._attack_alpha = math.exp(-self._frame_ms / self.ATTACK_MS)
        self._hold_frames = max(1, int(round(self.HOLD_MS / self._frame_ms)))
        self.gain = 0.0
        self.is_open = False
        self._hold_left = 0

    def bypass(self):
        self.gain = 1.0
        self.is_open = True
        self._hold_left = 0

    def process(self, data, threshold_db, release_ms):
        if _rms_db(data) > threshold_db:
            self._hold_left = self._hold_frames
            target = 1.0
        elif self._hold_left > 0:
            self._hold_left -= 1
            target = 1.0
        else:
            target = 0.0
        self.is_open = target > 0.5

        if target > self.gain:
            alpha = self._attack_alpha
        else:
            alpha = math.exp(-self._frame_ms / max(release_ms, 1e-3))
        self.gain = alpha * self.gain + (1.0 - alpha) * target
        return (data * self.gain).astype(np.float32)


class Limiter:
    """Per-frame peak limiter: gain drops immediately to keep the frame under threshold, then recovers exponentially."""

    def __init__(self, sr=48000, block_size=480, release_ms=100.0):
        self._frame_ms = 1000.0 * block_size / sr
        self.release_ms = release_ms
        self.gain = 1.0

    def process(self, data, threshold_lin):
        peak = float(np.max(np.abs(data))) if data.size else 0.0
        target = 1.0 if peak <= threshold_lin else threshold_lin / peak

        if target < self.gain:
            self.gain = target
        else:
            alpha = math.exp(-self._frame_ms / self.release_ms)
            self.gain = alpha * self.gain + (1.0 - alpha) * target
        return (data * self.gain).astype(np.float32)


class VocalExciter:
    def __init__(self, sr=48000):
        self.sr = sr
        self.freq = 3000.0
        self.amount = 0.2
        self.mix = 0.15
        self.b, self.a = _highpass(sr, self.freq, 0.707)
        self.reset_state()

    def reset_state(self):
        self.zi = np.zeros(2)

    def update_params(self, freq, amount, mix):
        if freq != self.freq:
            self.freq = freq
            self.b, self.a = _highpass(self.sr, freq, 0.707)
        self.amount = amount
        self.mix = mix

    def process(self, data):
        hp_data, self.zi = signal.lfilter(self.b, self.a, data, zi=self.zi)
        excited = np.tanh(hp_data * (1.0 + self.amount * 5.0))
        out = data * (1.0 - self.mix) + excited * self.mix
        return out.astype(np.float32)


class VocalDeEsser:
    SHELF_FREQ = 5000.0
    ENV_RELEASE_MS = 15.0
    # Below this the shelf is a no-op, so it is bypassed rather than run as an identity filter.
    BYPASS_DB = 0.05

    def __init__(self, sr=48000, block_size=480):
        self.sr = sr
        self._frame_ms = 1000.0 * block_size / sr
        self.threshold_db = -25.0
        self.amount = 0.5
        self.freq = 6000.0  # Center of the sibilant band
        self.b_bp, self.a_bp = _bandpass(sr, self.freq, 1.0)
        self.reset_state()

    def reset_state(self):
        self.zi_bp = np.zeros(2)
        self.zi_hs = np.zeros(2)
        self.env = 0.0
        self._shelf_engaged = False
        self._shelf_db = 0.0
        self.b_hs = None
        self.a_hs = None

    def update_params(self, threshold_db, amount):
        self.threshold_db = threshold_db
        self.amount = amount

    def process(self, data):
        sib_data, self.zi_bp = signal.lfilter(self.b_bp, self.a_bp, data, zi=self.zi_bp)
        sib_rms = float(np.sqrt(np.mean(sib_data * sib_data) + 1e-12))

        env_alpha = math.exp(-self._frame_ms / self.ENV_RELEASE_MS)
        self.env = env_alpha * self.env + (1.0 - env_alpha) * sib_rms
        env_db = 20.0 * np.log10(self.env + 1e-12)

        gain_reduction_db = 0.0
        if env_db > self.threshold_db:
            excess_db = env_db - self.threshold_db
            gain_reduction_db = max(-15.0, -excess_db * self.amount)

        if abs(gain_reduction_db) < self.BYPASS_DB:
            # The shelf state is not advanced while bypassed; it is re-seeded on re-engage.
            self._shelf_engaged = False
            return np.asarray(data, dtype=np.float32)

        if not self._shelf_engaged:
            # Seed the state from the current input so re-engaging does not click.
            self.b_hs, self.a_hs = _high_shelf(self.sr, self.SHELF_FREQ, 0.707, gain_reduction_db)
            self._shelf_db = gain_reduction_db
            self.zi_hs = signal.lfilter_zi(self.b_hs, self.a_hs) * float(data[0])
            self._shelf_engaged = True
        elif abs(gain_reduction_db - self._shelf_db) >= self.BYPASS_DB:
            self.b_hs, self.a_hs = _high_shelf(self.sr, self.SHELF_FREQ, 0.707, gain_reduction_db)
            self._shelf_db = gain_reduction_db

        out, self.zi_hs = signal.lfilter(self.b_hs, self.a_hs, data, zi=self.zi_hs)
        return out.astype(np.float32)


class AudioProcessor:
    FRAME_SECONDS = 0.01
    FFT_BINS = 64
    CLIP_LEVEL = 0.99
    CLIP_HOLD_S = 1.0
    LIMITER_RELEASE_MS = 100.0
    CPU_SMOOTHING = 0.1

    def __init__(self, settings):
        self.settings = settings
        self._lifecycle_lock = threading.RLock()
        self.running = threading.Event()
        self._frame_ready = threading.Event()
        self._session = None
        self.processor_thread = None
        self.input_stream = None
        self.output_stream = None
        self.monitor_stream = None
        self._out_scratch = np.zeros(1, dtype=np.float32)
        self._mon_scratch = np.zeros(1, dtype=np.float32)

        self.vst_plugin = None
        self.vst_failed = False
        self._loaded_vst_path = ""

        self.df_available = False
        self.df_model = None
        self.df_state = None
        self.df_enhance_fn = None
        self.torch_module = None
        self._init_deep_filter()

        self.input_rms = _SILENCE_DB
        self.output_rms = _SILENCE_DB
        self.gate_state = False
        self.fft_data = [0.0] * self.FFT_BINS
        self._cpu_load = 0.0
        self._clip_hold = 0
        self._dsp_error_logged = False
        self._df_error_logged = False

        self._configure(48000)

    def _configure(self, sr):
        """(Re)build all DSP state and buffers for a sample rate. Only called while stopped."""
        self.sr = int(sr)
        self.block_size = int(self.sr * self.FRAME_SECONDS)
        frame_ms = 1000.0 * self.block_size / self.sr

        # The eco NS works on two overlapping halves, so it needs an even block. For odd
        # blocks (e.g. 441 at 44.1 kHz) it runs on one padded sample and _eco_suppress trims.
        hop = (self.block_size + 1) // 2
        self.eco_ns = SpectralSubtractedNoiseSuppression(block_size=2 * hop, hop_size=hop, sr=self.sr)
        self.eq = Equalizer3Band(sr=self.sr)
        self.gate = NoiseGate(sr=self.sr, block_size=self.block_size)
        self.compressor = DynamicsCompressor(sr=self.sr, block_size=self.block_size)
        self.deesser = VocalDeEsser(sr=self.sr, block_size=self.block_size)
        self.exciter = VocalExciter(sr=self.sr)
        self.limiter = Limiter(sr=self.sr, block_size=self.block_size, release_ms=self.LIMITER_RELEASE_MS)

        self._fft_window = np.hanning(self.block_size).astype(np.float32)
        self._fft_scale = 2.0 / float(np.sum(self._fft_window))
        self._clip_hold_frames = max(1, int(round(self.CLIP_HOLD_S * 1000.0 / frame_ms)))
        self._clip_hold = 0
        self._cpu_load = 0.0

        ring_capacity = max(8 * self.block_size, 4096)
        self._input_ring = RingBuffer(ring_capacity)
        self._output_ring = RingBuffer(ring_capacity)
        self._monitor_ring = RingBuffer(ring_capacity)

    def _init_deep_filter(self):
        try:
            from df.enhance import init_df, enhance
            import torch
            self.df_model, self.df_state, _ = init_df()
            self.df_enhance_fn = enhance
            self.torch_module = torch
            self.df_available = True
            logger.info("DeepFilterNet loaded.")
        except Exception as e:
            self.df_model = None
            self.df_state = None
            self.df_enhance_fn = None
            self.torch_module = None
            self.df_available = False
            logger.info("DeepFilterNet not available (using Eco Mode fallback): %s", e)

    def _snapshot(self):
        # Copy once per frame so a concurrent settings change cannot land mid-frame.
        return {**DEFAULT_SETTINGS, **self.settings}

    def _apply_dsp_params(self, cfg):
        self.eq.update_gains(float(cfg["eq_low_gain_db"]), float(cfg["eq_mid_gain_db"]), float(cfg["eq_high_gain_db"]))
        self.compressor.threshold_db = float(cfg["compressor_threshold_db"])
        self.compressor.ratio = float(cfg["compressor_ratio"])
        self.compressor.attack_ms = max(1.0, float(cfg["compressor_attack_ms"]))
        self.compressor.release_ms = max(10.0, float(cfg["compressor_release_ms"]))
        self.deesser.update_params(float(cfg["deesser_threshold_db"]), float(cfg["deesser_amount"]))
        self.exciter.update_params(float(cfg["exciter_frequency"]), float(cfg["exciter_amount"]), float(cfg["exciter_mix"]))

    def update_settings(self, settings):
        # DSP parameters are applied per frame from a snapshot, so this is safe while running.
        with self._lifecycle_lock:
            self.settings = settings
            self._sync_vst(self._snapshot())

    def _sync_vst(self, cfg):
        vst_enabled = bool(cfg["vst_enabled"])
        vst_path = cfg["vst_path"] or ""

        if not vst_enabled or not vst_path:
            if self.vst_plugin is not None:
                logger.info("Unloading VST3 plugin.")
            self.vst_plugin = None
            self._loaded_vst_path = ""
            self.vst_failed = False
            return

        # A plugin that failed in the audio loop stays bypassed until its path changes or it is disabled.
        if self.vst_plugin is not None and self._loaded_vst_path == vst_path:
            return

        self.vst_plugin = None
        self.vst_failed = False
        logger.info("Loading VST3 plugin: %s", vst_path)
        try:
            if VST3Plugin is None:
                raise ImportError("pedalboard is not installed or VST3Plugin could not be imported.")
            self.vst_plugin = VST3Plugin(vst_path)
            self._loaded_vst_path = vst_path
            logger.info("Loaded VST3 plugin: %s", self.vst_plugin.name)
        except Exception as e:
            logger.error("Error loading VST3 plugin %s: %s", vst_path, e)
            self.vst_plugin = None
            self.vst_failed = True

    def show_vst_editor(self):
        plugin = self.vst_plugin
        if plugin is None:
            raise RuntimeError("No VST3 plugin loaded")
        plugin.show_editor()

    def get_devices(self):
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
                "default_samplerate": dev["default_samplerate"]
            }
            if dev["max_input_channels"] > 0:
                input_devs.append(dev_info)
            if dev["max_output_channels"] > 0:
                output_devs.append(dev_info)

        return {"inputs": input_devs, "outputs": output_devs}

    def _input_callback(self, indata, frames, time_info, status):
        if status:
            logger.warning("Input stream status: %s", status)
        self._input_ring.write(indata[:, 0])
        self._frame_ready.set()

    def _output_callback(self, outdata, frames, time_info, status):
        if status:
            logger.warning("Output stream status: %s", status)
        self._out_scratch = self._fill_outdata(outdata, self._output_ring, self._out_scratch)

    def _monitor_callback(self, outdata, frames, time_info, status):
        if status:
            logger.warning("Monitor stream status: %s", status)
        self._mon_scratch = self._fill_outdata(outdata, self._monitor_ring, self._mon_scratch)

    @staticmethod
    def _fill_outdata(outdata, ring, scratch):
        frames = outdata.shape[0]
        if scratch.shape[0] < frames:
            scratch = np.zeros(frames, dtype=np.float32)
        data = scratch[:frames]
        ring.read_into(data)
        outdata[:] = data[:, np.newaxis]
        return scratch

    @staticmethod
    def _stream_blocksize(cfg):
        value = cfg.get("buffer_size", "auto")
        if value == "auto":
            return 0
        try:
            return int(value)
        except (TypeError, ValueError):
            return 0

    def start(self):
        _init_com()
        with self._lifecycle_lock:
            if self.running.is_set():
                return True
            try:
                ok = self._start_locked()
            except Exception:
                logger.exception("Failed to start audio pipeline.")
                ok = False
            if not ok:
                self.stop()
            return ok

    def _start_locked(self):
        cfg = self._snapshot()
        input_idx = self._find_device_idx(cfg["input_device"], is_input=True)
        output_idx = self._find_device_idx(cfg["output_device"], is_input=False)
        monitor_idx = self._find_device_idx(cfg["monitor_device"], is_input=False)

        sr = self._negotiate_sample_rate(input_idx, output_idx, monitor_idx, cfg["monitor_enabled"])
        self._configure(sr)
        self._sync_vst(cfg)
        self._dsp_error_logged = False
        self._df_error_logged = False

        self._frame_ready.clear()
        self._session = object()
        self.running.set()
        self.processor_thread = threading.Thread(
            target=self._processing_loop, args=(self._session,), name="hushcore-dsp", daemon=True
        )
        self.processor_thread.start()

        blocksize = self._stream_blocksize(cfg)
        try:
            self.input_stream = sd.InputStream(
                device=input_idx, samplerate=self.sr, channels=1,
                blocksize=blocksize, callback=self._input_callback
            )
            self.input_stream.start()
            logger.info("Started input stream on device %s (blocksize=%s)", input_idx, blocksize)
        except Exception as e:
            logger.error("Failed to start input stream on device %s: %s", input_idx, e)
            return False

        try:
            self.output_stream = sd.OutputStream(
                device=output_idx, samplerate=self.sr, channels=2,
                blocksize=blocksize, callback=self._output_callback
            )
            self.output_stream.start()
            logger.info("Started output stream on device %s", output_idx)
        except Exception as e:
            logger.error("Failed to start output stream on device %s: %s", output_idx, e)
            return False

        if cfg["monitor_enabled"] and monitor_idx is not None:
            self.start_monitoring(cfg["monitor_device"])

        return True

    def start_monitoring(self, device_name):
        _init_com()
        with self._lifecycle_lock:
            self.stop_monitoring()
            monitor_idx = self._find_device_idx(device_name, is_input=False)
            if monitor_idx is None:
                return

            blocksize = self._stream_blocksize(self._snapshot())
            self._monitor_ring.clear()
            try:
                stream = sd.OutputStream(
                    device=monitor_idx, samplerate=self.sr, channels=2,
                    blocksize=blocksize, callback=self._monitor_callback
                )
                stream.start()
            except Exception as e:
                logger.error("Failed to start monitoring: %s", e)
                return
            self.monitor_stream = stream
            logger.info("Started monitoring stream on device %s", monitor_idx)

    def stop_monitoring(self):
        with self._lifecycle_lock:
            stream, self.monitor_stream = self.monitor_stream, None
            if stream is not None:
                self._close_stream(stream)
                logger.info("Stopped monitoring stream.")

    @staticmethod
    def _close_stream(stream):
        if stream is None:
            return
        try:
            stream.stop()
        except Exception:
            logger.debug("Stream stop failed", exc_info=True)
        try:
            stream.close()
        except Exception:
            logger.debug("Stream close failed", exc_info=True)

    def stop(self):
        _init_com()
        with self._lifecycle_lock:
            was_running = self.running.is_set()
            self.running.clear()
            self._session = None
            self._frame_ready.set()

            input_stream, self.input_stream = self.input_stream, None
            output_stream, self.output_stream = self.output_stream, None
            self._close_stream(input_stream)
            self._close_stream(output_stream)
            self.stop_monitoring()

            thread, self.processor_thread = self.processor_thread, None
            if thread is not None and thread is not threading.current_thread():
                thread.join(timeout=1.0)
                if thread.is_alive():
                    logger.warning("Processing thread did not exit within 1 s.")

            if was_running:
                logger.info("Audio pipeline stopped.")

    def _find_device_idx(self, device_name, is_input=True):
        devices = sd.query_devices()
        hostapis = sd.query_hostapis()

        if device_name:
            # 1. Match full name with Host API suffix
            for idx, dev in enumerate(devices):
                api_name = hostapis[dev["hostapi"]]["name"]
                full_name = f"{dev['name']} [{api_name}]"
                if full_name == device_name:
                    if is_input and dev["max_input_channels"] > 0:
                        return idx
                    if not is_input and dev["max_output_channels"] > 0:
                        return idx

            # 2. Substring match
            for idx, dev in enumerate(devices):
                if device_name.lower() in dev["name"].lower():
                    if is_input and dev["max_input_channels"] > 0:
                        return idx
                    if not is_input and dev["max_output_channels"] > 0:
                        return idx
        else:
            # If no device selected on startup, search for a Virtual Cable to avoid playing to speakers
            if not is_input:
                for idx, dev in enumerate(devices):
                    api_name = hostapis[dev["hostapi"]]["name"]
                    dev_name_lower = dev["name"].lower()
                    if ("cable" in dev_name_lower or "virtual" in dev_name_lower or "vb-audio" in dev_name_lower) and dev["max_output_channels"] > 0:
                        logger.info("Auto-selected Virtual Cable for default output: %s [%s]", dev["name"], api_name)
                        return idx

        # 3. Fallback: Find the default WASAPI device
        for idx, dev in enumerate(devices):
            api_name = hostapis[dev["hostapi"]]["name"]
            if "wasapi" in api_name.lower():
                if is_input and dev["max_input_channels"] > 0:
                    return idx
                if not is_input and dev["max_output_channels"] > 0:
                    return idx

        # 4. Final Fallback
        default = sd.default.device[0] if is_input else sd.default.device[1]
        return default

    def _negotiate_sample_rate(self, input_idx, output_idx, monitor_idx, monitor_enabled):
        rates_to_try = [48000, 44100]

        devs = sd.query_devices()
        fallback_rates = []
        for idx in [input_idx, output_idx, monitor_idx]:
            if idx is not None and 0 <= idx < len(devs):
                rate = int(devs[idx]["default_samplerate"])
                if rate not in rates_to_try and rate not in fallback_rates:
                    fallback_rates.append(rate)
        rates_to_try.extend(fallback_rates)

        for rate in rates_to_try:
            try:
                # check_*_settings asks PortAudio whether the device accepts this rate
                sd.check_input_settings(device=input_idx, samplerate=rate, channels=1)
                sd.check_output_settings(device=output_idx, samplerate=rate, channels=2)
                if monitor_enabled and monitor_idx is not None:
                    sd.check_output_settings(device=monitor_idx, samplerate=rate, channels=2)

                logger.info("Sample rate negotiated: %d Hz", rate)
                return rate
            except Exception:
                continue

        if input_idx is not None and 0 <= input_idx < len(devs):
            return int(devs[input_idx]["default_samplerate"])
        return 48000

    def _processing_loop(self, session):
        logger.info("Audio processing thread started.")
        block = self.block_size
        in_ring = self._input_ring
        out_ring = self._output_ring
        mon_ring = self._monitor_ring

        while self.running.is_set() and self._session is session:
            self._frame_ready.wait(0.1)
            # Clear before draining: a write that lands after the drain re-sets the event.
            self._frame_ready.clear()

            while in_ring.available >= block and self._session is session:
                frame = in_ring.read(block)
                cfg = self._snapshot()

                start = time.perf_counter()
                try:
                    out = self._process_frame(frame, cfg)
                except Exception:
                    if not self._dsp_error_logged:
                        logger.exception("DSP frame failed; passing audio through unprocessed.")
                        self._dsp_error_logged = True
                    out = frame
                self._record_cpu(time.perf_counter() - start)

                out_ring.write(out)
                if cfg["monitor_enabled"] and self.monitor_stream is not None:
                    mon_ring.write(out)

        logger.info("Audio processing thread finished.")

    def _record_cpu(self, elapsed):
        load = 100.0 * elapsed / (self.block_size / self.sr)
        self._cpu_load += self.CPU_SMOOTHING * (load - self._cpu_load)

    def _process_frame(self, frame, cfg=None):
        if cfg is None:
            cfg = self._snapshot()
        self._apply_dsp_params(cfg)
        self.input_rms = _rms_db(frame)

        x = frame * float(cfg["input_gain"])
        if cfg["gate_enabled"]:
            x = self.gate.process(x, float(cfg["gate_threshold_db"]), float(cfg["gate_release_ms"]))
        else:
            self.gate.bypass()
        self.gate_state = self.gate.is_open

        x = self._run_noise_suppression(x, cfg)

        # De-esser runs early so sibilance is softened before EQ and compression see it
        if cfg["deesser_enabled"]:
            x = self.deesser.process(x)
        if cfg["eq_enabled"]:
            x = self.eq.process(x)
        if cfg["compressor_enabled"]:
            x = self.compressor.process(x)
        # Exciter runs late so its harmonics are not reshaped by the compressor
        if cfg["exciter_enabled"]:
            x = self.exciter.process(x)
        x = self._run_vst(x, cfg)

        lim_threshold = 10.0 ** (float(cfg["limiter_threshold_db"]) / 20.0)
        x = self.limiter.process(x, lim_threshold)
        x = x * float(cfg["output_gain"])

        self._update_clip_hold(x)
        x = np.clip(x, -1.0, 1.0).astype(np.float32, copy=False)
        self.output_rms = _rms_db(x)
        self._update_fft(x)
        return x

    def _run_noise_suppression(self, x, cfg):
        mode = cfg["ns_mode"]
        if mode not in ("high", "eco"):
            return x

        # DeepFilterNet is only valid at 48 kHz; other rates use the eco DSP.
        if mode == "high" and self.df_available and self.sr == 48000:
            try:
                strength = min(max(float(cfg["ns_high_strength"]), 0.0), 1.0)
                return self._run_deep_filter(x, strength)
            except Exception:
                if not self._df_error_logged:
                    logger.exception("DeepFilterNet frame failed; falling back to Eco DSP.")
                    self._df_error_logged = True

        return self._eco_suppress(x, float(cfg["ns_eco_strength"]))

    def _run_deep_filter(self, x, strength):
        torch = self.torch_module
        audio = torch.from_numpy(np.ascontiguousarray(x)).float().unsqueeze(0)
        with torch.no_grad():
            enhanced = self.df_enhance_fn(self.df_model, self.df_state, audio)
        if hasattr(enhanced, "detach"):
            enhanced = enhanced.detach().cpu().numpy()
        wet = _fit_length(np.asarray(enhanced, dtype=np.float32).reshape(-1), x)
        return (x + strength * (wet - x)).astype(np.float32)

    def _eco_suppress(self, x, strength):
        hop = self.eco_ns.hop_size
        n = x.shape[0]
        if n < 2 * hop:
            x = np.concatenate((x, np.zeros(2 * hop - n, dtype=np.float32)))
        first = self.eco_ns.process(x[:hop], strength=strength)
        second = self.eco_ns.process(x[hop:2 * hop], strength=strength)
        return np.concatenate((first, second))[:n]

    def _run_vst(self, x, cfg):
        plugin = self.vst_plugin
        if not cfg["vst_enabled"] or plugin is None or self.vst_failed:
            return x
        try:
            y = np.asarray(plugin(x.reshape(1, -1), self.sr), dtype=np.float32)
            if y.ndim == 2:
                y = np.mean(y, axis=0)
            return _fit_length(y.reshape(-1), x)
        except Exception:
            # Bypass until the settings change; the audio loop must not die on a plugin fault.
            self.vst_failed = True
            logger.exception("VST3 plugin failed; bypassing it until settings change.")
            return x

    def _update_clip_hold(self, x):
        peak = float(np.max(np.abs(x))) if x.size else 0.0
        if peak >= self.CLIP_LEVEL:
            self._clip_hold = self._clip_hold_frames
        elif self._clip_hold > 0:
            self._clip_hold -= 1

    def _update_fft(self, x):
        spec = np.abs(np.fft.rfft(x * self._fft_window))[:self.FFT_BINS] * self._fft_scale
        spec_norm = np.clip(20.0 * np.log10(spec + 1e-5) + 60.0, 0.0, 60.0) / 60.0
        self.fft_data = [float(v) for v in spec_norm]

    def _latency_ms(self):
        frame_ms = 1000.0 * self.block_size / self.sr
        stream_s = 0.0
        for stream in (self.input_stream, self.output_stream):
            if stream is None:
                continue
            try:
                lat = stream.latency
            except Exception:
                continue
            if isinstance(lat, (tuple, list)):
                stream_s += sum(float(v) for v in lat)
            else:
                stream_s += float(lat)
        return frame_ms + 1000.0 * stream_s

    def get_status(self):
        return {
            "input_rms": float(self.input_rms),
            "output_rms": float(self.output_rms),
            "gate_state": bool(self.gate_state),
            "fft_data": [float(v) for v in self.fft_data],
            "is_running": bool(self.running.is_set()),
            "df_available": bool(self.df_available),
            "vst_loaded": bool(self.vst_plugin is not None and not self.vst_failed),
            "vst_failed": bool(self.vst_failed),
            "sample_rate": int(self.sr),
            "latency_ms": float(self._latency_ms()),
            "cpu_load": float(min(max(self._cpu_load, 0.0), 100.0)),
            "clipping": bool(self._clip_hold > 0),
        }
