//! A running engine: the streams, their threads, the monitor, and the status thread.
//! The control thread (main) drives this; the audio threads never block on it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::{AcqRel, Relaxed, Release}};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use arc_swap::ArcSwap;
use rtrb::{Consumer, RingBuffer};
use windows::Win32::Media::Audio::{eCapture, eRender, IAudioCaptureClient, IAudioRenderClient, IMMDevice};

use super::capture::{self, CaptureJob};
use super::clients::{open_capture, open_render, InitKind, Opened, StreamMode};
use super::devices::{resolve, Enumerator};
use super::ring::{frames_to_ms, RingBounds};
use super::render::{self, RenderJob};
use crate::dsp::Metrics;
use crate::ipc::{Event, Status};
use crate::settings::Settings;

pub const FAIL_NONE: u8 = 0;
pub const FAIL_CAPTURE: u8 = 1;
pub const FAIL_RENDER: u8 = 2;
pub const FAIL_MONITOR: u8 = 3;

const STATUS_INTERVAL: Duration = Duration::from_millis(33);
const THREAD_READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Target fill in render periods, per mode. The sweeps (12 busy-loop processes, 60 s each): shared held zero
/// steady-state underruns down to 0.0 and added latency with more; exclusive failed at 0.0 and held at 0.5.
pub const RING_PREFILL_SHARED_PERIODS: f32 = 0.0;
pub const RING_PREFILL_EXCLUSIVE_PERIODS: f32 = 0.5;
const EXCLUSIVE_RATE_CHECK: Duration = Duration::from_millis(500);
const EXCLUSIVE_RATE_TOLERANCE: f64 = 0.10;

/// Live settings. `generation` moves after every store so the capture thread can check cheaply.
pub struct Controller {
    pub settings: ArcSwap<Settings>,
    pub generation: AtomicU64,
}

impl Default for Controller {
    fn default() -> Self {
        Self { settings: ArcSwap::from_pointee(Settings::default()), generation: AtomicU64::new(1) }
    }
}

impl Controller {
    pub fn set_settings(&self, settings: Settings) {
        self.settings.store(Arc::new(settings));
        self.generation.fetch_add(1, Release);
    }

}

/// State shared by the session's threads. Audio threads touch only atomics and the try_lock mailbox.
pub struct Shared {
    pub stop: Arc<AtomicBool>,
    pub failed: AtomicU8,
    pub monitor_active: AtomicBool,
    pub mute_output: bool,
    pub mailbox: Mutex<Metrics>,
    pub proc_ns: AtomicU64,
    pub proc_frames: AtomicU64,
    pub render_fill_bits: AtomicU32,
    pub chain_latency_samples: AtomicU32,
    pub counts: Counts,
}

/// Event counters. Each is bumped from one audio thread with a relaxed atomic add.
#[derive(Default)]
pub struct Counts {
    /// Capture packets flagged DATA_DISCONTINUITY by WASAPI.
    pub discontinuities: AtomicU64,
    /// Capture-side pushes that found the ring full (newest samples dropped).
    pub capture_drops: AtomicU64,
    /// Render callbacks that dropped the oldest queued samples to bound latency.
    pub render_discards: AtomicU64,
    /// Drift corrections (one sample skipped or held); deliberate, so not xruns.
    pub slew_drops: AtomicU64,
    pub slew_repeats: AtomicU64,
    pub discarded_frames: AtomicU64,
    /// Render callbacks that found the ring short and padded with silence.
    pub underruns: AtomicU64,
    pub rendered_frames: AtomicU64,
    pub silence_frames: AtomicU64,
    pub capture_packets: AtomicU64,
    pub render_callbacks: AtomicU64,
}

/// Plain snapshot of `Counts` plus the frames the capture thread produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub produced_frames: u64,
    pub rendered_frames: u64,
    pub silence_frames: u64,
    pub discarded_frames: u64,
    pub underruns: u64,
    pub discontinuities: u64,
    pub capture_drops: u64,
    pub render_discards: u64,
    pub slew_drops: u64,
    pub slew_repeats: u64,
    pub capture_packets: u64,
    pub render_callbacks: u64,
}

impl Diagnostics {
    /// The protocol's `xruns`: every glitch source added together.
    pub fn xruns(&self) -> u64 {
        self.underruns + self.discontinuities + self.capture_drops + self.render_discards
    }
}

impl Shared {
    fn new(mute_output: bool) -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            failed: AtomicU8::new(FAIL_NONE),
            monitor_active: AtomicBool::new(false),
            mute_output,
            mailbox: Mutex::new(Metrics::default()),
            proc_ns: AtomicU64::new(0),
            proc_frames: AtomicU64::new(0),
            render_fill_bits: AtomicU32::new(0f32.to_bits()),
            chain_latency_samples: AtomicU32::new(0),
            counts: Counts::default(),
        }
    }

    fn diagnostics(&self) -> Diagnostics {
        let c = &self.counts;
        Diagnostics {
            produced_frames: self.proc_frames.load(Relaxed),
            rendered_frames: c.rendered_frames.load(Relaxed),
            silence_frames: c.silence_frames.load(Relaxed),
            discarded_frames: c.discarded_frames.load(Relaxed),
            underruns: c.underruns.load(Relaxed),
            discontinuities: c.discontinuities.load(Relaxed),
            capture_drops: c.capture_drops.load(Relaxed),
            render_discards: c.render_discards.load(Relaxed),
            slew_drops: c.slew_drops.load(Relaxed),
            slew_repeats: c.slew_repeats.load(Relaxed),
            capture_packets: c.capture_packets.load(Relaxed),
            render_callbacks: c.render_callbacks.load(Relaxed),
        }
    }
}

#[derive(Clone)]
pub struct StartRequest {
    pub input: String,
    pub output: String,
    pub monitor: String,
    pub monitor_enabled: bool,
    pub mode: StreamMode,
    /// Diagnostics only: render silence no matter what the chain produces.
    pub mute_output: bool,
}

struct MonitorRun {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Consumer<f32>>,
}

pub struct Session {
    shared: Arc<Shared>,
    events: Sender<Event>,
    stopped_sent: Arc<AtomicBool>,
    rate: u32,
    bounds: RingBounds,
    capture_ms: f32,
    render_ms: f32,
    capture_thread: Option<JoinHandle<()>>,
    render_thread: Option<JoinHandle<Consumer<f32>>>,
    status_thread: Option<JoinHandle<()>>,
    monitor: Option<MonitorRun>,
    monitor_consumer: Option<Consumer<f32>>,
}

struct OpenedPair {
    capture: Opened,
    capture_service: IAudioCaptureClient,
    render: Opened,
    render_service: IAudioRenderClient,
}

fn open_pair(input: &IMMDevice, output: &IMMDevice, mode: StreamMode) -> Result<OpenedPair> {
    let (capture, capture_service) = open_capture(input, mode)?;
    let (render, render_service) = open_render(output, mode, capture.format.sample_rate)?;
    Ok(OpenedPair { capture, capture_service, render, render_service })
}

fn ms_to_frames(ms: f32, rate: u32) -> usize {
    (ms * rate as f32 / 1000.0).round() as usize
}

impl Session {
    /// Opens both streams and starts their threads. Emits `error` (exclusive fallback) and `started`.
    pub fn start(req: &StartRequest, controller: &Arc<Controller>, events: &Sender<Event>) -> Result<Session> {
        let enumerator = Enumerator::new()?;
        let input = resolve(&enumerator, eCapture, &req.input, false)?;
        let output = resolve(&enumerator, eRender, &req.output, true)?;

        let pair = match open_pair(&input.device, &output.device, req.mode) {
            Ok(pair) => pair,
            Err(e) if req.mode == StreamMode::Exclusive => {
                let _ = events.send(Event::Error {
                    message: format!("exclusive mode unavailable ({e:#}); using shared mode"),
                    fatal: false,
                });
                open_pair(&input.device, &output.device, StreamMode::Shared)?
            }
            Err(e) => return Err(e),
        };

        let rate = pair.capture.format.sample_rate;
        if pair.render.format.sample_rate != rate {
            bail!("render rate {} does not match capture rate {rate}", pair.render.format.sample_rate);
        }
        let capture_ms = pair.capture.period_ms;
        let render_ms = pair.render.period_ms;
        let mode_name = match (pair.capture.init, pair.render.init) {
            (InitKind::Exclusive, InitKind::Exclusive) => "exclusive",
            (InitKind::LowLatency, InitKind::LowLatency) => "shared-low-latency",
            _ => "shared",
        };

        // Target fill = one render period, which is also the pre-fill (see ring::RingBounds).
        // The render start is gated on the first capture packet, which provides the cushion.
        let prefill_periods = if mode_name == "exclusive" { RING_PREFILL_EXCLUSIVE_PERIODS } else { RING_PREFILL_SHARED_PERIODS };
        let prefill = (ms_to_frames(render_ms, rate) as f32 * prefill_periods).round() as usize;
        let bounds = RingBounds::new(prefill, ms_to_frames(capture_ms, rate));
        let capacity = bounds.max * 2 + 1024;
        let (mut main_producer, main_consumer) = RingBuffer::<f32>::new(capacity);
        let (monitor_producer, monitor_consumer) = RingBuffer::<f32>::new(capacity);
        for _ in 0..bounds.target {
            let _ = main_producer.push(0.0);
        }

        let shared = Arc::new(Shared::new(req.mute_output));
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let capture_thread = capture::spawn(CaptureJob {
            opened: pair.capture,
            service: pair.capture_service,
            main_out: main_producer,
            monitor_out: monitor_producer,
            shared: shared.clone(),
            controller: controller.clone(),
            ready: ready_tx.clone(),
        })?;
        let render_thread = render::spawn(RenderJob {
            opened: pair.render,
            service: pair.render_service,
            consumer: main_consumer,
            shared: shared.clone(),
            stop: shared.stop.clone(),
            is_main: true,
            muted: shared.mute_output,
            bounds,
            ready: ready_tx,
        })?;

        let mut session = Session {
            shared: shared.clone(),
            events: events.clone(),
            stopped_sent: Arc::new(AtomicBool::new(false)),
            rate,
            bounds,
            capture_ms,
            render_ms,
            capture_thread: Some(capture_thread),
            render_thread: Some(render_thread),
            status_thread: None,
            monitor: None,
            monitor_consumer: Some(monitor_consumer),
        };

        for _ in 0..2 {
            match ready_rx.recv_timeout(THREAD_READY_TIMEOUT) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    session.abort();
                    bail!("audio thread failed to start: {e}");
                }
                Err(_) => {
                    session.abort();
                    bail!("audio threads did not report readiness");
                }
            }
        }

        // An exclusive stream whose frames do not arrive at the negotiated rate would play at the wrong
        // speed, so it is replaced by a shared stream rather than announced as started.
        if mode_name == "exclusive" {
            let (captured, rendered) = session.measure_frame_rates(EXCLUSIVE_RATE_CHECK);
            let off = |measured: f64| (measured / rate as f64 - 1.0).abs() > EXCLUSIVE_RATE_TOLERANCE;
            if off(captured) || off(rendered) {
                let _ = events.send(Event::Error {
                    message: format!(
                        "exclusive mode moved {captured:.0} frames/s in and {rendered:.0} frames/s out at {rate} Hz; using shared mode"
                    ),
                    fatal: false,
                });
                session.abort();
                let fallback = StartRequest { mode: StreamMode::Shared, ..req.clone() };
                return Session::start(&fallback, controller, events);
            }
        }

        let _ = events.send(Event::Started {
            sample_rate: rate,
            mode: mode_name,
            input_period_ms: capture_ms,
            output_period_ms: render_ms,
            // One pre-filled render period plus the two device periods.
            latency_ms: capture_ms + 2.0 * render_ms,
        });
        session.spawn_status();
        Ok(session)
    }

    /// Starts or stops the monitor stream. Replaces any monitor that is already running.
    pub fn set_monitor(&mut self, enabled: bool, device: &str) -> Result<()> {
        self.stop_monitor();
        if !enabled {
            return Ok(());
        }
        let enumerator = Enumerator::new()?;
        let endpoint = resolve(&enumerator, eRender, device, false)?;
        let (opened, service) = open_render(&endpoint.device, StreamMode::Shared, self.rate)?;
        let consumer = self.monitor_consumer.take().ok_or_else(|| anyhow!("monitor ring missing"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let thread = render::spawn(RenderJob {
            opened,
            service,
            consumer,
            shared: self.shared.clone(),
            stop: stop.clone(),
            is_main: false,
            muted: false,
            bounds: self.bounds,
            ready: ready_tx,
        })?;

        let failure = match ready_rx.recv_timeout(THREAD_READY_TIMEOUT) {
            Ok(Ok(())) => {
                self.shared.monitor_active.store(true, Release);
                self.monitor = Some(MonitorRun { stop, thread });
                return Ok(());
            }
            Ok(Err(e)) => format!("monitor start failed: {e}"),
            Err(_) => "monitor did not start in time".to_string(),
        };
        stop.store(true, Release);
        if let Ok(consumer) = thread.join() {
            self.monitor_consumer = Some(consumer);
        }
        bail!(failure)
    }

    fn stop_monitor(&mut self) {
        self.shared.monitor_active.store(false, Release);
        if let Some(run) = self.monitor.take() {
            run.stop.store(true, Release);
            if let Ok(consumer) = run.thread.join() {
                self.monitor_consumer = Some(consumer);
            }
        }
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.shared.diagnostics()
    }

    fn spawn_status(&mut self) {
        let params = StatusParams {
            shared: self.shared.clone(),
            events: self.events.clone(),
            stopped_sent: self.stopped_sent.clone(),
            rate: self.rate,
            capture_ms: self.capture_ms,
            render_ms: self.render_ms,
        };
        self.status_thread = thread::Builder::new().name("hushcore-status".into()).spawn(move || status_loop(params)).ok();
    }

    /// Stops threads and emits `stopped` once. Safe to call repeatedly.
    fn shutdown(&mut self) {
        self.shared.stop.store(true, Release);
        self.stop_monitor();
        if let Some(handle) = self.status_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.capture_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.render_thread.take() {
            let _ = handle.join();
        }
        if !self.stopped_sent.swap(true, AcqRel) {
            let _ = self.events.send(Event::Stopped);
        }
    }

    /// Frames per second the capture thread produced and the render thread consumed over `window`.
    fn measure_frame_rates(&self, window: Duration) -> (f64, f64) {
        let rendered = || {
            self.shared.counts.rendered_frames.load(Relaxed) + self.shared.counts.silence_frames.load(Relaxed)
        };
        let captured_before = self.shared.proc_frames.load(Relaxed);
        let rendered_before = rendered();
        let started = std::time::Instant::now();
        thread::sleep(window);
        let secs = started.elapsed().as_secs_f64();
        let captured = self.shared.proc_frames.load(Relaxed) - captured_before;
        let out = rendered() - rendered_before;
        (captured as f64 / secs, out as f64 / secs)
    }

    /// Tear down after a failed start without announcing `stopped`.
    fn abort(&mut self) {
        self.stopped_sent.store(true, Release);
        self.shutdown();
    }

    pub fn stop(mut self) {
        self.shutdown();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct StatusParams {
    shared: Arc<Shared>,
    events: Sender<Event>,
    stopped_sent: Arc<AtomicBool>,
    rate: u32,
    capture_ms: f32,
    render_ms: f32,
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn status_loop(p: StatusParams) {
    let mut last_ns = 0u64;
    let mut last_frames = 0u64;
    let mut cpu = 0.0f32;
    let mut have_cpu = false;

    while !p.shared.stop.load(Relaxed) {
        thread::sleep(STATUS_INTERVAL);
        if p.shared.stop.load(Relaxed) {
            break;
        }

        match p.shared.failed.swap(FAIL_NONE, Relaxed) {
            FAIL_NONE => {}
            FAIL_MONITOR => {
                let _ = p.events.send(Event::Error { message: "monitor output device lost".into(), fatal: false });
            }
            failure => {
                let which = if failure == FAIL_CAPTURE { "input" } else { "output" };
                let _ = p.events.send(Event::Error {
                    message: format!("{which} device lost; the engine stopped"),
                    fatal: false,
                });
                p.shared.stop.store(true, Release);
                if !p.stopped_sent.swap(true, AcqRel) {
                    let _ = p.events.send(Event::Stopped);
                }
                break;
            }
        }

        let ns = p.shared.proc_ns.load(Relaxed);
        let frames = p.shared.proc_frames.load(Relaxed);
        let (delta_ns, delta_frames) = (ns - last_ns, frames - last_frames);
        last_ns = ns;
        last_frames = frames;
        if delta_frames > 0 {
            let block_ns = delta_frames as f64 / p.rate as f64 * 1e9;
            let instant = (delta_ns as f64 / block_ns * 100.0) as f32;
            cpu = if have_cpu { cpu * 0.7 + instant * 0.3 } else { instant };
            have_cpu = true;
        }

        let metrics = p.shared.mailbox.lock().map(|m| m.clone()).unwrap_or_default();
        let fill_frames = f32::from_bits(p.shared.render_fill_bits.load(Relaxed));
        let chain_samples = p.shared.chain_latency_samples.load(Relaxed) as f32;
        let latency_ms = p.capture_ms
            + p.render_ms
            + frames_to_ms(fill_frames, p.rate)
            + frames_to_ms(chain_samples, p.rate);

        let status = Status {
            input_rms: finite_or(metrics.input_rms_db, -120.0),
            output_rms: finite_or(metrics.output_rms_db, -120.0),
            gate_state: metrics.gate_open,
            fft_data: metrics.fft.iter().map(|v| finite_or(*v, 0.0)).collect(),
            clipping: metrics.clipping,
            cpu_load: finite_or(cpu, 0.0).clamp(0.0, 100.0),
            latency_ms: finite_or(latency_ms, 0.0),
            sample_rate: p.rate,
            xruns: p.shared.diagnostics().xruns(),
        };
        if p.events.send(Event::Status(status)).is_err() {
            break;
        }
    }
}
