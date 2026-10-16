//! A running WASAPI engine: the streams, their threads, the monitor, and the status thread.
//! The control thread (main) drives this; the audio threads never block on it.

use std::sync::atomic::{AtomicBool, Ordering::{Relaxed, Release}};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use rtrb::Consumer;
use windows::Win32::Media::Audio::{eCapture, eRender, IAudioCaptureClient, IAudioRenderClient, IMMDevice};

use super::capture::{self, CaptureJob};
use super::clients::{open_capture, open_render, InitKind, Opened, StreamMode};
use super::devices::{resolve, Enumerator};
use super::render::{self, RenderJob};
use crate::audio::core::ring::RingBounds;
use crate::audio::core::{
    make_rings_for_buffer, spawn_status, Controller, Diagnostics, SessionLinks, Shared, StartRequest, StatusParams, StoppedOnce,
};
use crate::ipc::Event;

const THREAD_READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Target fill in render periods, per mode. The sweeps (12 busy-loop processes, 60 s each): shared held zero
/// steady-state underruns down to 0.0 and added latency with more; exclusive failed at 0.0 and held at 0.5.
pub const RING_PREFILL_SHARED_PERIODS: f32 = 0.0;
pub const RING_PREFILL_EXCLUSIVE_PERIODS: f32 = 0.5;
const EXCLUSIVE_RATE_CHECK: Duration = Duration::from_millis(500);
const EXCLUSIVE_RATE_TOLERANCE: f64 = 0.10;

struct MonitorRun {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Consumer<f32>>,
}

pub struct Session {
    shared: Arc<Shared>,
    links: SessionLinks,
    stopped: StoppedOnce,
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

impl Session {
    /// Opens both streams and starts their threads. Emits `error` (exclusive fallback) and `started`.
    pub fn start(req: &StartRequest, controller: &Arc<Controller>, links: &SessionLinks) -> Result<Session> {
        let events = &links.events;
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

        // The render start is gated on the first capture packet, which provides the cushion.
        let prefill_periods = if mode_name == "exclusive" { RING_PREFILL_EXCLUSIVE_PERIODS } else { RING_PREFILL_SHARED_PERIODS };
        let rings = make_rings_for_buffer(rate, capture_ms, render_ms, prefill_periods, pair.render.buffer_frames as usize);
        let bounds = rings.bounds;

        let shared = Arc::new(Shared::new(req.mute_output));
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let capture_thread = capture::spawn(CaptureJob {
            opened: pair.capture,
            service: pair.capture_service,
            main_out: rings.main_producer,
            monitor_out: rings.monitor_producer,
            shared: shared.clone(),
            controller: controller.clone(),
            ready: ready_tx.clone(),
        })?;
        let render_thread = match render::spawn(RenderJob {
            opened: pair.render,
            service: pair.render_service,
            consumer: rings.main_consumer,
            shared: shared.clone(),
            stop: shared.stop.clone(),
            is_main: true,
            muted: shared.mute_output,
            bounds,
            ready: ready_tx,
        }) {
            Ok(thread) => thread,
            Err(e) => {
                // The capture thread already holds the input endpoint; release it before failing.
                shared.stop.store(true, Release);
                let _ = capture_thread.join();
                return Err(e);
            }
        };

        let mut session = Session {
            shared: shared.clone(),
            links: links.clone(),
            stopped: StoppedOnce::default(),
            rate,
            bounds,
            capture_ms,
            render_ms,
            capture_thread: Some(capture_thread),
            render_thread: Some(render_thread),
            status_thread: None,
            monitor: None,
            monitor_consumer: Some(rings.monitor_consumer),
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
                return Session::start(&fallback, controller, links);
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
        if !session.spawn_status() {
            session.abort();
            bail!("could not start the status thread");
        }
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

    /// Without the status thread nothing would notice a lost device, so failure aborts the start.
    fn spawn_status(&mut self) -> bool {
        self.status_thread = spawn_status(StatusParams {
            shared: self.shared.clone(),
            links: self.links.clone(),
            rate: self.rate,
            capture_ms: self.capture_ms,
            render_ms: self.render_ms,
        });
        self.status_thread.is_some()
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
        self.stopped.emit(&self.links.events);
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
        self.stopped.suppress();
        self.shutdown();
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Tears down without announcing `stopped`: the supervisor is about to reopen the devices.
    pub fn abandon(mut self) {
        self.abort();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}
