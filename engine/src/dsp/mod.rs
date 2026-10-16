//! Mono DSP chain, ported from backend/audio_processor.py `_process_frame`.
//!
//! Order: input gain -> noise gate -> noise suppression (eco) -> de-esser -> 3-band EQ ->
//! compressor -> exciter -> limiter -> output gain. The VST3 stage is not ported; the
//! Python engine still hosts VST plugins.
//!
//! All time constants are derived from the block duration, so any block length gives the
//! same behaviour as the Python 10 ms frame (up to block-level envelope granularity).
use crate::settings::Settings;
use realfft::RealFftPlanner;

pub mod biquad;
pub mod compressor;
pub mod deesser;
pub mod exciter;
pub mod eq;
pub mod gate;
pub mod limiter;
pub mod meter;
pub mod ns;

use compressor::DynamicsCompressor;
use deesser::DeEsser;
use exciter::Exciter;
use eq::Equalizer3Band;
use gate::NoiseGate;
use limiter::Limiter;
use meter::SpectrumMeter;
use ns::NoiseSuppressor;

pub const FFT_BINS: usize = 64;

const SILENCE_DB: f64 = -120.0;
const CLIP_LEVEL: f64 = 0.99;
const CLIP_HOLD_S: f64 = 1.0;
const LIMITER_RELEASE_MS: f64 = 100.0;
/// Non-finite input becomes silence; magnitudes are bounded so the filters stay finite.
const INPUT_LIMIT: f32 = 64.0;

/// Metering values, refreshed after each `process` call.
/// `fft` is the 64-bin display (0..1) using the Python `_update_fft` dB mapping.
#[derive(Debug, Clone)]
pub struct Metrics {
    pub input_rms_db: f32,
    pub output_rms_db: f32,
    pub gate_open: bool,
    pub clipping: bool,
    pub fft: [f32; FFT_BINS],
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            input_rms_db: SILENCE_DB as f32,
            output_rms_db: SILENCE_DB as f32,
            gate_open: false,
            clipping: false,
            fft: [0.0; FFT_BINS],
        }
    }
}

/// Mono processing chain. `process` must accept ANY block length (>= 1), never allocate and never
/// block, because it runs on the real-time audio thread. All buffers and FFT plans are created
/// in `new`; `apply` only recomputes filter coefficients.
pub struct Chain {
    sr: f64,
    input_gain: f64,
    output_gain: f64,
    ns_on: bool,
    ns_strength: f64,
    ns: NoiseSuppressor,
    gate_on: bool,
    gate_threshold_db: f64,
    gate_release_ms: f64,
    gate: NoiseGate,
    eq_on: bool,
    eq: Equalizer3Band,
    comp_on: bool,
    comp: DynamicsCompressor,
    deesser_on: bool,
    deesser: DeEsser,
    exciter_on: bool,
    exciter: Exciter,
    limiter: Limiter,
    limiter_threshold: f64,
    meter: SpectrumMeter,
    clip_hold: usize,
    clip_hold_samples: usize,
    metrics: Metrics,
}

impl Chain {
    /// Builds the chain for `sample_rate` (clamped to 8..384 kHz). Allocates everything up front.
    pub fn new(sample_rate: u32) -> Self {
        let sr = f64::from(sample_rate.clamp(8_000, 384_000));
        let mut planner = RealFftPlanner::<f32>::new();
        let frame_len = (sr / 100.0).round() as usize; // the Python engine's 10 ms frame
        Self {
            sr,
            input_gain: 1.0,
            output_gain: 1.0,
            ns_on: true,
            ns_strength: 0.8,
            ns: NoiseSuppressor::new(sr, &mut planner),
            gate_on: true,
            gate_threshold_db: -45.0,
            gate_release_ms: 150.0,
            gate: NoiseGate::new(sr),
            eq_on: true,
            eq: Equalizer3Band::new(sr),
            comp_on: true,
            comp: DynamicsCompressor::new(sr),
            deesser_on: false,
            deesser: DeEsser::new(sr),
            exciter_on: false,
            exciter: Exciter::new(sr),
            limiter: Limiter::new(sr, LIMITER_RELEASE_MS),
            limiter_threshold: 10f64.powf(-1.0 / 20.0),
            meter: SpectrumMeter::new(frame_len, &mut planner),
            clip_hold: 0,
            clip_hold_samples: (sr * CLIP_HOLD_S) as usize,
            metrics: Metrics::default(),
        }
    }

    /// Applies settings. Values are clamped to the ranges in backend/config.py SCHEMA; NaN and
    /// infinite values fall back to the defaults. Does not allocate.
    pub fn apply(&mut self, s: &Settings) {
        self.input_gain = num(f64::from(s.input_gain), 1.0, 0.0, 4.0);
        self.output_gain = num(f64::from(s.output_gain), 1.0, 0.0, 4.0);

        // "high" uses the eco DSP until the native DeepFilterNet path exists, as in Python.
        self.ns_on = matches!(s.ns_mode.as_str(), "eco" | "high");
        self.ns_strength = num(f64::from(s.ns_eco_strength), 0.8, 0.0, 1.0);

        self.gate_on = s.gate_enabled;
        self.gate_threshold_db = num(f64::from(s.gate_threshold_db), -45.0, -80.0, 0.0);
        self.gate_release_ms = num(f64::from(s.gate_release_ms), 150.0, 10.0, 1000.0);

        self.eq_on = s.eq_enabled;
        self.eq.set_gains(
            num(f64::from(s.eq_low_gain_db), 0.0, -12.0, 12.0),
            num(f64::from(s.eq_mid_gain_db), 0.0, -12.0, 12.0),
            num(f64::from(s.eq_high_gain_db), 0.0, -12.0, 12.0),
        );

        self.comp_on = s.compressor_enabled;
        self.comp.configure(
            num(f64::from(s.compressor_threshold_db), -18.0, -60.0, 0.0),
            num(f64::from(s.compressor_ratio), 3.0, 1.0, 20.0),
            num(f64::from(s.compressor_attack_ms), 10.0, 1.0, 200.0),
            num(f64::from(s.compressor_release_ms), 100.0, 10.0, 1000.0),
        );

        let limiter_db = num(f64::from(s.limiter_threshold_db), -1.0, -20.0, 0.0);
        self.limiter_threshold = 10f64.powf(limiter_db / 20.0);

        self.deesser_on = s.deesser_enabled;
        self.deesser.configure(
            num(f64::from(s.deesser_threshold_db), -25.0, -60.0, 0.0),
            num(f64::from(s.deesser_amount), 0.5, 0.0, 1.0),
        );

        self.exciter_on = s.exciter_enabled;
        self.exciter.configure(
            num(f64::from(s.exciter_frequency), 3000.0, 1000.0, 8000.0),
            num(f64::from(s.exciter_amount), 0.2, 0.0, 1.0),
            num(f64::from(s.exciter_mix), 0.15, 0.0, 1.0),
        );
    }

    /// Processes `input` into `output[..input.len()]`. Any block length is accepted.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let n = input.len().min(output.len());
        if n == 0 {
            return;
        }
        let out = &mut output[..n];

        for (o, &x) in out.iter_mut().zip(&input[..n]) {
            *o = if x.is_finite() { x.clamp(-INPUT_LIMIT, INPUT_LIMIT) } else { 0.0 };
        }
        let input_rms = rms_db(out);
        let gain = self.input_gain as f32;
        for o in out.iter_mut() {
            *o *= gain;
        }

        if self.gate_on {
            self.gate.process(out, self.gate_threshold_db, self.gate_release_ms);
        } else {
            self.gate.bypass();
        }
        let gate_open = self.gate.is_open();

        // "off" runs the STFT at zero strength instead of skipping it, so the latency is the same
        // in both states and toggling does not click.
        let strength = if self.ns_on { self.ns_strength } else { 0.0 };
        self.ns.process(out, strength);

        if self.deesser_on {
            self.deesser.process(out);
        }
        if self.eq_on {
            self.eq.process(out);
        }
        if self.comp_on {
            self.comp.process(out);
        }
        if self.exciter_on {
            self.exciter.process(out);
        }
        self.limiter.process(out, self.limiter_threshold);

        let out_gain = self.output_gain as f32;
        for o in out.iter_mut() {
            *o *= out_gain;
        }

        let peak = out.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        if f64::from(peak) >= CLIP_LEVEL {
            self.clip_hold = self.clip_hold_samples;
        } else {
            self.clip_hold = self.clip_hold.saturating_sub(n);
        }

        for o in out.iter_mut() {
            *o = if o.is_finite() { o.clamp(-1.0, 1.0) } else { 0.0 };
        }
        let output_rms = rms_db(out);

        if let Some(bins) = self.meter.push(out) {
            self.metrics.fft = bins;
        }
        self.metrics.input_rms_db = input_rms as f32;
        self.metrics.output_rms_db = output_rms as f32;
        self.metrics.gate_open = gate_open;
        self.metrics.clipping = self.clip_hold > 0;
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Algorithmic latency of the chain in samples (the noise suppressor's STFT delay; the other
    /// stages are zero-delay). 255 samples: 5.3 ms at 48 kHz, 5.8 ms at 44.1 kHz.
    pub fn latency_samples(&self) -> u32 {
        ns::LATENCY_SAMPLES as u32
    }

    /// Sample rate the chain was built for, in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sr as u32
    }
}

/// Clamps `v` to [lo, hi], falling back to `default` for NaN and infinite values.
fn num(v: f64, default: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { default.clamp(lo, hi) }
}

/// Length of the level-detector window: the Python engine's 10 ms frame. Detectors measure over
/// this window at every block end, so their decisions do not depend on the driver block size.
pub(crate) fn detect_len(sr: f64) -> usize {
    (sr / 100.0).round().max(1.0) as usize
}

/// Ring of squared samples over the most recent `len` samples.
pub(crate) struct SquareWindow {
    buf: Vec<f64>,
    pos: usize,
}

impl SquareWindow {
    pub fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], pos: 0 }
    }

    #[inline]
    pub fn push(&mut self, v: f64) {
        self.buf[self.pos] = v * v;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
    }

    /// Mean square over the window.
    pub fn mean(&self) -> f64 {
        self.buf.iter().sum::<f64>() / self.buf.len() as f64
    }

    /// Level in dB (10 * log10(mean square + 1e-12)), as `rms_db` but over the window.
    pub fn level_db(&self) -> f64 {
        10.0 * (self.mean() + 1e-12).log10()
    }
}

/// RMS level in dBFS, matching the Python `_rms_db` (10 * log10(mean(x^2) + 1e-12)).
pub(crate) fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return SILENCE_DB;
    }
    let mut sum = 0.0f64;
    for &v in x {
        let v = f64::from(v);
        sum += v * v;
    }
    10.0 * (sum / x.len() as f64 + 1e-12).log10()
}
