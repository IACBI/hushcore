//! Real-time cost of the full mono chain with every stage enabled.
//!
//! Runs 10 s of noise plus a 440 Hz tone at 48 kHz through `Chain::process` in 128- and
//! 480-sample blocks, and prints the time per block and the real-time factor (processing time
//! divided by audio time; 0.01 means 1% of one core).
//!
//! Run: cargo run --release --example bench_chain

use hushcore_engine::{Chain, Settings};
use std::f32::consts::PI;
use std::hint::black_box;
use std::time::Instant;

const SR: u32 = 48_000;
const REPS: usize = 5;

fn main() {
    let seconds = 10;
    let n = seconds * SR as usize;

    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let input: Vec<f32> = (0..n)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let noise = ((seed >> 40) as f32 / (1u32 << 24) as f32 - 0.5) * 0.05;
            let tone = 0.2 * (2.0 * PI * 440.0 * i as f32 / SR as f32).sin();
            noise + tone
        })
        .collect();

    let settings = Settings {
        ns_mode: "eco".into(),
        eq_low_gain_db: 3.0,
        eq_mid_gain_db: -2.0,
        eq_high_gain_db: 2.0,
        deesser_enabled: true,
        exciter_enabled: true,
        ..Settings::default()
    };

    println!("hushcore chain, all stages enabled, {seconds} s at {SR} Hz, best of {REPS} runs");
    for block in [128usize, 480] {
        let block_s = block as f64 / f64::from(SR);
        let blocks = n / block;
        let mut best = f64::INFINITY;
        let mut checksum = 0.0f64;
        for _ in 0..REPS {
            let mut chain = Chain::new(SR);
            chain.apply(&settings);
            let mut out = vec![0.0f32; block];
            // Warm up on the first second so the noise tracker and filters are settled.
            for b in input[..SR as usize].chunks_exact(block) {
                chain.process(b, &mut out);
            }
            let start = Instant::now();
            for b in input.chunks_exact(block) {
                chain.process(b, &mut out);
                checksum += f64::from(out[0]);
            }
            let secs = start.elapsed().as_secs_f64();
            best = best.min(secs);
        }
        black_box(checksum);
        let per_block_us = best * 1e6 / blocks as f64;
        let rtf = best / (n as f64 / f64::from(SR));
        println!(
            "block {block:>4} ({:>5.2} ms): {per_block_us:>7.2} us/block, real-time factor {rtf:.5} ({:.3}% of one core)",
            block_s * 1e3,
            rtf * 100.0
        );
    }
}
