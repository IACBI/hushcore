//! 64-bin spectrum for the UI, matching the Python `_update_fft` (Hann window, 2/sum(w) scale,
//! 20*log10 mapping over 60 dB). A frame is the Python engine's 10 ms block.

use super::FFT_BINS;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::f64::consts::PI;
use std::sync::Arc;

pub struct SpectrumMeter {
    len: usize,
    fill: usize,
    frame: Vec<f32>,
    window: Vec<f32>,
    scale: f64,
    fft: Arc<dyn RealToComplex<f32>>,
    scratch: Vec<Complex<f32>>,
    work: Vec<f32>,
    spec: Vec<Complex<f32>>,
}

impl SpectrumMeter {
    pub fn new(len: usize, planner: &mut RealFftPlanner<f32>) -> Self {
        let len = len.max(2);
        // numpy.hanning(M): symmetric Hann.
        let window: Vec<f32> = (0..len)
            .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / (len - 1) as f64).cos()) as f32)
            .collect();
        let sum: f64 = window.iter().map(|&w| f64::from(w)).sum();
        let fft = planner.plan_fft_forward(len);
        Self {
            len,
            fill: 0,
            frame: vec![0.0; len],
            window,
            scale: 2.0 / sum,
            scratch: fft.make_scratch_vec(),
            work: vec![0.0; len],
            spec: fft.make_output_vec(),
            fft,
        }
    }

    /// Feeds output samples. Returns the bins of the last frame completed by this block.
    pub fn push(&mut self, data: &[f32]) -> Option<[f32; FFT_BINS]> {
        let mut latest = None;
        for &s in data {
            self.frame[self.fill] = s;
            self.fill += 1;
            if self.fill == self.len {
                self.fill = 0;
                latest = Some(self.analyse());
            }
        }
        latest
    }

    fn analyse(&mut self) -> [f32; FFT_BINS] {
        for ((w, &x), &h) in self.work.iter_mut().zip(&self.frame).zip(&self.window) {
            *w = x * h;
        }
        // Errors only come from mismatched lengths, which the constructor rules out.
        let _ = self.fft.process_with_scratch(&mut self.work, &mut self.spec, &mut self.scratch);

        let mut bins = [0.0f32; FFT_BINS];
        for (k, out) in bins.iter_mut().enumerate().take(self.spec.len()) {
            let mag = f64::from(self.spec[k].norm()) * self.scale;
            let db = 20.0 * (mag + 1e-5).log10() + 60.0;
            *out = (db.clamp(0.0, 60.0) / 60.0) as f32;
        }
        bins
    }
}
