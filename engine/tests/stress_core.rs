//! Long-run simulation of the capture-to-render ring under clock drift, jitter and period mismatch.
//! It drives `core::make_rings` and `core::RingRenderer` exactly as a backend does, on a simulated
//! clock, so drift policy regressions show up without audio hardware.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use hushcore_engine::audio::core::{make_rings, RingRenderer, Shared};

const RATE: u32 = 48_000;

struct Scenario {
    name: &'static str,
    capture_ms: f32,
    render_ms: f32,
    prefill_periods: f32,
    /// Render clock speed relative to capture (1.001 = render 1000 ppm fast).
    render_speed: f64,
    /// Capture delivers this many packets at once (1 = no jitter).
    burst: u32,
    seconds: f64,
}

#[derive(Debug, Default)]
struct Outcome {
    underruns_after_warmup: u64,
    discards: u64,
    max_fill: usize,
    max_latency_ms: f64,
    requested: u64,
    delivered: u64,
    /// Simulated time of the last underrun, in seconds.
    last_underrun_s: f64,
}

fn run(s: &Scenario) -> Outcome {
    let rings = make_rings(RATE, s.capture_ms, s.render_ms, s.prefill_periods);
    let bounds = rings.bounds;
    let mut producer = rings.main_producer;
    let shared = Arc::new(Shared::new(false));
    let mut renderer = RingRenderer::new(rings.main_consumer, shared.clone(), true, false, bounds);

    let capture_frames = (s.capture_ms as f64 * RATE as f64 / 1000.0).round() as usize;
    let render_frames = (s.render_ms as f64 * RATE as f64 / 1000.0).round() as usize;
    let capture_period = s.capture_ms as f64 / 1000.0;
    let render_period = s.render_ms as f64 / 1000.0 / s.render_speed;
    let packet = vec![0.25f32; capture_frames];

    let (mut next_capture, mut next_render) = (0.0f64, render_period / 2.0);
    let mut captures = 0u64;
    let mut out = Outcome::default();
    let warmup = 2.0;
    let mut underruns_at_warmup = None;

    while next_capture.min(next_render) < s.seconds {
        if next_capture <= next_render {
            captures += 1;
            if captures.is_multiple_of(s.burst as u64) {
                for _ in 0..s.burst {
                    for &v in &packet {
                        let _ = producer.push(v);
                    }
                }
            }
            next_capture += capture_period;
        } else {
            if underruns_at_warmup.is_none() && next_render >= warmup {
                underruns_at_warmup = Some(shared.counts.underruns.load(Relaxed));
            }
            let fill = renderer_fill(&producer, bounds.capacity(2 * render_frames));
            out.max_fill = out.max_fill.max(fill);
            out.max_latency_ms = out.max_latency_ms.max(fill as f64 * 1000.0 / RATE as f64);
            let before = shared.counts.underruns.load(Relaxed);
            let written = renderer.render(render_frames, |_, samples| assert!(samples.iter().all(|&v| v == 0.25 || v == 0.0)));
            if shared.counts.underruns.load(Relaxed) != before {
                out.last_underrun_s = next_render;
            }
            out.requested += render_frames as u64;
            out.delivered += written as u64;
            next_render += render_period;
        }
    }
    out.underruns_after_warmup = shared.counts.underruns.load(Relaxed) - underruns_at_warmup.unwrap_or(0);
    out.discards = shared.counts.render_discards.load(Relaxed);
    out
}

fn renderer_fill(producer: &rtrb::Producer<f32>, capacity: usize) -> usize {
    capacity - producer.slots()
}

fn report(s: &Scenario, o: &Outcome) {
    eprintln!(
        "{:<34} underruns(after 2 s)={:<4} last at {:>6.1} s discards={:<3} max_fill={:<5} max_latency={:.1} ms delivered={:.4}",
        s.name,
        o.underruns_after_warmup,
        o.last_underrun_s,
        o.discards,
        o.max_fill,
        o.max_latency_ms,
        o.delivered as f64 / o.requested as f64
    );
}

fn base(name: &'static str) -> Scenario {
    Scenario { name, capture_ms: 10.0, render_ms: 10.0, prefill_periods: 0.5, render_speed: 1.0, burst: 1, seconds: 600.0 }
}

#[test]
fn drift_within_crystal_tolerance_converges_then_stays_glitch_free() {
    // Two independent crystals are typically within ±100 ppm; allow ±300 ppm. A fast render clock
    // costs at most one underrun per cushion step (2) while the cushion grows to one packet; after
    // that the rest of the ten minutes must pass without a glitch and without latency growth.
    for (name, speed, max_underruns) in
        [("render 300 ppm fast", 1.0003, 2), ("render 100 ppm fast", 1.0001, 2), ("render 300 ppm slow", 0.9997, 0), ("same clock", 1.0, 0)]
    {
        let s = Scenario { render_speed: speed, ..base(name) };
        let o = run(&s);
        report(&s, &o);
        assert!(o.underruns_after_warmup <= max_underruns, "{name}: {o:?}");
        if o.underruns_after_warmup > 0 {
            assert!(o.last_underrun_s < 300.0, "{name}: still glitching at {:.0} s", o.last_underrun_s);
        }
        assert_eq!(o.discards, 0, "{name}: {o:?}");
        assert!(o.max_latency_ms < 35.0, "{name}: latency grew to {:.1} ms", o.max_latency_ms);
    }
}

#[test]
fn large_drift_stays_bounded() {
    // A misbehaving device (1% off) cannot be fully corrected by one-sample slew, but latency
    // must stay bounded and the ring must keep delivering audio.
    for (name, speed) in [("render 1% fast", 1.01), ("render 1% slow", 0.99)] {
        let s = Scenario { render_speed: speed, seconds: 60.0, ..base(name) };
        let o = run(&s);
        report(&s, &o);
        assert!(o.max_latency_ms < 60.0, "{name}: latency grew to {:.1} ms", o.max_latency_ms);
        assert!(o.delivered as f64 / o.requested as f64 > 0.97, "{name}: {o:?}");
    }
}

#[test]
fn bursty_capture_and_mismatched_periods_stay_glitch_free() {
    let cases = [
        Scenario { burst: 2, ..base("capture bursts of 2 packets") },
        Scenario { render_ms: 3.0, ..base("render 3 ms, capture 10 ms") },
        Scenario { capture_ms: 3.0, render_ms: 10.0, ..base("capture 3 ms, render 10 ms") },
        Scenario { capture_ms: 2.67, render_ms: 2.67, ..base("2.67 ms both (128 frames)") },
    ];
    for s in &cases {
        let o = run(s);
        report(s, &o);
        assert_eq!(o.underruns_after_warmup, 0, "{}: {o:?}", s.name);
        assert!(o.max_latency_ms < 45.0, "{}: latency {:.1} ms", s.name, o.max_latency_ms);
    }
}
