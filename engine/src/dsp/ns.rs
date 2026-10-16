//! Eco noise suppression: spectral subtraction with minimum-statistics noise tracking.
//!
//! Port of SpectralSubtractedNoiseSuppression in backend/audio_processor.py, re-designed for
//! low latency. The Python engine uses a 480-sample sine-windowed STFT (10 ms latency). Here
//! the window is 256 samples with a 128-sample hop (sine window at analysis and synthesis,
//! which sums to exactly 1 under 50% overlap), giving 255 samples (5.3 ms at 48 kHz) of
//! delay. Frequency resolution halves (187.5 Hz bins at 48 kHz), so the neighbourhood and
//! dilation widths are re-expressed in Hz. Per-frame constants that were tuned for 5 ms hops
//! are converted to time constants.
//!
//! Strength 0 gives gain 1 in every bin: the STFT then reconstructs its input exactly, delayed
//! by LATENCY_SAMPLES.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::f64::consts::PI;
use std::sync::Arc;

pub const WINDOW: usize = 256;
pub const HOP: usize = 128;
pub const BINS: usize = WINDOW / 2 + 1;
/// Delay from input to output: the first sample of a hop is complete only once the whole
/// window has arrived.
pub const LATENCY_SAMPLES: usize = WINDOW - 1;

const FIFO_MASK: usize = 511;

// Constants from the Python class. Times are in seconds; per-frame weights were tuned at a
// 5 ms hop (240 samples at 48 kHz) and are converted to time constants below.
const BIAS: f64 = 2.75;
const BIAS_CURVE_S: [f64; 5] = [0.0, 0.05, 0.2, 0.5, 1.5];
const BIAS_CURVE: [f64; 5] = [0.35, 0.53, 0.70, 0.80, 1.0];
const OVER_SUBTRACTION: f64 = 1.5;
const FLOOR_DB: f64 = 20.0;
const WINDOW_S: f64 = 1.5;
const SUBWINDOW_S: f64 = 0.25;
const WARMUP_S: f64 = 0.3;
const SETTLE_S: f64 = 0.1;
const POWER_SMOOTHING_PER_5MS: f64 = 0.8;
const GAIN_ATTACK_PER_5MS: f64 = 0.3;
const GAIN_RELEASE_PER_5MS: f64 = 0.7;
const REF_FRAME_S: f64 = 0.005;
const TONAL_RATIO: f64 = 10.0;
/// 6 bins of 187.5 Hz, close to the Python 12 bins of 100 Hz.
const NEIGHBOUR_HALF_BINS: usize = 6;
/// About 375 Hz either side of a tonal peak.
const TONAL_DILATE_BINS: usize = 2;
const POWER_EPS: f64 = 1e-12;
const STATE_FLUSH: f64 = 1e-30;

pub struct NoiseSuppressor {
    hop_s: f64,
    window: Vec<f32>,
    synth: Vec<f32>,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    fwd_scratch: Vec<Complex<f32>>,
    inv_scratch: Vec<Complex<f32>>,
    spec: Vec<Complex<f32>>,
    /// Sliding analysis frame of the last WINDOW input samples.
    frame: Vec<f32>,
    /// FFT input/output buffer; the realfft calls overwrite it.
    work: Vec<f32>,
    ola: Vec<f32>,
    hop_in: Vec<f32>,
    hop_fill: usize,
    /// Output FIFO; primed with HOP-1 zeros so the delay is exactly LATENCY_SAMPLES.
    fifo: [f32; FIFO_MASK + 1],
    fifo_head: usize,
    fifo_len: usize,
    strength: f64,

    settle_frames: usize,
    warmup_frames: f64,
    sub_len: usize,
    n_sub: usize,
    power_alpha: f64,
    weight_attack: f64,
    weight_release: f64,
    frames: usize,
    pending_fill: bool,

    power_smooth: Vec<f64>,
    cur_min: Vec<f64>,
    sub_min: Vec<f64>,
    sub_idx: usize,
    sub_count: usize,
    noise: Vec<f64>,
    floor_nb: Vec<f64>,
    gain_pow: Vec<f64>,
    gain: Vec<f64>,
}

impl NoiseSuppressor {
    pub fn new(sr: f64, planner: &mut RealFftPlanner<f32>) -> Self {
        let hop_s = HOP as f64 / sr;
        let fwd = planner.plan_fft_forward(WINDOW);
        let inv = planner.plan_fft_inverse(WINDOW);
        let window: Vec<f32> = (0..WINDOW)
            .map(|i| (PI * (i as f64 + 0.5) / WINDOW as f64).sin() as f32)
            .collect();
        let inv_n = 1.0 / WINDOW as f32;
        let synth = window.iter().map(|&w| w * inv_n).collect();
        let frame_ratio = hop_s / REF_FRAME_S;
        let n_sub = (WINDOW_S / SUBWINDOW_S).round().max(1.0) as usize;
        Self {
            hop_s,
            window,
            synth,
            fwd_scratch: fwd.make_scratch_vec(),
            inv_scratch: inv.make_scratch_vec(),
            spec: fwd.make_output_vec(),
            fwd,
            inv,
            frame: vec![0.0; WINDOW],
            work: vec![0.0; WINDOW],
            ola: vec![0.0; WINDOW],
            hop_in: vec![0.0; HOP],
            hop_fill: 0,
            fifo: [0.0; FIFO_MASK + 1],
            fifo_head: 0,
            fifo_len: HOP - 1,
            strength: 0.8,
            settle_frames: (SETTLE_S * sr / HOP as f64).round().max(1.0) as usize,
            warmup_frames: (WARMUP_S * sr / HOP as f64).max(1.0),
            sub_len: (SUBWINDOW_S * sr / HOP as f64).round().max(1.0) as usize,
            n_sub,
            power_alpha: POWER_SMOOTHING_PER_5MS.powf(frame_ratio),
            weight_attack: GAIN_ATTACK_PER_5MS.powf(frame_ratio),
            weight_release: GAIN_RELEASE_PER_5MS.powf(frame_ratio),
            frames: 0,
            pending_fill: true,
            power_smooth: vec![0.0; BINS],
            cur_min: vec![f64::INFINITY; BINS],
            sub_min: vec![f64::INFINITY; n_sub * BINS],
            sub_idx: 0,
            sub_count: 0,
            noise: vec![0.0; BINS],
            floor_nb: vec![0.0; BINS],
            gain_pow: vec![1.0; BINS],
            gain: vec![1.0; BINS],
        }
    }

    /// Processes a block in place. `strength` is clamped to [0, 1]; 0 is an exact delayed pass-through.
    pub fn process(&mut self, data: &mut [f32], strength: f64) {
        // NaN must not reach `clamp` below, which panics on a NaN bound.
        self.strength = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 0.0 };
        for s in data.iter_mut() {
            self.hop_in[self.hop_fill] = *s;
            self.hop_fill += 1;
            if self.hop_fill == HOP {
                self.hop_fill = 0;
                self.run_hop();
            }
            *s = self.fifo_pop();
        }
    }

    fn fifo_pop(&mut self) -> f32 {
        // Cannot underflow: the FIFO holds HOP-1 zeros and gains HOP samples per hop, so it
        // never runs empty when a sample is popped.
        let v = self.fifo[self.fifo_head];
        self.fifo_head = (self.fifo_head + 1) & FIFO_MASK;
        self.fifo_len = self.fifo_len.saturating_sub(1);
        v
    }

    fn fifo_push(&mut self, v: f32) {
        let idx = (self.fifo_head + self.fifo_len) & FIFO_MASK;
        self.fifo[idx] = v;
        self.fifo_len += 1;
    }

    fn run_hop(&mut self) {
        self.frame.copy_within(HOP.., 0);
        self.frame[WINDOW - HOP..].copy_from_slice(&self.hop_in);

        for ((w, &x), &h) in self.work.iter_mut().zip(&self.frame).zip(&self.window) {
            *w = x * h;
        }
        let _ = self.fwd.process_with_scratch(&mut self.work, &mut self.spec, &mut self.fwd_scratch);

        if self.pending_fill {
            // The window is still filling with zeros; a partial frame would drag the
            // minimum tracker down, so this hop passes through untouched.
            self.pending_fill = false;
        } else {
            self.suppress();
        }

        let _ = self.inv.process_with_scratch(&mut self.spec, &mut self.work, &mut self.inv_scratch);
        for ((o, &x), &w) in self.ola.iter_mut().zip(&self.work).zip(&self.synth) {
            *o += x * w;
        }
        for i in 0..HOP {
            let v = self.ola[i];
            self.fifo_push(v);
        }
        self.ola.copy_within(HOP.., 0);
        self.ola[WINDOW - HOP..].fill(0.0);
    }

    fn suppress(&mut self) {
        let frames = self.frames;
        let s = self.strength;

        for k in 0..BINS {
            let p = f64::from(self.spec[k].norm_sqr());
            let ps = if frames == 0 {
                p
            } else {
                self.power_alpha * self.power_smooth[k] + (1.0 - self.power_alpha) * p
            };
            self.power_smooth[k] = if ps < STATE_FLUSH { 0.0 } else { ps };
        }

        let tracked_s = frames.saturating_sub(self.settle_frames) as f64 * self.hop_s;
        let bias = BIAS * interp(tracked_s, &BIAS_CURVE_S, &BIAS_CURVE);
        self.update_noise(frames, bias);

        // Tonal bins take the local noise floor, so a steady tone is not treated as noise.
        for k in 0..BINS {
            let lo = k.saturating_sub(NEIGHBOUR_HALF_BINS);
            let hi = (k + NEIGHBOUR_HALF_BINS).min(BINS - 1);
            self.floor_nb[k] = self.noise[lo..=hi].iter().fold(f64::INFINITY, |m, &v| m.min(v));
        }

        let over = OVER_SUBTRACTION * s;
        let floor_pow = 10f64.powf(-FLOOR_DB * s / 10.0);
        for k in 0..BINS {
            let lo = k.saturating_sub(TONAL_DILATE_BINS);
            let hi = (k + TONAL_DILATE_BINS).min(BINS - 1);
            let tonal = (lo..=hi).any(|j| self.power_smooth[j] > TONAL_RATIO * self.floor_nb[j]);
            let noise_eff = if tonal { self.floor_nb[k] } else { self.noise[k] };
            let p = f64::from(self.spec[k].norm_sqr());
            self.gain_pow[k] = (1.0 - over * noise_eff / (p + POWER_EPS)).clamp(floor_pow, 1.0);
        }

        for k in 0..BINS {
            let g = &self.gain_pow;
            let smoothed = if k == 0 || k + 1 == BINS {
                g[k]
            } else {
                0.25 * g[k - 1] + 0.5 * g[k] + 0.25 * g[k + 1]
            };
            let w = if smoothed < self.gain[k] { self.weight_attack } else { self.weight_release };
            self.gain[k] = w * self.gain[k] + (1.0 - w) * smoothed;
        }

        // Raised-cosine fade-in so suppression ramps up while the noise estimate settles.
        let ramp = 0.5 * (1.0 - (PI * (frames as f64 / self.warmup_frames).min(1.0)).cos());
        self.frames += 1;
        for k in 0..BINS {
            let applied = (1.0 - ramp * (1.0 - self.gain[k])).max(0.0);
            self.spec[k] = self.spec[k].scale(applied.sqrt() as f32);
        }
    }

    /// Minimum-statistics noise floor: the running minimum over the last WINDOW_S seconds
    /// of smoothed power, scaled by `bias`.
    fn update_noise(&mut self, frames: usize, bias: f64) {
        if frames < self.settle_frames {
            // The smoother is still converging, so its early minima would read too low.
            self.noise.fill(0.0);
            return;
        }
        for k in 0..BINS {
            self.cur_min[k] = self.cur_min[k].min(self.power_smooth[k]);
        }
        self.sub_count += 1;
        if self.sub_count >= self.sub_len {
            let row = self.sub_idx * BINS;
            self.sub_min[row..row + BINS].copy_from_slice(&self.cur_min);
            self.sub_idx = (self.sub_idx + 1) % self.n_sub;
            self.cur_min.fill(f64::INFINITY);
            self.sub_count = 0;
        }
        for k in 0..BINS {
            let mut m = self.cur_min[k];
            for r in 0..self.n_sub {
                m = m.min(self.sub_min[r * BINS + k]);
            }
            self.noise[k] = bias * m;
        }
    }
}

/// Piecewise-linear interpolation, clamped at both ends (numpy.interp semantics).
fn interp(x: f64, xs: &[f64], ys: &[f64]) -> f64 {
    let last = xs.len() - 1;
    if x <= xs[0] {
        return ys[0];
    }
    if x >= xs[last] {
        return ys[last];
    }
    for i in 0..last {
        if x <= xs[i + 1] {
            let t = (x - xs[i]) / (xs[i + 1] - xs[i]);
            return ys[i] + t * (ys[i + 1] - ys[i]);
        }
    }
    ys[last]
}
