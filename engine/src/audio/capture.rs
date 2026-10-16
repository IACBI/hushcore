//! Capture thread: reads the input endpoint, applies input gain and the DSP chain, and feeds the render
//! ring (and the monitor ring while monitoring is on). Nothing here allocates, locks or logs per packet.

use std::sync::atomic::Ordering::{Acquire, Relaxed};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::Result;
use rtrb::Producer;
use windows::Win32::Media::Audio::{IAudioCaptureClient, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT};

use super::clients::{InitKind, Opened};
use super::com::{buffer_slice, ComInit, MmcssGuard, MtaPayload, Wait};
use super::format::{read_mono, StreamFormat};
use super::session::{Controller, Shared, FAIL_CAPTURE};
use crate::dsp::Chain;

pub struct CaptureJob {
    pub opened: Opened,
    pub service: IAudioCaptureClient,
    pub main_out: Producer<f32>,
    pub monitor_out: Producer<f32>,
    pub shared: Arc<Shared>,
    pub controller: Arc<Controller>,
    pub ready: Sender<Result<(), String>>,
}

pub fn spawn(job: CaptureJob) -> Result<JoinHandle<()>> {
    let payload = MtaPayload(job);
    Ok(thread::Builder::new().name("hushcore-capture".into()).spawn(move || {
        let payload = payload;
        run(payload.0)
    })?)
}

fn run(job: CaptureJob) {
    let _com = match ComInit::new() {
        Ok(c) => c,
        Err(e) => {
            let _ = job.ready.send(Err(format!("{e:#}")));
            return;
        }
    };
    let _mmcss = MmcssGuard::enter_pro_audio();

    let scratch = (job.opened.buffer_frames as usize).max(1);
    let mut processor = Processor {
        packets: 0,
        format: job.opened.format,
        chain: Chain::new(job.opened.format.sample_rate),
        mono_in: vec![0.0; scratch],
        mono_out: vec![0.0; scratch],
        gain: 1.0,
        applied_generation: 0,
        main_out: job.main_out,
        monitor_out: job.monitor_out,
        shared: job.shared.clone(),
        controller: job.controller.clone(),
    };

    // SAFETY: the client was initialised on this session's control thread and is started exactly once here.
    if let Err(e) = unsafe { job.opened.client.Start() } {
        let _ = job.ready.send(Err(format!("capture start failed: {e}")));
        return;
    }
    let _ = job.ready.send(Ok(()));

    // Exclusive streams report a phantom packet right after each release (the device position shows
    // only one new packet per event), so they are read one packet per event.
    let single_packet = job.opened.init == InitKind::Exclusive;
    let mut failed = false;
    while !job.shared.stop.load(Relaxed) {
        match job.opened.event.wait(100) {
            Wait::Signaled => {}
            Wait::TimedOut => continue,
            Wait::Failed => {
                failed = true;
                break;
            }
        }
        if drain(&job.service, &mut processor, single_packet).is_err() {
            failed = true;
            break;
        }
    }
    if failed {
        job.shared.failed.store(FAIL_CAPTURE, Relaxed);
    }
    // SAFETY: stopping a started client is always valid; errors on the way out have no recovery.
    let _ = unsafe { job.opened.client.Stop() };
}

/// Pulls every queued packet. `Err` means the endpoint is gone or the client is unusable.
fn drain(service: &IAudioCaptureClient, processor: &mut Processor, single_packet: bool) -> std::result::Result<(), ()> {
    loop {
        // SAFETY: plain query on the started capture client.
        let pending = unsafe { service.GetNextPacketSize() }.map_err(|_| ())?;
        if pending == 0 {
            return Ok(());
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        // SAFETY: out pointers reference live locals; the buffer is released below on every path.
        unsafe { service.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }.map_err(|_| ())?;

        // The first packet after Start always carries the discontinuity flag; that is not an xrun.
        if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && processor.packets > 0 {
            processor.shared.counts.discontinuities.fetch_add(1, Relaxed);
        }
        processor.packets += 1;
        let frame_count = frames as usize;
        processor.shared.counts.capture_packets.fetch_add(1, Relaxed);
        if frame_count > 0 {
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            // SAFETY: WASAPI guarantees `frames` frames at `data` until ReleaseBuffer; silent packets may be null.
            let bytes = if silent {
                None
            } else {
                unsafe { buffer_slice(data, frame_count * processor.format.frame_bytes()) }.map(|b| &*b)
            };
            processor.packet(bytes, frame_count);
        }

        // SAFETY: releases exactly the frames returned by GetBuffer.
        unsafe { service.ReleaseBuffer(frames) }.map_err(|_| ())?;
        if single_packet {
            return Ok(());
        }
    }
}

struct Processor {
    packets: u64,
    format: StreamFormat,
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

    fn packet(&mut self, bytes: Option<&[u8]>, frames: usize) {
        let started = Instant::now();
        self.refresh_settings();
        let frame_bytes = self.format.frame_bytes();
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(self.mono_in.len());
            match bytes {
                Some(src) => read_mono(&src[done * frame_bytes..(done + n) * frame_bytes], &mut self.mono_in[..n], &self.format),
                None => self.mono_in[..n].fill(0.0),
            }
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
}

/// Pushes what fits; returns how many samples did not fit (the newest ones are dropped on overflow).
fn push_all(producer: &mut Producer<f32>, samples: &[f32]) -> usize {
    let mut dropped = 0;
    for &sample in samples {
        if producer.push(sample).is_err() {
            dropped += 1;
        }
    }
    dropped
}
