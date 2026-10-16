//! De-esser: sibilance band (6 kHz band-pass) drives a 5 kHz high shelf cut.

use super::biquad::{bandpass, high_shelf, Biquad};
use super::{detect_len, SquareWindow};

const SHELF_FREQ: f64 = 5000.0;
const SIB_FREQ: f64 = 6000.0;
const ENV_RELEASE_MS: f64 = 15.0;
/// Below this the shelf would be an audible no-op; it is bypassed instead of run as identity.
const BYPASS_DB: f64 = 0.05;

pub struct DeEsser {
    sr: f64,
    threshold_db: f64,
    amount: f64,
    sib_bp: Biquad,
    shelf: Biquad,
    shelf_db: f64,
    engaged: bool,
    env: f64,
    level: SquareWindow,
}

impl DeEsser {
    pub fn new(sr: f64) -> Self {
        Self {
            sr,
            threshold_db: -25.0,
            amount: 0.5,
            sib_bp: Biquad::new(bandpass(sr, SIB_FREQ, 1.0)),
            shelf: Biquad::default(),
            shelf_db: 0.0,
            engaged: false,
            env: 0.0,
            level: SquareWindow::new(detect_len(sr)),
        }
    }

    pub fn configure(&mut self, threshold_db: f64, amount: f64) {
        self.threshold_db = threshold_db;
        self.amount = amount;
    }

    pub fn process(&mut self, data: &mut [f32]) {
        let n = data.len();
        if n == 0 {
            return;
        }
        let dt_ms = 1000.0 * n as f64 / self.sr;

        // The sibilance band is measured in a first pass; the shelf runs in a second pass.
        for s in data.iter() {
            let y = self.sib_bp.tick(f64::from(*s));
            self.level.push(y);
        }
        let sib_rms = self.level.mean().sqrt();

        let env_alpha = (-dt_ms / ENV_RELEASE_MS).exp();
        self.env = env_alpha * self.env + (1.0 - env_alpha) * sib_rms;
        let env_db = 20.0 * (self.env + 1e-12).log10();

        let mut reduction_db = 0.0;
        if env_db > self.threshold_db {
            let excess_db = env_db - self.threshold_db;
            reduction_db = (-excess_db * self.amount).max(-15.0);
        }

        if reduction_db.abs() < BYPASS_DB {
            // The shelf state is not advanced while bypassed; it is re-seeded on re-engage.
            self.engaged = false;
            self.sib_bp.flush_denormals();
            return;
        }

        if !self.engaged {
            // Seed from the current input so engaging does not click.
            self.shelf.set_coeffs(high_shelf(self.sr, SHELF_FREQ, 0.707, reduction_db));
            self.shelf_db = reduction_db;
            self.shelf.seed_steady(f64::from(data[0]));
            self.engaged = true;
        } else if (reduction_db - self.shelf_db).abs() >= BYPASS_DB {
            self.shelf.set_coeffs(high_shelf(self.sr, SHELF_FREQ, 0.707, reduction_db));
            self.shelf_db = reduction_db;
        }

        for s in data.iter_mut() {
            *s = self.shelf.tick(f64::from(*s)) as f32;
        }
        self.shelf.flush_denormals();
        self.sib_bp.flush_denormals();
    }
}
