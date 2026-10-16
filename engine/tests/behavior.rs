//! Behaviour tests for the eco noise suppressor and the full chain: tone and noise separation,
//! silence and non-finite input, block-size independence, latency, metering and clipping.

use hushcore_engine::dsp::ns::{NoiseSuppressor, LATENCY_SAMPLES};
use hushcore_engine::{Chain, Settings};
use realfft::RealFftPlanner;
use std::f64::consts::PI;

/// Small deterministic generator (xorshift64*), so the tests need no RNG crate.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal sample (Box-Muller).
    fn gauss(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-300);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

fn white_noise(rng: &mut Rng, n: usize, sigma: f64) -> Vec<f32> {
    (0..n).map(|_| (sigma * rng.gauss()) as f32).collect()
}

fn sine(n: usize, freq: f64, amp: f64, sr: f64) -> Vec<f32> {
    (0..n).map(|i| (amp * (2.0 * PI * freq * i as f64 / sr).sin()) as f32).collect()
}

fn add(a: &mut [f32], b: &[f32]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x += *y;
    }
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Amplitude of the `freq` component over `x`, by correlation with a complex exponential.
fn tone_amplitude(x: &[f32], freq: f64, sr: f64) -> f64 {
    let w = 2.0 * PI * freq / sr;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let phase = w * i as f64;
        re += f64::from(v) * phase.cos();
        im -= f64::from(v) * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn run_ns(ns: &mut NoiseSuppressor, x: &[f32], strength: f64, block: usize) -> Vec<f32> {
    let mut y = x.to_vec();
    for b in y.chunks_mut(block) {
        ns.process(b, strength);
    }
    y
}

fn new_ns(sr: f64) -> NoiseSuppressor {
    NoiseSuppressor::new(sr, &mut RealFftPlanner::<f32>::new())
}

#[test]
fn eco_ns_keeps_tone_and_removes_noise() {
    for sr in [44_100.0, 48_000.0, 96_000.0] {
        check_ns_tone_and_noise(sr);
    }
}

fn check_ns_tone_and_noise(sr: f64) {
    let n = (3.0 * sr) as usize;
    let warm = (0.5 * sr) as usize;
    let lat = LATENCY_SAMPLES;
    let mut rng = Rng(0x5EED_0001);

    // Tone at -20 dBFS (amplitude 0.1) over white noise at -60 dBFS RMS.
    let mut x = sine(n, 440.0, 0.1, sr);
    add(&mut x, &white_noise(&mut rng, n, 0.001));
    let y = run_ns(&mut new_ns(sr), &x, 0.8, 480);

    // Output sample i is input sample i - LATENCY_SAMPLES.
    let tone_in = tone_amplitude(&x[warm..n - lat], 440.0, sr);
    let tone_out = tone_amplitude(&y[warm + lat..n], 440.0, sr);
    let energy_kept = (tone_out / tone_in).powi(2);
    eprintln!("sr {sr}: tone energy kept after 0.5 s: {:.1}%", 100.0 * energy_kept);
    assert!(energy_kept >= 0.8, "sr {sr}: tone energy kept {energy_kept:.3} < 0.8");

    // Noise only: RMS over the last 1.5 s, after the tracker has converged.
    let mut rng = Rng(0x5EED_0002);
    let noise = white_noise(&mut rng, n, 0.001);
    let y = run_ns(&mut new_ns(sr), &noise, 0.8, 480);
    let from = (1.5 * sr) as usize;
    let drop_db = 20.0 * (rms(&noise[from..n - lat]) / rms(&y[from + lat..n])).log10();
    eprintln!("sr {sr}: noise-only RMS drop at strength 0.8: {drop_db:.1} dB");
    assert!(drop_db >= 6.0, "sr {sr}: noise RMS dropped only {drop_db:.2} dB");
}

#[test]
fn eco_ns_strength_zero_is_delayed_passthrough() {
    let n = 4800;
    let mut rng = Rng(7);
    let x = white_noise(&mut rng, n, 0.2);
    let mut ns = new_ns(48_000.0);
    let y = run_ns(&mut ns, &x, 0.0, 480);
    let max_err = (LATENCY_SAMPLES..n)
        .map(|i| (y[i] - x[i - LATENCY_SAMPLES]).abs())
        .fold(0.0f32, f32::max);
    assert!(max_err < 1e-5, "strength 0 changed the signal by {max_err:e}");
}

#[test]
fn eco_ns_is_block_size_independent() {
    let n = 96_000;
    let mut rng = Rng(11);
    let mut x = sine(n, 1000.0, 0.05, 48_000.0);
    add(&mut x, &white_noise(&mut rng, n, 0.01));

    let reference = run_ns(&mut new_ns(48_000.0), &x, 0.8, 480);
    for block in [64, 128, 441] {
        let y = run_ns(&mut new_ns(48_000.0), &x, 0.8, block);
        let max_err = reference.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max_err == 0.0, "block {block}: NS output differs by {max_err:e}");
    }
}

#[test]
fn silence_in_gives_silence_out_without_nan() {
    let y = run_ns(&mut new_ns(48_000.0), &[0.0; 48_000], 0.8, 480);
    assert!(y.iter().all(|&v| v == 0.0));

    let mut chain = Chain::new(48_000);
    chain.apply(&Settings::default());
    let zeros = vec![0.0f32; 480];
    let mut out = vec![1.0f32; 480];
    for _ in 0..200 {
        chain.process(&zeros, &mut out);
        assert!(out.iter().all(|&v| v == 0.0), "silence produced non-zero output");
    }
    let m = chain.metrics();
    assert!(m.input_rms_db <= -119.0 && m.output_rms_db <= -119.0);
    assert!(m.fft.iter().all(|v| v.is_finite() && *v == 0.0));
    assert!(!m.clipping && !m.gate_open);
}

#[test]
fn non_finite_input_and_settings_stay_finite() {
    let mut chain = Chain::new(48_000);
    let s = Settings {
        input_gain: f32::NAN,
        output_gain: f32::INFINITY,
        ns_eco_strength: f32::NEG_INFINITY,
        gate_threshold_db: f32::NAN,
        eq_low_gain_db: 1e30,
        compressor_ratio: f32::NAN,
        compressor_attack_ms: 0.0,
        limiter_threshold_db: f32::NAN,
        exciter_frequency: f32::INFINITY,
        exciter_amount: f32::NAN,
        deesser_enabled: true,
        exciter_enabled: true,
        ..Settings::default()
    };
    chain.apply(&s);
    let mut input = sine(480, 300.0, 0.5, 48_000.0);
    input[3] = f32::NAN;
    input[7] = f32::INFINITY;
    input[9] = -1e30;
    let mut out = vec![0.0f32; 480];
    for _ in 0..50 {
        chain.process(&input, &mut out);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0), "non-finite or over-range output");
    }
    assert!(chain.metrics().output_rms_db.is_finite());
}

#[test]
fn any_block_length_and_sample_rate_is_accepted() {
    let mut rng = Rng(99);
    for sr in [44_100u32, 48_000, 96_000] {
        let mut chain = Chain::new(sr);
        chain.apply(&Settings { deesser_enabled: true, exciter_enabled: true, ..Settings::default() });
        assert_eq!(chain.latency_samples(), 255);
        let input: Vec<f32> = white_noise(&mut rng, sr as usize / 2, 0.1);
        for block in [1usize, 3, 127, 441, 480, 960, 5000] {
            let mut out = vec![0.0f32; block];
            for chunk in input.chunks(block) {
                let o = &mut out[..chunk.len()];
                chain.process(chunk, o);
                assert!(o.iter().all(|v| v.is_finite()));
            }
        }
    }
}

#[test]
fn chain_latency_is_exact_and_under_six_ms_at_48k() {
    let mut chain = Chain::new(48_000);
    chain.apply(&Settings {
        gate_enabled: false,
        eq_enabled: false,
        compressor_enabled: false,
        limiter_threshold_db: 0.0,
        ns_eco_strength: 0.0,
        ..Settings::default()
    });
    let lat = chain.latency_samples() as usize;
    assert!(lat as f64 / 48.0 <= 6.0, "latency {lat} samples exceeds 6 ms at 48 kHz");

    let n = 4000;
    let impulse_at = 1000;
    let mut x = vec![0.0f32; n];
    x[impulse_at] = 0.5;
    let mut y = vec![0.0f32; n];
    for (xi, yi) in x.chunks(480).zip(y.chunks_mut(480)) {
        chain.process(xi, yi);
    }
    let peak = (0..n).max_by(|&a, &b| y[a].abs().total_cmp(&y[b].abs())).unwrap();
    assert_eq!(peak, impulse_at + lat, "impulse peak at {peak}, expected {}", impulse_at + lat);
    assert!((y[peak] - 0.5).abs() < 1e-4, "impulse amplitude {} != 0.5", y[peak]);
}

#[test]
fn chain_output_is_block_size_consistent() {
    let n = 96_000;
    let mut rng = Rng(4242);
    let mut x = sine(n, 220.0, 0.2, 48_000.0);
    add(&mut x, &white_noise(&mut rng, n, 0.01));
    let settings = Settings { deesser_enabled: true, exciter_enabled: true, ..Settings::default() };

    let run = |block: usize| {
        let mut chain = Chain::new(48_000);
        chain.apply(&settings);
        let mut y = vec![0.0f32; n];
        for (xi, yi) in x.chunks(block).zip(y.chunks_mut(block)) {
            chain.process(xi, yi);
        }
        y
    };
    let reference = run(480);
    let warm = 24_000;
    for block in [64usize, 128, 441] {
        let y = run(block);
        let err: Vec<f32> = reference[warm..].iter().zip(&y[warm..]).map(|(a, b)| a - b).collect();
        let err_db = 20.0 * (rms(&err) / rms(&reference[warm..])).log10();
        eprintln!("block {block} vs 480: error RMS {err_db:.1} dB relative to output");
        assert!(err_db < -30.0, "block {block}: output differs from block 480 by {err_db:.1} dB");
    }
}

#[test]
fn metrics_report_spectrum_levels_and_clipping() {
    // 1 kHz at amplitude 0.5 sits in bin 10 of the 480-sample (100 Hz) display frame.
    let mut chain = Chain::new(48_000);
    chain.apply(&Settings { gate_enabled: false, compressor_enabled: false, ..Settings::default() });
    let x = sine(48_000, 1000.0, 0.5, 48_000.0);
    let mut y = vec![0.0f32; 480];
    for block in x.chunks(480) {
        chain.process(block, &mut y);
    }
    let m = chain.metrics();
    let expected_in = 20.0 * (0.5f64 / 2f64.sqrt()).log10();
    assert!((f64::from(m.input_rms_db) - expected_in).abs() < 0.1, "input rms {}", m.input_rms_db);
    let peak_bin = (1..64).max_by(|&a, &b| m.fft[a].total_cmp(&m.fft[b])).unwrap();
    assert_eq!(peak_bin, 10);
    assert!(m.fft.iter().all(|v| (0.0..=1.0).contains(v)));
    assert!(!m.clipping, "limiter at -1 dB should keep a 0.5 sine from clipping");

    // Output gain pushes the limited signal over the clip level.
    chain.apply(&Settings { output_gain: 4.0, limiter_threshold_db: 0.0, ..Settings::default() });
    for block in x.chunks(480).take(20) {
        chain.process(block, &mut y);
    }
    assert!(chain.metrics().clipping);
}

#[test]
fn gate_metric_follows_threshold() {
    let mut chain = Chain::new(48_000);
    chain.apply(&Settings::default());
    let mut rng = Rng(3);
    let quiet = white_noise(&mut rng, 480, 0.0001);
    let mut y = vec![0.0f32; 480];
    for _ in 0..10 {
        chain.process(&quiet, &mut y);
    }
    assert!(!chain.metrics().gate_open);

    let loud = sine(480, 300.0, 0.3, 48_000.0);
    chain.process(&loud, &mut y);
    assert!(chain.metrics().gate_open);
}
