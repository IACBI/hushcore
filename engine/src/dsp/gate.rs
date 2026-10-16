//! Noise gate with a 2 ms attack, a hold time and a configurable release.

use super::{detect_len, SquareWindow};

pub const ATTACK_MS: f64 = 2.0;
pub const HOLD_MS: f64 = 20.0;

pub struct NoiseGate {
    sr: f64,
    gain: f64,
    is_open: bool,
    hold_left: usize,
    hold_samples: usize,
    level: SquareWindow,
}

impl NoiseGate {
    pub fn new(sr: f64) -> Self {
        Self {
            sr,
            gain: 0.0,
            is_open: false,
            hold_left: 0,
            hold_samples: (HOLD_MS * sr / 1000.0).round() as usize,
            level: SquareWindow::new(detect_len(sr)),
        }
    }

    /// Passes audio unchanged. Reports the gate as open, as the Python `bypass` does.
    pub fn bypass(&mut self) {
        self.gain = 1.0;
        self.is_open = true;
        self.hold_left = 0;
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    pub fn process(&mut self, data: &mut [f32], threshold_db: f64, release_ms: f64) {
        if data.is_empty() {
            return;
        }
        let n = data.len();
        let dt_ms = 1000.0 * n as f64 / self.sr;

        for &s in data.iter() {
            self.level.push(f64::from(s));
        }
        let target = if self.level.level_db() > threshold_db {
            self.hold_left = self.hold_samples;
            1.0
        } else if self.hold_left > 0 {
            self.hold_left = self.hold_left.saturating_sub(n);
            1.0
        } else {
            0.0
        };
        self.is_open = target > 0.5;

        let alpha = if target > self.gain {
            (-dt_ms / ATTACK_MS).exp()
        } else {
            (-dt_ms / release_ms.max(1e-3)).exp()
        };
        self.gain = alpha * self.gain + (1.0 - alpha) * target;

        let gain = self.gain as f32;
        for s in data.iter_mut() {
            *s *= gain;
        }
    }
}
