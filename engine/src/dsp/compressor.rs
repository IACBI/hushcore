//! Feed-forward RMS compressor with a block-level envelope (dB domain).

use super::{detect_len, SquareWindow};

pub struct DynamicsCompressor {
    sr: f64,
    threshold_db: f64,
    ratio: f64,
    attack_ms: f64,
    release_ms: f64,
    env_db: f64,
    level: SquareWindow,
}

impl DynamicsCompressor {
    pub fn new(sr: f64) -> Self {
        Self {
            sr,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            env_db: -60.0,
            level: SquareWindow::new(detect_len(sr)),
        }
    }

    /// Times are in milliseconds. `apply` in the chain clamps them to the config ranges.
    pub fn configure(&mut self, threshold_db: f64, ratio: f64, attack_ms: f64, release_ms: f64) {
        self.threshold_db = threshold_db;
        self.ratio = ratio;
        self.attack_ms = attack_ms.max(1e-3);
        self.release_ms = release_ms.max(1e-3);
    }

    /// Applies one constant gain to the whole block, computed from the block's envelope.
    /// The envelope is advanced by the block duration; the level is the last 10 ms.
    pub fn process(&mut self, data: &mut [f32]) {
        if data.is_empty() {
            return;
        }
        let dt_ms = 1000.0 * data.len() as f64 / self.sr;
        for &s in data.iter() {
            self.level.push(f64::from(s));
        }
        let rms = self.level.level_db();

        let time_ms = if rms > self.env_db { self.attack_ms } else { self.release_ms };
        let alpha = (-dt_ms / time_ms).exp();
        self.env_db = alpha * self.env_db + (1.0 - alpha) * rms;

        let mut gain_db = 0.0;
        if self.env_db > self.threshold_db {
            let excess_db = self.env_db - self.threshold_db;
            gain_db = excess_db * (1.0 / self.ratio.max(1.0) - 1.0);
        }
        let gain = 10f64.powf(gain_db / 20.0) as f32;
        for s in data.iter_mut() {
            *s *= gain;
        }
    }
}
