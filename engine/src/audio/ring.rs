//! Render-side ring accounting. The decisions are pure so the real-time loop only executes them.

/// Fill thresholds in mono samples (frames) for a capture-to-render ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingBounds {
    /// Fill the ring is pre-filled to and returned to after an overflow.
    pub target: usize,
    /// Above this, the oldest samples are dropped (overflow policy).
    pub max: usize,
    /// Above this, one extra sample is skipped per callback so a fast capture clock cannot run away.
    pub high: usize,
    /// Below this, the last sample is held one extra frame per callback so a slow capture clock cannot starve.
    pub low: usize,
}

impl RingBounds {
    /// `prefill_frames` is the target fill; `capture_frames` is the packet the capture side delivers. The
    /// steady fill sits about one capture packet above the target, so the thresholds leave that much margin.
    pub fn new(prefill_frames: usize, capture_frames: usize) -> Self {
        let target = prefill_frames.max(1);
        Self {
            target,
            max: target + 4 * capture_frames,
            high: target + 2 * capture_frames,
            low: target / 2,
        }
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

/// `available` queued samples, `wanted` frames the device needs now.
pub fn plan_render(available: usize, wanted: usize, bounds: RingBounds) -> RenderPlan {
    let (discard, mut queued) = if available > bounds.max {
        let discard = available - bounds.target.min(available);
        (discard, available - discard)
    } else {
        (0, available)
    };

    let mut slew_drop = 0;
    if queued > bounds.high && queued > 0 {
        slew_drop = 1;
        queued -= 1;
    }

    let mut want = wanted;
    let mut repeat = 0;
    if wanted > 1 && queued > 0 && queued < bounds.low {
        repeat = 1;
        want = wanted - 1;
    }

    let take = queued.min(want);
    RenderPlan { discard, slew_drop, take, repeat, silence: want - take }
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

    fn bounds() -> RingBounds {
        RingBounds { target: 480, max: 1920, high: 960, low: 240 }
    }

    fn total_frames(p: &RenderPlan, wanted: usize) -> bool {
        p.take + p.repeat + p.silence == wanted
    }

    #[test]
    fn steady_state_plays_without_underrun_or_correction() {
        let p = plan_render(480, 480, bounds());
        assert_eq!(p, RenderPlan { discard: 0, slew_drop: 0, take: 480, repeat: 0, silence: 0 });
        assert!(!p.underrun());
    }

    #[test]
    fn short_ring_pads_the_rest_with_silence() {
        let p = plan_render(100, 480, RingBounds { low: 0, ..bounds() });
        assert_eq!(p.take, 100);
        assert_eq!(p.silence, 380);
        assert!(p.underrun());
        assert!(total_frames(&p, 480));
    }

    #[test]
    fn empty_ring_is_all_silence() {
        let p = plan_render(0, 480, RingBounds { low: 0, ..bounds() });
        assert_eq!(p.take, 0);
        assert_eq!(p.silence, 480);
    }

    #[test]
    fn overflow_drops_oldest_down_to_target() {
        let p = plan_render(3000, 480, bounds());
        assert_eq!(p.discard, 2520);
        assert_eq!(p.take, 480);
        assert_eq!(p.silence, 0);
    }

    #[test]
    fn high_fill_skips_one_sample_without_overflow() {
        let p = plan_render(1200, 480, bounds());
        assert_eq!(p.discard, 0);
        assert_eq!(p.slew_drop, 1);
        assert_eq!(p.take, 480);
    }

    #[test]
    fn low_fill_holds_the_last_sample_for_one_frame() {
        let p = plan_render(200, 480, bounds());
        assert_eq!(p.repeat, 1);
        assert_eq!(p.take, 200);
        assert_eq!(p.silence, 279);
        assert!(total_frames(&p, 480));
    }

    #[test]
    fn every_callback_fills_exactly_the_wanted_frames() {
        for available in [0usize, 1, 2, 239, 240, 479, 480, 959, 960, 1200, 1919, 1920, 1921, 7000] {
            for wanted in [1usize, 2, 160, 240, 480, 960] {
                let p = plan_render(available, wanted, bounds());
                assert!(total_frames(&p, wanted), "{available} {wanted} {p:?}");
                assert!(p.discard + p.slew_drop + p.take <= available);
                assert!(p.repeat <= 1 && p.slew_drop <= 1);
            }
        }
    }

    #[test]
    fn bounds_scale_with_the_period() {
        let b = RingBounds::new(160, 160);
        assert_eq!(b, RingBounds { target: 160, max: 800, high: 480, low: 80 });
        let tiny = RingBounds::new(0, 160);
        assert_eq!(tiny.target, 1);
        assert!(tiny.low < tiny.target && tiny.target < tiny.high && tiny.high < tiny.max);
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
