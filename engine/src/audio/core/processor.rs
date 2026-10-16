//! Capture-side processing shared by every backend: input gain, the DSP chain, and the pushes into
//! the render and monitor rings. Nothing here allocates, locks (beyond `try_lock`) or logs.

use std::sync::atomic::Ordering::{Acquire, Relaxed};
use std::sync::Arc;
use std::time::Instant;

use rtrb::Producer;

use super::state::{Controller, Shared};
use crate::dsp::Chain;

pub struct Processor {
    chain: Chain,
    mono_in: Vec<f32>,
    mono_out: Vec<f32>,
    gain: f32,
    applied_generation: u64,
    main_out: Producer<f32>,
    monitor_out: Producer<f32>,
    shared: Arc<Shared>,
    controller: Arc<Controller>,
}

impl Processor {
    /// `max_block` is the largest chunk processed at once; longer packets are split, so it only sizes
    /// the scratch buffers (allocated here, never on the audio thread).
    pub fn new(
        sample_rate: u32,
        max_block: usize,
        main_out: Producer<f32>,
        monitor_out: Producer<f32>,
        shared: Arc<Shared>,
        controller: Arc<Controller>,
    ) -> Self {
        let scratch = max_block.max(1);
        Self {
            chain: Chain::new(sample_rate),
            mono_in: vec![0.0; scratch],
            mono_out: vec![0.0; scratch],
            gain: 1.0,
            applied_generation: 0,
            main_out,
            monitor_out,
            shared,
            controller,
        }
    }

    /// Applies new settings when the controller's generation moved. Cheap when nothing changed.
    fn refresh_settings(&mut self) {
        let generation = self.controller.generation.load(Acquire);
        if generation == self.applied_generation {
            return;
        }
        let snapshot = self.controller.settings.load();
        self.chain.apply(&snapshot);
        self.gain = snapshot.input_gain;
        self.applied_generation = generation;
        self.shared.chain_latency_samples.store(self.chain.latency_samples(), Relaxed);
    }

    /// Processes one packet of `frames` mono frames. `fill(offset, dst)` writes frames
    /// `offset..offset + dst.len()` of the packet into `dst` (silence included).
    pub fn packet(&mut self, frames: usize, mut fill: impl FnMut(usize, &mut [f32])) {
        let started = Instant::now();
        self.refresh_settings();
        self.shared.counts.capture_packets.fetch_add(1, Relaxed);
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(self.mono_in.len());
            fill(done, &mut self.mono_in[..n]);
            for sample in &mut self.mono_in[..n] {
                *sample *= self.gain;
            }
            self.chain.process(&self.mono_in[..n], &mut self.mono_out[..n]);

            if push_all(&mut self.main_out, &self.mono_out[..n]) > 0 {
                self.shared.counts.capture_drops.fetch_add(1, Relaxed);
            }
            if self.shared.monitor_active.load(Relaxed) {
                push_all(&mut self.monitor_out, &self.mono_out[..n]);
            }
            done += n;
        }
        self.shared.proc_ns.fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
        self.shared.proc_frames.fetch_add(frames as u64, Relaxed);
        if let Ok(mut mailbox) = self.shared.mailbox.try_lock() {
            mailbox.clone_from(self.chain.metrics());
        }
    }

    /// Counts a packet the OS flagged as discontinuous (input was lost before it).
    pub fn discontinuity(&self) {
        self.shared.counts.discontinuities.fetch_add(1, Relaxed);
    }
}

/// Pushes what fits; returns how many samples did not fit (the newest ones are dropped on overflow).
fn push_all(producer: &mut Producer<f32>, samples: &[f32]) -> usize {
    let free = producer.slots();
    let n = samples.len().min(free);
    if n > 0 {
        if let Ok(mut chunk) = producer.write_chunk(n) {
            let (first, second) = chunk.as_mut_slices();
            first.copy_from_slice(&samples[..first.len()]);
            second.copy_from_slice(&samples[first.len()..n]);
            chunk.commit_all();
        }
    }
    samples.len() - n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use rtrb::RingBuffer;

    fn processor(capacity: usize, max_block: usize) -> (Processor, rtrb::Consumer<f32>, Arc<Shared>) {
        let (main_p, main_c) = RingBuffer::new(capacity);
        let (mon_p, _mon_c) = RingBuffer::new(capacity);
        let shared = Arc::new(Shared::new(false));
        let controller = Arc::new(Controller::default());
        // Dynamics stages adapt per block (the chain is block-size consistent only to about -30 dB, see
        // tests/behavior.rs), so they are bypassed: this tests the chunking, not the chain.
        controller.set_settings(Settings {
            input_gain: 0.5,
            ns_mode: "off".into(),
            gate_enabled: false,
            eq_enabled: false,
            compressor_enabled: false,
            deesser_enabled: false,
            exciter_enabled: false,
            ..Settings::default()
        });
        (Processor::new(48_000, max_block, main_p, mon_p, shared.clone(), controller), main_c, shared)
    }

    fn ramp(offset: usize, dst: &mut [f32]) {
        for (i, s) in dst.iter_mut().enumerate() {
            *s = (((offset + i) as f32) * 0.05).sin() * 0.3;
        }
    }

    #[test]
    fn chunked_packets_match_one_block() {
        let (mut small, mut small_out, shared) = processor(4096, 64);
        let (mut big, mut big_out, _) = processor(4096, 4096);
        for _ in 0..4 {
            small.packet(500, ramp);
            big.packet(500, ramp);
        }
        assert_eq!(shared.proc_frames.load(Relaxed), 2000);
        assert_eq!(small_out.slots(), 2000);
        while let (Ok(a), Ok(b)) = (small_out.pop(), big_out.pop()) {
            assert!((a - b).abs() < 1e-6, "{a} != {b}");
        }
    }

    #[test]
    fn full_ring_drops_the_newest_and_counts_it() {
        let (mut p, out, shared) = processor(100, 4096);
        p.packet(150, |_, dst| dst.fill(0.1));
        assert_eq!(out.slots(), 100);
        assert_eq!(shared.counts.capture_drops.load(Relaxed), 1);
    }
}
