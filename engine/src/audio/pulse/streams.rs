//! The capture and render threads. Each owns one `Simple` connection: `read` and `write` block, and
//! the server's clock paces them, so no thread waits on a timer. Nothing here allocates per period.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use rtrb::Consumer;

use crate::audio::core::ring::RingBounds;
use crate::audio::core::{Processor, RingRenderer, Shared, FAIL_CAPTURE, FAIL_MONITOR, FAIL_RENDER};

/// Capture and render period. Chosen by measurement in the Docker harness (docs/ and driver/linux/README).
pub const CAPTURE_PERIOD_MS: f32 = 5.0;
pub const RENDER_PERIOD_MS: f32 = 5.0;
/// Server-side playback buffer target: about two periods, so the server never starves between writes.
pub const PLAYBACK_TARGET_MS: f32 = 10.0;
/// How long the render thread waits for the first captured packet before it starts anyway.
const FIRST_PACKET_DEADLINE: Duration = Duration::from_secs(2);

/// Ready signal per thread: the stream's latency in microseconds, or why it failed.
pub type Ready = Sender<Result<u64, String>>;

pub struct CaptureJob {
    pub simple: Simple,
    pub processor: Processor,
    pub shared: Arc<Shared>,
    pub frames: usize,
    pub ready: Ready,
}

pub struct RenderJob {
    pub simple: Simple,
    pub consumer: Consumer<f32>,
    pub shared: Arc<Shared>,
    pub stop: Arc<AtomicBool>,
    pub is_main: bool,
    pub muted: bool,
    pub bounds: RingBounds,
    pub frames: usize,
    pub ready: Ready,
}

/// Mono float32 native-endian frames per period.
pub fn period_frames(rate: u32, period_ms: f32) -> usize {
    crate::audio::core::ms_to_frames(period_ms, rate).max(1)
}

/// Bytes of one mono float32 frame count, as the server buffer attributes expect.
fn bytes(frames: usize) -> u32 {
    (frames * std::mem::size_of::<f32>()) as u32
}

pub fn float_spec(rate: u32) -> Spec {
    Spec { format: Format::FLOAT32NE, channels: 1, rate }
}

/// Recording: one period per read, with the fragment size set so each read returns exactly one period.
pub fn capture_attr(frames: usize) -> BufferAttr {
    BufferAttr {
        maxlength: bytes(frames) * 4,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: bytes(frames),
    }
}

/// Playback: a small server-side target and a small prebuffer, so latency stays near two periods.
pub fn playback_attr(frames: usize, rate: u32) -> BufferAttr {
    let target = period_frames(rate, PLAYBACK_TARGET_MS);
    BufferAttr {
        maxlength: u32::MAX,
        tlength: bytes(target),
        prebuf: bytes(frames),
        minreq: bytes(frames),
        fragsize: u32::MAX,
    }
}

pub fn spawn_capture(job: CaptureJob) -> Result<JoinHandle<()>> {
    Ok(thread::Builder::new().name("hushcore-capture".into()).spawn(move || capture_loop(job))?)
}

pub fn spawn_render(job: RenderJob) -> Result<JoinHandle<Consumer<f32>>> {
    let name = if job.is_main { "hushcore-render" } else { "hushcore-monitor" };
    Ok(thread::Builder::new().name(name.into()).spawn(move || render_loop(job))?)
}

fn latency_us(simple: &Simple) -> u64 {
    simple.get_latency().map_or(0, |m| m.0)
}

fn capture_loop(job: CaptureJob) {
    let CaptureJob { simple, mut processor, shared, frames, ready } = job;
    let mut bytes = vec![0u8; frames * std::mem::size_of::<f32>()];
    let mut announced = false;
    while !shared.stop.load(Relaxed) {
        if let Err(e) = simple.read(&mut bytes) {
            if !announced {
                let _ = ready.send(Err(format!("capture read failed: {e}")));
            } else if !shared.stop.load(Relaxed) {
                shared.fail(FAIL_CAPTURE);
            }
            return;
        }
        if !announced {
            announced = true;
            let _ = ready.send(Ok(latency_us(&simple)));
        }
        processor.packet(frames, |offset, dst| decode(&bytes, offset, dst));
    }
}

/// Copies `dst.len()` frames starting at frame `offset` of `bytes` into `dst`.
fn decode(bytes: &[u8], offset: usize, dst: &mut [f32]) {
    let start = offset * 4;
    for (sample, raw) in dst.iter_mut().zip(bytes[start..].chunks_exact(4)) {
        *sample = f32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
    }
}

fn encode(bytes: &mut [u8], offset: usize, samples: &[f32]) {
    for (i, sample) in samples.iter().enumerate() {
        let at = (offset + i) * 4;
        bytes[at..at + 4].copy_from_slice(&sample.to_ne_bytes());
    }
}

fn render_loop(job: RenderJob) -> Consumer<f32> {
    let RenderJob { simple, consumer, shared, stop, is_main, muted, bounds, frames, ready } = job;
    let mut ring = RingRenderer::new(consumer, shared.clone(), is_main, muted, bounds);
    if !is_main {
        // Audio queued before the monitor was enabled is stale.
        ring.drop_stale();
    } else {
        // Start only once capture has delivered audio, so the first writes do not find an empty ring.
        let deadline = Instant::now() + FIRST_PACKET_DEADLINE;
        while shared.proc_frames.load(Relaxed) == 0 && !stop.load(Relaxed) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
    let mut bytes = vec![0u8; frames * std::mem::size_of::<f32>()];
    let mut announced = false;
    while !stop.load(Relaxed) {
        let written = ring.render(frames, |offset, samples| encode(&mut bytes, offset, samples));
        bytes[written * 4..].fill(0);
        if let Err(e) = simple.write(&bytes) {
            if !announced {
                let _ = ready.send(Err(format!("playback write failed: {e}")));
            } else if !stop.load(Relaxed) {
                if is_main {
                    shared.fail(FAIL_RENDER);
                } else {
                    shared.monitor_active.store(false, Relaxed);
                    shared.fail(FAIL_MONITOR);
                }
            }
            break;
        }
        if !announced {
            announced = true;
            let _ = ready.send(Ok(latency_us(&simple)));
        }
    }
    ring.into_consumer()
}

/// Opens one connection for a stream on the control thread, so a refused device is reported
/// synchronously from `Session::start`.
pub fn open(direction: Direction, device: &str, label: &str, rate: u32, attr: &BufferAttr) -> Result<Simple> {
    let spec = float_spec(rate);
    Simple::new(None, "HushCore", direction, Some(device), label, &spec, None, Some(attr))
        .map_err(|e| anyhow::anyhow!("cannot open {label} on \"{device}\": {e}"))
}
