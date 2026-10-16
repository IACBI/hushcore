//! Capture thread: reads the input endpoint, applies input gain and the DSP chain, and feeds the render
//! ring (and the monitor ring while monitoring is on). Nothing here allocates, locks or logs per packet.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::Result;
use rtrb::Producer;
use windows::Win32::Media::Audio::{IAudioCaptureClient, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT};

use super::clients::{InitKind, Opened};
use super::com::{buffer_slice, ComInit, MmcssGuard, MtaPayload, Wait};
use super::format::{read_mono, StreamFormat};
use crate::audio::core::{Controller, Processor, Shared, FAIL_CAPTURE};

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

    let format = job.opened.format;
    let mut capture = Capture {
        packets: 0,
        format,
        processor: Processor::new(
            format.sample_rate,
            job.opened.buffer_frames as usize,
            job.main_out,
            job.monitor_out,
            job.shared.clone(),
            job.controller.clone(),
        ),
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
        if drain(&job.service, &mut capture, single_packet).is_err() {
            failed = true;
            break;
        }
    }
    if failed {
        job.shared.fail(FAIL_CAPTURE);
    }
    // SAFETY: stopping a started client is always valid; errors on the way out have no recovery.
    let _ = unsafe { job.opened.client.Stop() };
}

struct Capture {
    packets: u64,
    format: StreamFormat,
    processor: Processor,
}

/// Pulls every queued packet. `Err` means the endpoint is gone or the client is unusable.
fn drain(service: &IAudioCaptureClient, capture: &mut Capture, single_packet: bool) -> std::result::Result<(), ()> {
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
        if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && capture.packets > 0 {
            capture.processor.discontinuity();
        }
        capture.packets += 1;
        let frame_count = frames as usize;
        if frame_count > 0 {
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let format = capture.format;
            let frame_bytes = format.frame_bytes();
            // SAFETY: WASAPI guarantees `frames` frames at `data` until ReleaseBuffer; silent packets may be null.
            let bytes = if silent { None } else { unsafe { buffer_slice(data, frame_count * frame_bytes) }.map(|b| &*b) };
            capture.processor.packet(frame_count, |offset, dst| match bytes {
                Some(src) => read_mono(&src[offset * frame_bytes..(offset + dst.len()) * frame_bytes], dst, &format),
                None => dst.fill(0.0),
            });
        }

        // SAFETY: releases exactly the frames returned by GetBuffer.
        unsafe { service.ReleaseBuffer(frames) }.map_err(|_| ())?;
        if single_packet {
            return Ok(());
        }
    }
}
