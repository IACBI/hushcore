//! A running PulseAudio-protocol session: the record and playback connections, their threads, the
//! monitor, and the status thread. The control thread drives this; audio threads never block on it.

use std::sync::atomic::{AtomicBool, Ordering::{Relaxed, Release}};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use libpulse_binding::stream::Direction;
use rtrb::Consumer;

use super::connection::{Node, Server, ServerInfo};
use super::devices::{resolve_input, resolve_output};
use super::modules;
use super::streams::{self, CaptureJob, RenderJob, CAPTURE_PERIOD_MS, RENDER_PERIOD_MS};
use crate::audio::core::names::is_hushcore_sink;
use crate::audio::core::ring::RingBounds;
use crate::audio::core::{
    make_rings, spawn_status, Controller, Diagnostics, Processor, SessionLinks, Shared, StartRequest, StatusParams,
    StoppedOnce, FAIL_CAPTURE, FAIL_RENDER,
};
use crate::ipc::Event;

const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Without a captured packet for this long, the input is treated as lost. `pa_simple_read` does not
/// return when its source is unloaded, so a device loss shows up only as a stall.
const STALL_LIMIT: Duration = Duration::from_millis(500);
const WATCHDOG_TICK: Duration = Duration::from_millis(50);
/// How often the watchdog asks the server whether the devices are still listed.
const PRESENCE_INTERVAL: Duration = Duration::from_millis(250);
/// How long teardown waits for a capture thread before leaving it behind (blocked in a read).
const CAPTURE_JOIN_LIMIT: Duration = Duration::from_secs(1);
/// Settle time before the first latency read, so the server has started both streams.
const LATENCY_SETTLE: Duration = Duration::from_millis(150);
/// Target fill in render periods. Tuned by the Docker harness; see driver/linux/README.md.
pub const RING_PREFILL_PERIODS: f32 = 1.0;

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
    frames: usize,
    capture_thread: Option<JoinHandle<()>>,
    render_thread: Option<JoinHandle<Consumer<f32>>>,
    status_thread: Option<JoinHandle<()>>,
    watchdog_thread: Option<JoinHandle<()>>,
    monitor: Option<MonitorRun>,
    monitor_consumer: Option<Consumer<f32>>,
}

/// Reports a lost device and exits. Two signals: no packet captured for `STALL_LIMIT`, and the
/// server no longer listing the input or the output by name. The second matters because the server
/// moves a stream whose device vanished to the default device, so capture keeps running on silence.
/// The watchdog has its own server connection, so it never touches the audio streams.
fn spawn_watchdog(shared: Arc<Shared>, input: String, output: String) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("hushcore-watchdog".into())
        .spawn(move || {
            let mut server: Option<Server> = Server::connect().ok();
            let mut last = shared.proc_frames.load(Relaxed);
            let mut since = Instant::now();
            let mut last_presence = Instant::now();
            while !shared.stop.load(Relaxed) {
                thread::sleep(WATCHDOG_TICK);
                let now = shared.proc_frames.load(Relaxed);
                if now != last {
                    last = now;
                    since = Instant::now();
                } else if since.elapsed() >= STALL_LIMIT {
                    shared.fail(FAIL_CAPTURE);
                    return;
                }
                if last_presence.elapsed() >= PRESENCE_INTERVAL {
                    last_presence = Instant::now();
                    match server.as_mut().map(|s| presence(s, &input, &output)) {
                        Some(Ok((true, true))) => {}
                        Some(Ok((false, _))) => {
                            shared.fail(FAIL_CAPTURE);
                            return;
                        }
                        Some(Ok((true, false))) => {
                            shared.fail(FAIL_RENDER);
                            return;
                        }
                        _ => {
                            // The server is gone, or the connection to it is; the streams are too.
                            shared.fail(FAIL_CAPTURE);
                            return;
                        }
                    }
                }
            }
        })
        .ok()
}

/// Whether the input source and the output sink are still listed, as (input, output).
fn presence(server: &mut Server, input: &str, output: &str) -> Result<(bool, bool)> {
    let info = server.server_info()?;
    let has_input = server.sources(&info)?.iter().any(|n| n.pulse_name == input);
    let has_output = server.sinks(&info)?.iter().any(|n| n.pulse_name == output);
    Ok((has_input, has_output))
}

/// Waits briefly for the capture thread. A thread still blocked in a read is left behind, so a
/// lost device cannot hang the supervisor's restart.
fn join_capture(handle: JoinHandle<()>) {
    let deadline = Instant::now() + CAPTURE_JOIN_LIMIT;
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

/// The server's devices and defaults, read in one pass.
struct Snapshot {
    info: ServerInfo,
    sinks: Vec<Node>,
    sources: Vec<Node>,
}

fn snapshot(server: &mut Server) -> Result<Snapshot> {
    let info = server.server_info()?;
    let sinks = server.sinks(&info)?;
    let sources = server.sources(&info)?;
    Ok(Snapshot { info, sinks, sources })
}

impl Session {
    /// Opens the streams and starts their threads. This backend has no exclusive mode, so an
    /// exclusive request runs shared and `started.mode` says so.
    pub fn start(req: &StartRequest, controller: &Arc<Controller>, links: &SessionLinks) -> Result<Session> {
        let mut server = Server::connect()?;
        let mut snap = snapshot(&mut server)?;
        // An empty output with no HushCore Sink: create the pair for this process (see modules.rs).
        let sink_missing = !snap.sinks.iter().any(|n| is_hushcore_sink(&n.description));
        if req.output.trim().is_empty() && sink_missing && modules::ensure_pair(&mut server, &snap.info)? {
            snap = snapshot(&mut server)?;
        }
        let input: Node = resolve_input(&snap.sources, &req.input)?.clone();
        let output: Node = resolve_output(&snap.sinks, &req.output, true)?.clone();
        drop(server);
        if input.rate == 0 {
            bail!("input device \"{}\" reports no sample rate", input.description);
        }
        let rate = input.rate;
        let frames = streams::period_frames(rate, CAPTURE_PERIOD_MS);
        let render_frames = streams::period_frames(rate, RENDER_PERIOD_MS);
        let capture = streams::open(
            Direction::Record,
            &input.pulse_name,
            "HushCore capture",
            rate,
            &streams::capture_attr(frames),
        )?;
        let playback = streams::open(
            Direction::Playback,
            &output.pulse_name,
            "HushCore output",
            rate,
            &streams::playback_attr(render_frames, rate),
        )?;

        let shared = Arc::new(Shared::new(req.mute_output));
        let rings = make_rings(rate, CAPTURE_PERIOD_MS, RENDER_PERIOD_MS, RING_PREFILL_PERIODS);
        let bounds = rings.bounds;
        let processor = Processor::new(
            rate,
            frames,
            rings.main_producer,
            rings.monitor_producer,
            shared.clone(),
            controller.clone(),
        );

        let (capture_ready, capture_rx) = channel();
        let (render_ready, render_rx) = channel();
        let capture_thread = streams::spawn_capture(CaptureJob {
            simple: capture,
            processor,
            shared: shared.clone(),
            frames,
            ready: capture_ready,
        })?;
        let render_thread = streams::spawn_render(RenderJob {
            simple: playback,
            consumer: rings.main_consumer,
            shared: shared.clone(),
            stop: shared.stop.clone(),
            is_main: true,
            muted: shared.mute_output,
            bounds,
            frames: render_frames,
            ready: render_ready,
        })?;

        let mut session = Session {
            shared: shared.clone(),
            links: links.clone(),
            stopped: StoppedOnce::default(),
            rate,
            bounds,
            frames,
            capture_thread: Some(capture_thread),
            render_thread: Some(render_thread),
            status_thread: None,
            watchdog_thread: None,
            monitor: None,
            monitor_consumer: Some(rings.monitor_consumer),
        };

        let input_us = match await_ready(&capture_rx) {
            Ok(us) => us,
            Err(e) => {
                session.abort();
                return Err(e.context("capture did not start"));
            }
        };
        let output_us = match await_ready(&render_rx) {
            Ok(us) => us,
            Err(e) => {
                session.abort();
                return Err(e.context("playback did not start"));
            }
        };
        thread::sleep(LATENCY_SETTLE);

        let _ = links.events.send(Event::Started {
            sample_rate: rate,
            mode: "shared",
            input_period_ms: CAPTURE_PERIOD_MS,
            output_period_ms: RENDER_PERIOD_MS,
            latency_ms: (input_us + output_us) as f32 / 1000.0,
        });
        session.watchdog_thread = spawn_watchdog(shared.clone(), input.pulse_name.clone(), output.pulse_name.clone());
        session.status_thread = spawn_status(StatusParams {
            shared,
            links: links.clone(),
            rate,
            capture_ms: CAPTURE_PERIOD_MS,
            render_ms: RENDER_PERIOD_MS,
        });
        Ok(session)
    }

    /// Starts or stops the monitor stream. Replaces any monitor that is already running.
    pub fn set_monitor(&mut self, enabled: bool, device: &str) -> Result<()> {
        self.stop_monitor();
        if !enabled {
            return Ok(());
        }
        let mut server = Server::connect()?;
        let snap = snapshot(&mut server)?;
        drop(server);
        let output = resolve_output(&snap.sinks, device, false)?.clone();
        let playback = streams::open(
            Direction::Playback,
            &output.pulse_name,
            "HushCore monitor",
            self.rate,
            &streams::playback_attr(self.frames, self.rate),
        )?;
        let consumer = self.monitor_consumer.take().ok_or_else(|| anyhow!("monitor ring missing"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let (ready, ready_rx) = channel();
        let thread = streams::spawn_render(RenderJob {
            simple: playback,
            consumer,
            shared: self.shared.clone(),
            stop: stop.clone(),
            is_main: false,
            muted: false,
            bounds: self.bounds,
            frames: self.frames,
            ready,
        })?;
        match await_ready(&ready_rx) {
            Ok(_) => {
                self.shared.monitor_active.store(true, Release);
                self.monitor = Some(MonitorRun { stop, thread });
                Ok(())
            }
            Err(e) => {
                stop.store(true, Release);
                if let Ok(consumer) = thread.join() {
                    self.monitor_consumer = Some(consumer);
                }
                Err(e.context("monitor did not start"))
            }
        }
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

    /// Stops threads and emits `stopped` once. Safe to call repeatedly.
    fn shutdown(&mut self) {
        self.shared.stop.store(true, Release);
        self.stop_monitor();
        if let Some(handle) = self.status_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.watchdog_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.capture_thread.take() {
            join_capture(handle);
        }
        if let Some(handle) = self.render_thread.take() {
            let _ = handle.join();
        }
        self.stopped.emit(&self.links.events);
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

fn await_ready(rx: &Receiver<Result<u64, String>>) -> Result<u64> {
    match rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ok(latency_us)) => Ok(latency_us),
        Ok(Err(message)) => Err(anyhow!(message)),
        Err(_) => Err(anyhow!("thread did not report readiness")),
    }
}
