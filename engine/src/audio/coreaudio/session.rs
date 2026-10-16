//! A running Core Audio session: the input and output streams, the monitor stream, and the threads that
//! report silence and permission problems. cpal runs the stream callbacks on Core Audio's IO threads, which
//! already run at real-time priority, so this file requests no thread priority. The callbacks only move
//! samples between the core rings and the device and set atomics: they never allocate, lock or log.

use std::io;
use std::sync::atomic::{
    AtomicBool,
    Ordering::{Relaxed, Release},
};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Error as CpalError, ErrorKind, InputCallbackInfo, OutputCallbackInfo, Stream, StreamConfig};
use rtrb::Consumer;

use super::devices::{self, Flow};
use super::format::{self, PREFERRED_FRAMES};
use crate::audio::core::ring::RingBounds;
use crate::audio::core::{
    make_rings, spawn_status, Controller, Diagnostics, Processor, RingRenderer, Rings, SessionLinks, Shared,
    StartRequest, StatusParams, StoppedOnce, StreamMode, FAIL_CAPTURE, FAIL_MONITOR, FAIL_RENDER,
};
use crate::ipc::Event;

/// Largest callback the rings and the processor scratch are sized for. The real period is read from each
/// stream (`Stream::buffer_size`); this only bounds memory and capacity, so a larger device callback is
/// still handled (the processor splits it, the ring drops overflow and counts it).
const MAX_FRAMES: u32 = 4096;

const PERMISSION_HINT: &str = "macOS denied microphone access: System Settings → Privacy & Security → Microphone, allow the HushCore backend's terminal/app";
const SILENCE_HINT: &str = "the input has delivered only silence for 1 s; if this is unexpected, check System Settings → Privacy & Security → Microphone for the app that started HushCore";

/// Ring cushion in capture packets. Input and output are separate devices on independent clocks, so the
/// first output callback can run before the first input packet (the start is gated on that packet, see
/// `wait_for_capture`); the cushion then absorbs callback jitter.
const CUSHION_PACKETS: f32 = 0.5;
const CAPTURE_GATE: Duration = Duration::from_secs(2);
/// Upper bound on waiting for the monitor ring to come back from a stream that is closing.
const HANDBACK_WAIT: Duration = Duration::from_millis(500);
/// Input that delivers only zeros for this long, after callbacks have started, gets the silence hint.
const SILENCE_WINDOW: Duration = Duration::from_secs(1);
const WATCH_TICK: Duration = Duration::from_millis(50);

/// A running session. Not `Send`-bound: the supervisor owns it on the control thread, so the streams never
/// cross threads (cpal's `Stream` is `Send` only by composition on macOS, and nothing relies on it).
pub struct Session {
    shared: Arc<Shared>,
    links: SessionLinks,
    stopped: StoppedOnce,
    rate: u32,
    bounds: RingBounds,
    input: Option<Stream>,
    output: Option<Stream>,
    monitor: Option<Stream>,
    /// `None` while a monitor stream holds the ring; it comes back through `monitor_return`.
    monitor_consumer: Option<Consumer<f32>>,
    monitor_return: Receiver<Consumer<f32>>,
    monitor_handback: Sender<Consumer<f32>>,
    status_thread: Option<JoinHandle<()>>,
    watch_thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Opens the input and output at the input device's default rate and starts them, then emits `started`.
    pub fn start(req: &StartRequest, controller: &Arc<Controller>, links: &SessionLinks) -> Result<Session> {
        let events = &links.events;
        if req.mode == StreamMode::Exclusive {
            let _ = events.send(Event::Error {
                message: "exclusive mode is WASAPI only; using shared mode".into(),
                fatal: false,
            });
        }

        let host = devices::host();
        let input = devices::resolve(&host, Flow::Input, &req.input, false)?;
        let output = devices::resolve(&host, Flow::Output, &req.output, true)?;

        let in_config = devices::default_config(&input.device, Flow::Input)?;
        let rate = in_config.sample_rate();
        if !format::supports(&devices::supported_configs(&input.device, Flow::Input)?, rate) {
            bail!("input \"{}\" cannot run at {rate} Hz as 32-bit float", input.name);
        }
        let out_config = devices::default_config(&output.device, Flow::Output)?;
        if !format::supports(&devices::supported_configs(&output.device, Flow::Output)?, rate) {
            bail!(
                "output \"{}\" cannot run at {rate} Hz as 32-bit float; choose a 44.1 or 48 kHz output such as HushCore Sink",
                output.name
            );
        }

        // The rings are created before any stream exists, so they are sized for the largest callback. Their
        // pre-filled silence is set afterwards, from the input's real period.
        let max_ms = format::period_ms(MAX_FRAMES, rate);
        let Rings { main_producer, main_consumer, monitor_producer, monitor_consumer, .. } =
            make_rings(rate, max_ms, max_ms, 0.0);

        let shared = Arc::new(Shared::new(req.mute_output));
        let permission = Arc::new(AtomicBool::new(false));
        let heard = Arc::new(AtomicBool::new(false));
        let in_channels = usize::from(in_config.channels()).max(1);
        let out_channels = usize::from(out_config.channels()).max(1);

        let mut processor = Processor::new(
            rate,
            MAX_FRAMES as usize,
            main_producer,
            monitor_producer,
            shared.clone(),
            controller.clone(),
        );
        let heard_in = heard.clone();
        let input_stream = input
            .device
            .build_input_stream::<f32, _, _>(
                StreamConfig {
                    channels: in_config.channels(),
                    sample_rate: rate,
                    buffer_size: format::buffer_size(in_config.buffer_size(), PREFERRED_FRAMES),
                },
                move |data: &[f32], _: &InputCallbackInfo| {
                    if !heard_in.load(Relaxed) && data.iter().any(|&s| s != 0.0) {
                        heard_in.store(true, Relaxed);
                    }
                    processor.packet(data.len() / in_channels, |offset, dst| {
                        format::downmix(data, in_channels, offset, dst)
                    });
                },
                error_hook(ErrorSink {
                    shared: shared.clone(),
                    which: FAIL_CAPTURE,
                    permission: Some(permission.clone()),
                }),
                None,
            )
            .map_err(|e| open_failed(events, "input", e))?;

        let capture_frames = buffer_frames(&input_stream, "input")?;
        let capture_ms = format::period_ms(capture_frames, rate);
        let cushion = (capture_frames as f32 * CUSHION_PACKETS).round() as usize;
        let bounds = RingBounds::new(cushion, capture_frames as usize);
        let mut ring = RingRenderer::new(main_consumer, shared.clone(), true, req.mute_output, bounds);

        let output_stream = output
            .device
            .build_output_stream::<f32, _, _>(
                StreamConfig {
                    channels: out_config.channels(),
                    sample_rate: rate,
                    buffer_size: format::buffer_size(out_config.buffer_size(), PREFERRED_FRAMES),
                },
                move |data: &mut [f32], _: &OutputCallbackInfo| render_into(&mut ring, data, out_channels),
                error_hook(ErrorSink { shared: shared.clone(), which: FAIL_RENDER, permission: None }),
                None,
            )
            .map_err(|e| open_failed(events, "output", e))?;
        let render_frames = buffer_frames(&output_stream, "output")?;
        let render_ms = format::period_ms(render_frames, rate);

        input_stream.play().map_err(|e| anyhow!("input stream did not start: {e}"))?;
        wait_for_capture(&shared);
        output_stream.play().map_err(|e| anyhow!("output stream did not start: {e}"))?;

        let (handback, monitor_return) = channel();
        let mut session = Session {
            shared: shared.clone(),
            links: links.clone(),
            stopped: StoppedOnce::default(),
            rate,
            bounds,
            input: Some(input_stream),
            output: Some(output_stream),
            monitor: None,
            monitor_consumer: Some(monitor_consumer),
            monitor_return,
            monitor_handback: handback,
            status_thread: None,
            watch_thread: None,
        };
        session.status_thread =
            spawn_status(StatusParams { shared: shared.clone(), links: links.clone(), rate, capture_ms, render_ms });
        let watch_events = links.events.clone();
        match thread::Builder::new()
            .name("hushcore-permission".into())
            .spawn(move || watch_input(shared, permission, heard, watch_events))
        {
            Ok(handle) => session.watch_thread = Some(handle),
            Err(e) => {
                session.abort();
                return Err(io_failed(e));
            }
        }

        let _ = events.send(Event::Started {
            sample_rate: rate,
            mode: "shared",
            input_period_ms: capture_ms,
            output_period_ms: render_ms,
            latency_ms: capture_ms + 2.0 * render_ms,
        });
        Ok(session)
    }

    /// Starts or stops the monitor stream on `device`, replacing any monitor already running.
    pub fn set_monitor(&mut self, enabled: bool, device: &str) -> Result<()> {
        self.stop_monitor();
        if !enabled {
            return Ok(());
        }

        let host = devices::host();
        let endpoint = devices::resolve(&host, Flow::Output, device, false)?;
        let config = devices::default_config(&endpoint.device, Flow::Output)?;
        if !format::supports(&devices::supported_configs(&endpoint.device, Flow::Output)?, self.rate) {
            bail!("monitor \"{}\" cannot run at {} Hz as 32-bit float", endpoint.name, self.rate);
        }
        let channels = usize::from(config.channels()).max(1);
        let buffer = format::buffer_size(config.buffer_size(), PREFERRED_FRAMES);
        let consumer = self
            .monitor_consumer
            .take()
            .ok_or_else(|| anyhow!("monitor ring was not returned by the previous monitor; restart the session"))?;

        // Audio queued before monitoring was switched on is stale, so it is dropped on the control thread.
        let mut ring = RingRenderer::new(consumer, self.shared.clone(), false, false, self.bounds);
        ring.drop_stale();
        let mut sink = MonitorSink { ring: Some(ring), back: self.monitor_handback.clone() };
        let errors = ErrorSink { shared: self.shared.clone(), which: FAIL_MONITOR, permission: None };

        let built = endpoint.device.build_output_stream::<f32, _, _>(
            StreamConfig { channels: config.channels(), sample_rate: self.rate, buffer_size: buffer },
            move |data: &mut [f32], _: &OutputCallbackInfo| match sink.ring.as_mut() {
                Some(ring) => render_into(ring, data, channels),
                None => data.fill(0.0),
            },
            error_hook(errors),
            None,
        );
        let stream = match built {
            Ok(stream) => stream,
            Err(e) => {
                self.reclaim_monitor_ring();
                bail!("monitor could not open \"{}\": {e}", endpoint.name);
            }
        };
        if let Err(e) = stream.play() {
            drop(stream);
            self.reclaim_monitor_ring();
            bail!("monitor did not start: {e}");
        }
        self.monitor = Some(stream);
        self.shared.monitor_active.store(true, Release);
        Ok(())
    }

    fn stop_monitor(&mut self) {
        self.shared.monitor_active.store(false, Release);
        if let Some(stream) = self.monitor.take() {
            let _ = stream.pause();
            drop(stream);
            self.reclaim_monitor_ring();
        }
    }

    /// Takes back the monitor's ring consumer once its stream has closed.
    fn reclaim_monitor_ring(&mut self) {
        if let Ok(consumer) = self.monitor_return.recv_timeout(HANDBACK_WAIT) {
            self.monitor_consumer = Some(consumer);
        }
    }

    /// Counters since `start`, read without locking.
    pub fn diagnostics(&self) -> Diagnostics {
        self.shared.diagnostics()
    }

    /// Stops the callbacks and threads and emits `stopped` once. Safe to call repeatedly.
    fn shutdown(&mut self) {
        self.shared.stop.store(true, Release);
        self.stop_monitor();
        for stream in [self.input.take(), self.output.take()].into_iter().flatten() {
            let _ = stream.pause();
            drop(stream);
        }
        for handle in [self.status_thread.take(), self.watch_thread.take()].into_iter().flatten() {
            let _ = handle.join();
        }
        self.stopped.emit(&self.links.events);
    }

    /// Tears down after a failed start without announcing `stopped`.
    fn abort(&mut self) {
        self.stopped.suppress();
        self.shutdown();
    }

    /// Stops the session and emits `stopped`.
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

/// Frames per callback the stream reports it will deliver. cpal answers this right after creation.
fn buffer_frames(stream: &Stream, side: &str) -> Result<u32> {
    stream.buffer_size().map_err(|e| anyhow!("cannot read the {side} buffer size: {e}"))
}

/// Waits (bounded) for the first capture packet, so the output never starts on an empty ring.
fn wait_for_capture(shared: &Shared) {
    let deadline = Instant::now() + CAPTURE_GATE;
    while shared.proc_frames.load(Relaxed) == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
}

/// Fills one device buffer from the ring: mono frames go to every channel, and frames the ring could not
/// supply are silenced.
fn render_into(ring: &mut RingRenderer, data: &mut [f32], channels: usize) {
    let frames = data.len() / channels;
    let written = ring.render(frames, |offset, samples| {
        let start = offset * channels;
        format::upmix(samples, channels, &mut data[start..start + samples.len() * channels]);
    });
    data[written * channels..].fill(0.0);
}

/// What a cpal error callback does. It runs on Core Audio's threads, so it only sets flags and counters; the
/// status thread turns a device loss into a supervised reconnect.
struct ErrorSink {
    shared: Arc<Shared>,
    which: u8,
    /// Set for the input, where macOS reports a refused microphone.
    permission: Option<Arc<AtomicBool>>,
}

impl ErrorSink {
    fn report(&self, err: CpalError) {
        match err.kind() {
            // Core Audio reports every HAL processor overload as Xrun on every stream. Only capture has a
            // counter that matches it (a lost input period); render overloads are already counted by the ring.
            ErrorKind::Xrun => {
                if self.which == FAIL_CAPTURE {
                    self.shared.counts.discontinuities.fetch_add(1, Relaxed);
                }
            }
            _ if format::is_permission_error(&err) => {
                if let Some(flag) = &self.permission {
                    flag.store(true, Relaxed);
                }
            }
            // Only a device that is gone or a stream that is invalidated needs a reconnect. Other kinds, such
            // as a route change (cpal keeps the stream running), are not failures.
            ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated | ErrorKind::HostUnavailable => {
                if self.which == FAIL_MONITOR {
                    self.shared.monitor_active.store(false, Relaxed);
                }
                self.shared.fail(self.which);
            }
            _ => {}
        }
    }
}

fn error_hook(sink: ErrorSink) -> impl FnMut(CpalError) + Send + 'static {
    move |err: CpalError| sink.report(err)
}

/// Owns the monitor's ring consumer while the monitor stream exists and hands it back when the stream is
/// dropped, so the next monitor can reuse the ring. Dropping a cpal stream frees its callback synchronously.
struct MonitorSink {
    ring: Option<RingRenderer>,
    back: Sender<Consumer<f32>>,
}

impl Drop for MonitorSink {
    fn drop(&mut self) {
        if let Some(ring) = self.ring.take() {
            let _ = self.back.send(ring.into_consumer());
        }
    }
}

/// Reports a problem with the input once, as a non-fatal error. A refused microphone (`PermissionDenied`) gets
/// the actionable hint; silence gets a softer one, since a muted microphone also delivers only zeros. The
/// stream stays up either way: reopening it into the same denial would only repeat the error.
fn watch_input(shared: Arc<Shared>, permission: Arc<AtomicBool>, heard: Arc<AtomicBool>, events: Sender<Event>) {
    let started = Instant::now();
    while !shared.stop.load(Relaxed) {
        let message = if permission.load(Relaxed) {
            Some(PERMISSION_HINT)
        } else if started.elapsed() >= SILENCE_WINDOW
            && shared.counts.capture_packets.load(Relaxed) > 0
            && !heard.load(Relaxed)
        {
            Some(SILENCE_HINT)
        } else {
            None
        };
        if let Some(message) = message {
            let _ = events.send(Event::Error { message: message.into(), fatal: false });
            return;
        }
        thread::sleep(WATCH_TICK);
    }
}

/// Builds the error for a stream that could not be opened. A refused microphone also reports the actionable
/// hint, since the supervisor only shows the wrapped cpal message.
fn open_failed(events: &Sender<Event>, side: &str, err: CpalError) -> anyhow::Error {
    if format::is_permission_error(&err) {
        let _ = events.send(Event::Error { message: PERMISSION_HINT.into(), fatal: false });
    }
    anyhow!("could not open {side}: {err}")
}

fn io_failed(err: io::Error) -> anyhow::Error {
    anyhow!("cannot start the permission watcher thread: {err}")
}
