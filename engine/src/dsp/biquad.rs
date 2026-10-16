//! RBJ Audio EQ Cookbook biquads, designed in float64 as in backend/audio_processor.py.

use std::f64::consts::PI;

/// Normalised biquad coefficients (a0 is folded into the others).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    fn normalised(b: [f64; 3], a: [f64; 3]) -> Self {
        let a0 = a[0];
        Self {
            b0: b[0] / a0,
            b1: b[1] / a0,
            b2: b[2] / a0,
            a1: a[1] / a0,
            a2: a[2] / a0,
        }
    }
}

/// Low shelf, `gain_db` at DC.
pub fn low_shelf(sr: f64, f0: f64, q: f64, gain_db: f64) -> Coeffs {
    let a = 10f64.powf(gain_db / 40.0);
    let (sn, cs) = (2.0 * PI * f0 / sr).sin_cos();
    let alpha = sn / 2.0 * ((a + 1.0 / a) * (1.0 / q - 1.0) + 2.0).sqrt();
    let sa = 2.0 * a.sqrt() * alpha;
    Coeffs::normalised(
        [
            a * ((a + 1.0) - (a - 1.0) * cs + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
            a * ((a + 1.0) - (a - 1.0) * cs - sa),
        ],
        [
            (a + 1.0) + (a - 1.0) * cs + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * cs),
            (a + 1.0) + (a - 1.0) * cs - sa,
        ],
    )
}

/// High shelf, `gain_db` at Nyquist.
pub fn high_shelf(sr: f64, f0: f64, q: f64, gain_db: f64) -> Coeffs {
    let a = 10f64.powf(gain_db / 40.0);
    let (sn, cs) = (2.0 * PI * f0 / sr).sin_cos();
    let alpha = sn / 2.0 * ((a + 1.0 / a) * (1.0 / q - 1.0) + 2.0).sqrt();
    let sa = 2.0 * a.sqrt() * alpha;
    Coeffs::normalised(
        [
            a * ((a + 1.0) + (a - 1.0) * cs + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
            a * ((a + 1.0) + (a - 1.0) * cs - sa),
        ],
        [
            (a + 1.0) - (a - 1.0) * cs + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * cs),
            (a + 1.0) - (a - 1.0) * cs - sa,
        ],
    )
}

/// Peaking EQ bell of `gain_db` centred on `f0`.
pub fn peaking(sr: f64, f0: f64, q: f64, gain_db: f64) -> Coeffs {
    let a = 10f64.powf(gain_db / 40.0);
    let (sn, cs) = (2.0 * PI * f0 / sr).sin_cos();
    let alpha = sn / (2.0 * q);
    Coeffs::normalised(
        [1.0 + alpha * a, -2.0 * cs, 1.0 - alpha * a],
        [1.0 + alpha / a, -2.0 * cs, 1.0 - alpha / a],
    )
}

/// Constant 0 dB peak-gain band-pass.
pub fn bandpass(sr: f64, f0: f64, q: f64) -> Coeffs {
    let (sn, cs) = (2.0 * PI * f0 / sr).sin_cos();
    let alpha = sn / (2.0 * q);
    Coeffs::normalised([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cs, 1.0 - alpha])
}

/// Second-order high-pass. `f0` is clamped to [10 Hz, 0.45 sr] as in the Python design.
pub fn highpass(sr: f64, f0: f64, q: f64) -> Coeffs {
    let f0 = f0.clamp(10.0, 0.45 * sr);
    let (sn, cs) = (2.0 * PI * f0 / sr).sin_cos();
    let alpha = sn / (2.0 * q);
    Coeffs::normalised(
        [(1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0],
        [1.0 + alpha, -2.0 * cs, 1.0 - alpha],
    )
}

/// Transposed direct-form II section, the same structure scipy's lfilter uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    c: Coeffs,
    z1: f64,
    z2: f64,
}

/// Below this magnitude a state value is flushed to zero so decaying tails never reach
/// the (very slow) subnormal range.
const FLUSH: f64 = 1e-30;

impl Biquad {
    pub fn new(c: Coeffs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    /// Swaps coefficients and keeps the state, so a redesign does not click.
    pub fn set_coeffs(&mut self, c: Coeffs) {
        self.c = c;
    }

    /// Initialises the state to the steady response to a constant input `x0`, equivalent to
    /// scipy.signal.lfilter_zi(b, a) * x0. Used when a filter engages on a non-zero signal.
    pub fn seed_steady(&mut self, x0: f64) {
        let c = self.c;
        let dc = (c.b0 + c.b1 + c.b2) / (1.0 + c.a1 + c.a2);
        self.z1 = (c.b1 + c.b2 - (c.a1 + c.a2) * dc) * x0;
        self.z2 = (c.b2 - c.a2 * dc) * x0;
    }

    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y
    }

    /// Call once per block; keeps the poles' decay out of the subnormal range.
    pub fn flush_denormals(&mut self) {
        if self.z1.abs() < FLUSH {
            self.z1 = 0.0;
        }
        if self.z2.abs() < FLUSH {
            self.z2 = 0.0;
        }
    }
}
