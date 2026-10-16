//! Peak limiter: instant attack to the ceiling for the block, exponential release afterwards.

pub struct Limiter {
    sr: f64,
    release_ms: f64,
    gain: f64,
}

impl Limiter {
    pub fn new(sr: f64, release_ms: f64) -> Self {
        Self { sr, release_ms: release_ms.max(1e-3), gain: 1.0 }
    }

    /// `threshold_lin` is the linear ceiling and must be positive.
    pub fn process(&mut self, data: &mut [f32], threshold_lin: f64) {
        if data.is_empty() {
            return;
        }
        let dt_ms = 1000.0 * data.len() as f64 / self.sr;
        let peak = data.iter().fold(0.0f64, |m, &v| m.max(f64::from(v.abs())));
        let target = if peak <= threshold_lin { 1.0 } else { threshold_lin / peak };

        if target < self.gain {
            self.gain = target;
        } else {
            let alpha = (-dt_ms / self.release_ms).exp();
            self.gain = alpha * self.gain + (1.0 - alpha) * target;
        }

        let gain = self.gain as f32;
        for s in data.iter_mut() {
            *s *= gain;
        }
    }
}
