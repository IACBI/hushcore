//! Render-side ring accounting. The decision is pure so the real-time loop only executes it.

/// What one render callback does with the queued samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPlan {
    /// Oldest samples to drop before reading (overflow policy).
    pub discard: usize,
    /// Samples to play from the ring.
    pub take: usize,
    /// Frames to fill with silence because the ring ran dry.
    pub silence: usize,
}

impl RenderPlan {
    pub fn underrun(&self) -> bool {
        self.silence > 0
    }
}

/// `available` queued samples, `wanted` frames the device needs now. When more than `max` samples are
/// queued, the oldest are dropped so the queue returns to `target` (keeps latency bounded).
pub fn plan_render(available: usize, wanted: usize, target: usize, max: usize) -> RenderPlan {
    let (discard, queued) = if available > max {
        let discard = available - target.min(available);
        (discard, available - discard)
    } else {
        (0, available)
    };
    let take = queued.min(wanted);
    RenderPlan { discard, take, silence: wanted - take }
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

    #[test]
    fn steady_state_plays_without_underrun_or_discard() {
        let p = plan_render(480, 480, 480, 1920);
        assert_eq!(p, RenderPlan { discard: 0, take: 480, silence: 0 });
        assert!(!p.underrun());
    }

    #[test]
    fn short_ring_pads_the_rest_with_silence() {
        let p = plan_render(100, 480, 480, 1920);
        assert_eq!(p, RenderPlan { discard: 0, take: 100, silence: 380 });
        assert!(p.underrun());
    }

    #[test]
    fn empty_ring_is_all_silence() {
        let p = plan_render(0, 480, 480, 1920);
        assert_eq!(p.take, 0);
        assert_eq!(p.silence, 480);
    }

    #[test]
    fn overflow_drops_oldest_down_to_target() {
        let p = plan_render(3000, 480, 480, 1920);
        assert_eq!(p.discard, 2520);
        assert_eq!(p.take, 480);
        assert_eq!(p.silence, 0);
        assert_eq!(3000 - p.discard, 480);
    }

    #[test]
    fn accounting_is_conserved() {
        for available in [0usize, 1, 479, 480, 1919, 1920, 1921, 7000] {
            for wanted in [1usize, 240, 480, 960] {
                let p = plan_render(available, wanted, 480, 1920);
                assert_eq!(p.take + p.silence, wanted);
                assert!(p.discard + p.take <= available);
            }
        }
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
