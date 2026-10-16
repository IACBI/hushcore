//! Render-side ring accounting. The decisions are pure so the real-time loop only executes them.
//!
//! Every threshold is a *margin*: how many frames are queued beyond what the render callback wants
//! right now. Measuring against `wanted` (not the raw fill) keeps the policy correct when the
//! capture and render periods differ, because the raw fill at render time naturally includes one
//! render period plus a capture-packet sawtooth.

/// Fixed geometry of a capture-to-render ring, in mono frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingBounds {
    /// Frames the ring is pre-filled with; also the margin restored after an overflow.
    pub target: usize,
    /// Frames the capture side delivers per packet: the size of the sawtooth seen at render time.
    pub capture: usize,
    /// Upper bound for the adaptive cushion (see `Cushion`).
    pub cushion_max: usize,
}

impl RingBounds {
    pub fn new(prefill_frames: usize, capture_frames: usize) -> Self {
        let capture = capture_frames.max(1);
        Self { target: prefill_frames.max(1), capture, cushion_max: capture }
    }

    /// Above this *smoothed* margin one sample is skipped per callback. It bounds latency both for a
    /// fast capture clock and for an unlucky start phase (the steady margin can sit anywhere within one
    /// packet of the target, depending on where the render instant falls between capture packets).
    pub fn high(&self, cushion: usize) -> usize {
        cushion + self.target + self.capture / 2
    }

    /// Above this margin the oldest frames are dropped at once (a stall or a long burst).
    pub fn max(&self, cushion: usize) -> usize {
        cushion + self.target + 4 * self.capture
    }

    /// Ring capacity that holds the largest margin plus a render buffer of `render_frames`.
    pub fn capacity(&self, render_frames: usize) -> usize {
        2 * (self.max(self.cushion_max) + render_frames) + 1024
    }
}

/// What one render callback does with the queued samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPlan {
    /// Oldest samples dropped because the ring overflowed.
    pub discard: usize,
    /// One sample skipped to correct a fast capture clock (0 or 1).
    pub slew_drop: usize,
    /// Samples played from the ring.
    pub take: usize,
    /// One held frame (repeat of the last played sample) to correct a slow capture clock (0 or 1).
    pub repeat: usize,
    /// Frames filled with silence because the ring ran dry.
    pub silence: usize,
}

impl RenderPlan {
    pub fn underrun(&self) -> bool {
        self.silence > 0
    }
}

/// Controller state the plan depends on, kept by `RingRenderer`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Control {
    /// Adaptive minimum margin (0 until underruns show the clocks need one), see `Cushion`.
    pub cushion: usize,
    /// Exponentially smoothed margin (`available - wanted`) over recent callbacks. Smoothing hides the
    /// sawtooth that mismatched capture and render periods put on the instantaneous margin.
    pub smoothed_margin: f32,
}

/// `available` queued samples, `wanted` frames the device needs now.
pub fn plan_render(available: usize, wanted: usize, bounds: RingBounds, control: Control) -> RenderPlan {
    let cushion = control.cushion;
    let keep = wanted + cushion + bounds.target;
    let (discard, mut queued) = if available > wanted + bounds.max(cushion) {
        (available - keep, keep)
    } else {
        (0, available)
    };

    let mut slew_drop = 0;
    // Never trim below the target right now, whatever the average says.
    if control.smoothed_margin > bounds.high(cushion) as f32 && queued > wanted + cushion + bounds.target {
        slew_drop = 1;
        queued -= 1;
    }

    let mut want = wanted;
    let mut repeat = 0;
    // Holding one sample stretches the queue by one frame; it applies only while audio is queued.
    if wanted > 1 && queued > 0 && queued < wanted + cushion {
        repeat = 1;
        want = wanted - 1;
    }

    let take = queued.min(want);
    RenderPlan { discard, slew_drop, take, repeat, silence: want - take }
}

/// Adaptive minimum margin. Two free-running clocks drift; each time the render instant crosses a
/// capture packet the margin seen at render time drops by one packet. Holding a cushion of one
/// packet makes those crossings glitch-free, but costs latency, so the cushion starts at zero and
/// grows half a packet per underrun after warm-up: a drifting pair glitches at most twice per
/// session, and same-clock systems never pay for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cushion {
    pub frames: usize,
    rendered: usize,
}

/// Frames rendered (2 s at 48 kHz) before underruns start to grow the cushion. Counted in audio
/// rather than callbacks so start-up transients are ignored the same way at any period size.
pub const CUSHION_WARMUP_FRAMES: usize = 96_000;

impl Cushion {
    pub fn new() -> Self {
        Self { frames: 0, rendered: 0 }
    }

    pub fn observe(&mut self, plan: &RenderPlan, bounds: RingBounds) {
        if self.rendered < CUSHION_WARMUP_FRAMES {
            self.rendered += plan.take + plan.repeat + plan.silence;
            return;
        }
        if plan.underrun() {
            let step = (bounds.capture / 2).max(1);
            self.frames = (self.frames + step).min(bounds.cushion_max);
        }
    }
}

impl Default for Cushion {
    fn default() -> Self {
        Self::new()
    }
}

/// Exponential smoothing used for the average ring fill (about 8 callbacks time constant).
pub fn smooth(previous: f32, sample: f32) -> f32 {
    previous + (sample - previous) * 0.125
}

pub fn frames_to_ms(frames: f32, sample_rate: u32) -> f32 {
    frames * 1000.0 / sample_rate as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 ms packets at 48 kHz, half a period of pre-fill.
    fn bounds() -> RingBounds {
        RingBounds::new(240, 480)
    }

    /// Control whose smoothed margin equals the instantaneous one (no history).
    fn at(available: usize, wanted: usize, cushion: usize) -> Control {
        Control { cushion, smoothed_margin: available as f32 - wanted as f32 }
    }

    fn plan(available: usize, wanted: usize, b: RingBounds, cushion: usize) -> RenderPlan {
        plan_render(available, wanted, b, at(available, wanted, cushion))
    }

    fn total_frames(p: &RenderPlan, wanted: usize) -> bool {
        p.take + p.repeat + p.silence == wanted
    }

    #[test]
    fn steady_state_plays_without_underrun_or_correction() {
        // One packet arrived on top of the pre-fill: margin = target.
        let p = plan(720, 480, bounds(), 0);
        assert_eq!(p, RenderPlan { discard: 0, slew_drop: 0, take: 480, repeat: 0, silence: 0 });
        assert!(!p.underrun());
    }

    #[test]
    fn short_ring_holds_one_sample_and_pads_the_rest() {
        let p = plan(100, 480, bounds(), 0);
        assert_eq!(p.repeat, 1);
        assert_eq!(p.take, 100);
        assert_eq!(p.silence, 379);
        assert!(p.underrun());
        assert!(total_frames(&p, 480));
    }

    #[test]
    fn empty_ring_is_all_silence() {
        let p = plan(0, 480, bounds(), 0);
        assert_eq!(p.take, 0);
        assert_eq!(p.repeat, 0);
        assert_eq!(p.silence, 480);
    }

    #[test]
    fn overflow_drops_oldest_down_to_the_target_margin() {
        let p = plan(5000, 480, bounds(), 0);
        assert_eq!(p.discard, 5000 - (480 + 240));
        assert_eq!(p.take, 480);
        assert_eq!(p.silence, 0);
    }

    #[test]
    fn high_smoothed_margin_skips_one_sample_without_overflow() {
        // margin 1000 - 480 = 520 > high (240 + 240 = 480) but below max (240 + 1920).
        let p = plan(1000, 480, bounds(), 0);
        assert_eq!(p.discard, 0);
        assert_eq!(p.slew_drop, 1);
        assert_eq!(p.take, 480);
    }

    #[test]
    fn slew_follows_the_average_not_one_callback() {
        let b = bounds();
        // A momentary peak with a normal average is the packet sawtooth: no correction.
        let p = plan_render(1000, 480, b, Control { cushion: 0, smoothed_margin: 300.0 });
        assert_eq!(p.slew_drop, 0);
        // A high average with a low instantaneous margin must not trim below the target.
        let p = plan_render(600, 480, b, Control { cushion: 0, smoothed_margin: 900.0 });
        assert_eq!(p.slew_drop, 0);
    }

    #[test]
    fn margin_below_the_cushion_holds_one_sample() {
        // 500 queued, 480 wanted: margin 20 < cushion 120.
        let p = plan(500, 480, bounds(), 120);
        assert_eq!(p.repeat, 1);
        assert_eq!(p.take, 479);
        assert!(!p.underrun());
        // Without a cushion the same fill needs no correction.
        assert_eq!(plan(500, 480, bounds(), 0).repeat, 0);
    }

    #[test]
    fn small_capture_packets_do_not_trigger_the_slew() {
        // Capture 3 ms (144 frames), render 10 ms: the raw fill at render time is about one render
        // period plus the target plus a packet, which must not count as "too full".
        let b = RingBounds::new(240, 144);
        // The smoothed margin sits mid-sawtooth: target + half a packet.
        let control = Control { cushion: 0, smoothed_margin: 240.0 + 72.0 };
        for available in [480 + 240, 480 + 240 + 143] {
            let p = plan_render(available, 480, b, control);
            assert_eq!((p.slew_drop, p.discard, p.repeat, p.silence), (0, 0, 0, 0), "{available}");
        }
    }

    #[test]
    fn every_callback_fills_exactly_the_wanted_frames() {
        for cushion in [0usize, 120, 480] {
            for available in [0usize, 1, 2, 239, 240, 479, 480, 959, 960, 1200, 1919, 1920, 1921, 7000] {
                for wanted in [1usize, 2, 160, 240, 480, 960] {
                    let p = plan(available, wanted, bounds(), cushion);
                    assert!(total_frames(&p, wanted), "{available} {wanted} {p:?}");
                    assert!(p.discard + p.slew_drop + p.take <= available);
                    assert!(p.repeat <= 1 && p.slew_drop <= 1);
                }
            }
        }
    }

    #[test]
    fn cushion_grows_only_after_warmup_and_is_capped() {
        let b = bounds();
        let underrun = plan(0, 480, b, 0);
        let mut c = Cushion::new();
        for _ in 0..CUSHION_WARMUP_FRAMES / 480 {
            c.observe(&underrun, b);
        }
        assert_eq!(c.frames, 0);
        c.observe(&underrun, b);
        assert_eq!(c.frames, 240);
        for _ in 0..10 {
            c.observe(&underrun, b);
        }
        assert_eq!(c.frames, b.cushion_max);
        let clean = plan(720, 480, b, c.frames);
        c.observe(&clean, b);
        assert_eq!(c.frames, b.cushion_max);
    }

    #[test]
    fn bounds_scale_with_the_packet() {
        let b = RingBounds::new(160, 160);
        assert_eq!(b, RingBounds { target: 160, capture: 160, cushion_max: 160 });
        assert_eq!(b.high(0), 240);
        assert_eq!(b.max(0), 800);
        assert!(b.capacity(480) > 2 * (b.max(b.cushion_max) + 480));
        let tiny = RingBounds::new(0, 0);
        assert_eq!((tiny.target, tiny.capture), (1, 1));
    }

    #[test]
    fn fill_smoothing_converges_and_ms_conversion() {
        let mut v = 0.0;
        for _ in 0..200 {
            v = smooth(v, 480.0);
        }
        assert!((v - 480.0).abs() < 0.5);
        assert!((frames_to_ms(480.0, 48_000) - 10.0).abs() < 1e-4);
    }
}
