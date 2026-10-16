//! DSP chain. STUB: pass-through with the final public contract; the DSP agent replaces the internals.
use crate::settings::Settings;

pub const FFT_BINS: usize = 64;

#[derive(Debug, Clone)]
pub struct Metrics {
    pub input_rms_db: f32,
    pub output_rms_db: f32,
    pub gate_open: bool,
    pub clipping: bool,
    pub fft: [f32; FFT_BINS], // 0..1, same mapping as backend/audio_processor.py _update_fft
}

impl Default for Metrics {
    fn default() -> Self {
        Self { input_rms_db: -120.0, output_rms_db: -120.0, gate_open: false, clipping: false, fft: [0.0; FFT_BINS] }
    }
}

/// Mono processing chain. `process` must accept ANY block length (>= 1), never allocate and never block,
/// because it runs on the real-time audio thread.
pub struct Chain {
    metrics: Metrics,
}

impl Chain {
    pub fn new(_sample_rate: u32) -> Self {
        Self { metrics: Metrics::default() }
    }
    pub fn apply(&mut self, _settings: &Settings) {}
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        output[..input.len()].copy_from_slice(input);
    }
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }
    /// Algorithmic latency of the chain in samples (look-ahead + STFT delay), reported to the UI.
    pub fn latency_samples(&self) -> u32 {
        0
    }
}
