//! Vocal exciter: high-passed signal driven through tanh, mixed back in above the cutoff.

use super::biquad::{highpass, Biquad};

pub struct Exciter {
    sr: f64,
    freq: f64,
    amount: f64,
    mix: f64,
    hp: Biquad,
}

impl Exciter {
    pub fn new(sr: f64) -> Self {
        Self { sr, freq: 3000.0, amount: 0.2, mix: 0.15, hp: Biquad::new(highpass(sr, 3000.0, 0.707)) }
    }

    pub fn configure(&mut self, freq: f64, amount: f64, mix: f64) {
        if freq != self.freq {
            self.freq = freq;
            self.hp.set_coeffs(highpass(self.sr, freq, 0.707));
        }
        self.amount = amount;
        self.mix = mix;
    }

    pub fn process(&mut self, data: &mut [f32]) {
        let drive = 1.0 + self.amount * 5.0;
        for s in data.iter_mut() {
            let x = f64::from(*s);
            let excited = (self.hp.tick(x) * drive).tanh();
            *s = (x * (1.0 - self.mix) + excited * self.mix) as f32;
        }
        self.hp.flush_denormals();
    }
}
