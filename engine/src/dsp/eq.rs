//! Three-band EQ: low shelf at 200 Hz, peaking at 1 kHz, high shelf at 4 kHz (Q 0.707).

use super::biquad::{high_shelf, low_shelf, peaking, Biquad};

pub struct Equalizer3Band {
    sr: f64,
    gains_db: [f64; 3],
    low: Biquad,
    mid: Biquad,
    high: Biquad,
}

impl Equalizer3Band {
    pub fn new(sr: f64) -> Self {
        Self {
            sr,
            gains_db: [0.0; 3],
            low: Biquad::new(low_shelf(sr, 200.0, 0.707, 0.0)),
            mid: Biquad::new(peaking(sr, 1000.0, 0.707, 0.0)),
            high: Biquad::new(high_shelf(sr, 4000.0, 0.707, 0.0)),
        }
    }

    /// Redesigns the filters only when a gain changed. Filter state is kept.
    pub fn set_gains(&mut self, low_db: f64, mid_db: f64, high_db: f64) {
        let gains = [low_db, mid_db, high_db];
        if gains == self.gains_db {
            return;
        }
        self.gains_db = gains;
        self.low.set_coeffs(low_shelf(self.sr, 200.0, 0.707, low_db));
        self.mid.set_coeffs(peaking(self.sr, 1000.0, 0.707, mid_db));
        self.high.set_coeffs(high_shelf(self.sr, 4000.0, 0.707, high_db));
    }

    pub fn process(&mut self, data: &mut [f32]) {
        for s in data.iter_mut() {
            let y = self.low.tick(f64::from(*s));
            let y = self.mid.tick(y);
            *s = self.high.tick(y) as f32;
        }
        self.low.flush_denormals();
        self.mid.flush_denormals();
        self.high.flush_denormals();
    }
}
