//! Render-side ring draining shared by every backend: executes `ring::plan_render` (overflow discard,
//! drift slew, underrun padding) and keeps the counters. Used for the main output and the monitor.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use rtrb::Consumer;

use super::ring::{plan_render, smooth, Control, Cushion, RingBounds};
use super::state::Shared;

pub struct RingRenderer {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    is_main: bool,
    muted: bool,
    bounds: RingBounds,
    cushion: Cushion,
    smoothed_margin: Option<f32>,
    fill_frames: f32,
}

impl RingRenderer {
    /// `muted` applies to the main output only (selftest); the monitor is never muted.
    pub fn new(consumer: Consumer<f32>, shared: Arc<Shared>, is_main: bool, muted: bool, bounds: RingBounds) -> Self {
        Self { consumer, shared, is_main, muted: muted && is_main, bounds, cushion: Cushion::new(), smoothed_margin: None, fill_frames: 0.0 }
    }

    /// Drops everything queued. The monitor calls this before it starts: older audio is stale.
    pub fn drop_stale(&mut self) {
        let stale = self.consumer.slots();
        if stale > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(stale) {
                chunk.commit_all();
            }
        }
    }

    pub fn into_consumer(self) -> Consumer<f32> {
        self.consumer
    }

    /// Produces `wanted` frames. Audio goes out through `write(offset, samples)` in order, starting at
    /// offset 0; the return value is how many frames were written. The caller fills the rest of the
    /// device buffer (frames `written..wanted`) with silence.
    pub fn render(&mut self, wanted: usize, mut write: impl FnMut(usize, &[f32])) -> usize {
        if wanted == 0 {
            return 0;
        }
        let queued = self.consumer.slots();
        let margin = queued as f32 - wanted as f32;
        let smoothed = match self.smoothed_margin {
            Some(previous) => smooth(previous, margin),
            None => margin,
        };
        self.smoothed_margin = Some(smoothed);
        let control = Control { cushion: self.cushion.frames, smoothed_margin: smoothed };
        let plan = plan_render(queued, wanted, self.bounds, control);
        self.cushion.observe(&plan, self.bounds);
        let counts = &self.shared.counts;
        if self.is_main {
            counts.render_callbacks.fetch_add(1, Relaxed);
        }
        if plan.discard > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(plan.discard) {
                chunk.commit_all();
            }
            if self.is_main {
                counts.render_discards.fetch_add(1, Relaxed);
                counts.discarded_frames.fetch_add(plan.discard as u64, Relaxed);
            }
        }
        if plan.slew_drop > 0 {
            let _ = self.consumer.pop();
            if self.is_main {
                counts.slew_drops.fetch_add(1, Relaxed);
            }
        }
        if self.is_main {
            if plan.repeat > 0 {
                counts.slew_repeats.fetch_add(1, Relaxed);
            }
            if plan.underrun() {
                counts.underruns.fetch_add(1, Relaxed);
            }
            counts.rendered_frames.fetch_add(plan.take as u64, Relaxed);
            counts.silence_frames.fetch_add(plan.silence as u64, Relaxed);
        }

        // Muting only replaces the audio with silence; the ring is always consumed so latency stays bounded.
        let mut written = 0;
        if plan.take > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(plan.take) {
                if !self.muted {
                    let (first, second) = chunk.as_slices();
                    write(0, first);
                    if !second.is_empty() {
                        write(first.len(), second);
                    }
                    written = plan.take;
                    if plan.repeat > 0 {
                        let last = second.last().or(first.last()).copied().unwrap_or(0.0);
                        write(written, &[last]);
                        written += 1;
                    }
                }
                chunk.commit_all();
            }
        }

        if self.is_main {
            let fill = (queued - plan.discard) as f32;
            self.fill_frames = smooth(self.fill_frames, fill);
            self.shared.render_fill_bits.store(self.fill_frames.to_bits(), Relaxed);
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    fn setup(queued: usize) -> (RingRenderer, Arc<Shared>) {
        let (mut p, c) = RingBuffer::new(8192);
        for i in 0..queued {
            p.push(i as f32).unwrap();
        }
        let shared = Arc::new(Shared::new(false));
        let bounds = RingBounds::new(240, 480);
        (RingRenderer::new(c, shared.clone(), true, false, bounds), shared)
    }

    fn collect(r: &mut RingRenderer, wanted: usize) -> (Vec<f32>, usize) {
        let mut out = vec![f32::NAN; wanted];
        let written = r.render(wanted, |offset, samples| out[offset..offset + samples.len()].copy_from_slice(samples));
        (out, written)
    }

    #[test]
    fn steady_state_plays_in_order() {
        let (mut r, shared) = setup(480);
        let (out, written) = collect(&mut r, 480);
        assert_eq!(written, 480);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[479], 479.0);
        assert_eq!(shared.counts.underruns.load(Relaxed), 0);
    }

    #[test]
    fn short_ring_holds_one_sample_then_reports_an_underrun() {
        let (mut r, shared) = setup(200);
        let (out, written) = collect(&mut r, 480);
        // 200 played plus one held repeat; the caller pads the rest.
        assert_eq!(written, 201);
        assert_eq!(out[200], 199.0);
        assert_eq!(shared.counts.underruns.load(Relaxed), 1);
        assert_eq!(shared.counts.slew_repeats.load(Relaxed), 1);
    }

    #[test]
    fn overflow_discards_the_oldest() {
        let (mut r, shared) = setup(3000);
        let (out, written) = collect(&mut r, 480);
        assert_eq!(written, 480);
        // Dropped down to wanted + target = 720 queued.
        assert_eq!(out[0], 2280.0);
        assert_eq!(shared.counts.render_discards.load(Relaxed), 1);
    }

    #[test]
    fn muted_consumes_but_writes_nothing() {
        let (p, c) = RingBuffer::new(1024);
        drop(p);
        let shared = Arc::new(Shared::new(true));
        let mut r = RingRenderer::new(c, shared, true, true, RingBounds::new(1, 160));
        assert_eq!(r.render(160, |_, _| panic!("muted output must not write")), 0);
    }
}
