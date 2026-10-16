//! Render thread: pops mono frames from the ring in its event callback and writes them to the endpoint.
//! Used for the main output and, with `is_main == false`, for the monitor stream.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::Result;
use rtrb::Consumer;
use windows::Win32::Media::Audio::{IAudioClient, IAudioRenderClient};

use super::clients::{InitKind, Opened};
use super::com::{buffer_slice, ComInit, MmcssGuard, MtaPayload, Wait};
use super::format::{write_mono, StreamFormat};
use super::clients::SILENT_FLAG;
use super::ring::{plan_render, smooth};
use super::session::{Shared, FAIL_MONITOR, FAIL_RENDER};

pub struct RenderJob {
    pub opened: Opened,
    pub service: IAudioRenderClient,
    pub consumer: Consumer<f32>,
    pub shared: Arc<Shared>,
    pub stop: Arc<AtomicBool>,
    pub is_main: bool,
    pub muted: bool,
    pub target_samples: usize,
    pub max_samples: usize,
    pub ready: Sender<Result<(), String>>,
}

/// The thread hands the ring consumer back when it exits, so the monitor can be restarted later.
pub fn spawn(job: RenderJob) -> Result<JoinHandle<Consumer<f32>>> {
    let name = if job.is_main { "hushcore-render" } else { "hushcore-monitor" };
    let payload = MtaPayload(job);
    Ok(thread::Builder::new().name(name.into()).spawn(move || {
        let payload = payload;
        run(payload.0)
    })?)
}

fn run(job: RenderJob) -> Consumer<f32> {
    let RenderJob { opened, service, mut consumer, shared, stop, is_main, muted, target_samples, max_samples, ready } = job;
    let _com = match ComInit::new() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return consumer;
        }
    };
    let _mmcss = MmcssGuard::enter_pro_audio();

    if !is_main {
        // Audio queued before the monitor was enabled is stale; drop it before playing anything.
        let stale = consumer.slots();
        if stale > 0 {
            if let Ok(chunk) = consumer.read_chunk(stale) {
                chunk.commit_all();
            }
        }
    }

    let format = opened.format;
    let buffer_frames = opened.buffer_frames;
    let mut renderer = Renderer {
        client: opened.client,
        service,
        consumer,
        shared: shared.clone(),
        is_main,
        muted: muted && is_main,
        format,
        buffer_frames,
        target: target_samples,
        max: max_samples,
        fill_frames: 0.0,
        exclusive: opened.init == InitKind::Exclusive,
    };

    // Event-driven render streams must hold data before Start; one full buffer of silence does that.
    if let Err(e) = renderer.prefill_silence() {
        let _ = ready.send(Err(format!("render prefill failed: {e}")));
        return renderer.into_consumer();
    }
    // SAFETY: the client is initialised on the control thread and started exactly once here.
    if let Err(e) = unsafe { renderer.client.Start() } {
        let _ = ready.send(Err(format!("render start failed: {e}")));
        return renderer.into_consumer();
    }
    let _ = ready.send(Ok(()));

    let mut failed = false;
    while !stop.load(Relaxed) {
        match opened.event.wait(100) {
            Wait::Signaled => {}
            Wait::TimedOut => continue,
            Wait::Failed => {
                failed = true;
                break;
            }
        }
        if stop.load(Relaxed) {
            break;
        }
        if renderer.tick().is_err() {
            failed = true;
            break;
        }
    }
    if failed {
        if is_main {
            shared.failed.store(FAIL_RENDER, Relaxed);
        } else {
            shared.monitor_active.store(false, Relaxed);
            shared.failed.store(FAIL_MONITOR, Relaxed);
        }
    }
    // SAFETY: stopping a started client is always valid; errors on the way out have no recovery.
    let _ = unsafe { renderer.client.Stop() };
    renderer.into_consumer()
}

struct Renderer {
    client: IAudioClient,
    service: IAudioRenderClient,
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    is_main: bool,
    muted: bool,
    format: StreamFormat,
    buffer_frames: u32,
    target: usize,
    max: usize,
    fill_frames: f32,
    exclusive: bool,
}

impl Renderer {
    fn into_consumer(self) -> Consumer<f32> {
        self.consumer
    }

    fn prefill_silence(&mut self) -> std::result::Result<(), String> {
        let frames = self.buffer_frames;
        // SAFETY: GetBuffer for the full buffer size is valid before Start; the slice is used only until release.
        let ptr = unsafe { self.service.GetBuffer(frames) }.map_err(|e| e.to_string())?;
        let bytes = frames as usize * self.format.frame_bytes();
        // SAFETY: GetBuffer returned `frames` frames, i.e. `bytes` bytes, valid until ReleaseBuffer.
        match unsafe { buffer_slice(ptr, bytes) } {
            Some(dst) => dst.fill(0),
            None => return Err("null render buffer".into()),
        }
        // SAFETY: releases the exact count obtained from GetBuffer.
        unsafe { self.service.ReleaseBuffer(frames, SILENT_FLAG) }.map_err(|e| e.to_string())
    }

    /// One event callback: decide, fill the device buffer, release it. `Err` means the device is gone.
    fn tick(&mut self) -> std::result::Result<(), ()> {
        // Exclusive streams are event-driven on whole buffers: each event frees one full period, and
        // GetCurrentPadding reports the occupied buffer, not free space, so it is only used in shared mode.
        let padding = if self.exclusive {
            0
        } else {
            // SAFETY: plain query on the started render client.
            unsafe { self.client.GetCurrentPadding() }.map_err(|_| ())?
        };
        let wanted = self.buffer_frames.saturating_sub(padding) as usize;
        if wanted == 0 {
            return Ok(());
        }

        let queued = self.consumer.slots();
        let plan = plan_render(queued, wanted, self.target, self.max);
        if self.is_main {
            self.shared.counts.render_callbacks.fetch_add(1, Relaxed);
        }
        if plan.discard > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(plan.discard) {
                chunk.commit_all();
            }
            if self.is_main {
                self.shared.counts.render_discards.fetch_add(1, Relaxed);
                self.shared.counts.discarded_frames.fetch_add(plan.discard as u64, Relaxed);
            }
        }
        if self.is_main {
            if plan.underrun() {
                self.shared.counts.underruns.fetch_add(1, Relaxed);
            }
            self.shared.counts.rendered_frames.fetch_add(plan.take as u64, Relaxed);
            self.shared.counts.silence_frames.fetch_add(plan.silence as u64, Relaxed);
        }

        let frame_bytes = self.format.frame_bytes();
        // SAFETY: the padding query says `wanted` frames are free; the slice ends at ReleaseBuffer below.
        let ptr = unsafe { self.service.GetBuffer(wanted as u32) }.map_err(|_| ())?;
        let dst = unsafe { buffer_slice(ptr, wanted * frame_bytes) }.ok_or(())?;

        // Muting only replaces the audio with silence; the ring is always consumed so latency stays bounded.
        let mut written = 0;
        if plan.take > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(plan.take) {
                if !self.muted {
                    let (first, second) = chunk.as_slices();
                    let split = first.len() * frame_bytes;
                    write_mono(&mut dst[..split], first, &self.format);
                    write_mono(&mut dst[split..plan.take * frame_bytes], second, &self.format);
                    written = plan.take;
                }
                chunk.commit_all();
            }
        }
        dst[written * frame_bytes..].fill(0);

        let flags = if written == 0 { SILENT_FLAG } else { 0 };
        // SAFETY: releases exactly the frames obtained from GetBuffer above.
        unsafe { self.service.ReleaseBuffer(wanted as u32, flags) }.map_err(|_| ())?;

        if self.is_main {
            let fill = (queued - plan.discard) as f32;
            self.fill_frames = smooth(self.fill_frames, fill);
            self.shared.render_fill_bits.store(self.fill_frames.to_bits(), Relaxed);
        }
        Ok(())
    }
}
