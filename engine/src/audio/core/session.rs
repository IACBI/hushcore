//! Pieces of a running session that do not depend on the OS: ring construction, the status thread,
//! and the hand-off of device failures to the supervisor in `main.rs`.

use std::sync::atomic::{AtomicBool, Ordering::{AcqRel, Relaxed, Release}};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use super::ring::{frames_to_ms, RingBounds};
use super::state::{Shared, FAIL_CAPTURE, FAIL_MONITOR, FAIL_NONE, FAIL_RENDER};
use crate::ipc::{Event, Inbound, Status};

const STATUS_INTERVAL: Duration = Duration::from_millis(33);

/// Where a session reports. `control` receives `Inbound::DeviceLost` so the supervisor can restart;
/// diagnostics callers that do not supervise pass a sender whose receiver they ignore.
#[derive(Clone)]
pub struct SessionLinks {
    pub events: Sender<Event>,
    pub control: Sender<Inbound>,
    /// Tags `DeviceLost`, so a failure from a session that was already replaced is ignored.
    pub session_id: u64,
}

pub fn ms_to_frames(ms: f32, rate: u32) -> usize {
    (ms * rate as f32 / 1000.0).round() as usize
}

pub struct Rings {
    pub main_producer: Producer<f32>,
    pub main_consumer: Consumer<f32>,
    pub monitor_producer: Producer<f32>,
    pub monitor_consumer: Consumer<f32>,
    pub bounds: RingBounds,
}

/// Builds the capture-to-render rings for the negotiated periods. The main ring is pre-filled with
/// `prefill_periods` render periods of silence (see `ring::RingBounds`). Sized for a render
/// callback that asks for up to two periods; see `make_rings_for_buffer` when the device buffer is known.
pub fn make_rings(rate: u32, capture_ms: f32, render_ms: f32, prefill_periods: f32) -> Rings {
    make_rings_for_buffer(rate, capture_ms, render_ms, prefill_periods, 2 * ms_to_frames(render_ms, rate))
}

/// Like `make_rings`, for a render callback that can ask for up to `render_buffer_frames` at once
/// (WASAPI shared mode asks for the whole free buffer, which may exceed two periods).
pub fn make_rings_for_buffer(
    rate: u32,
    capture_ms: f32,
    render_ms: f32,
    prefill_periods: f32,
    render_buffer_frames: usize,
) -> Rings {
    let prefill = (ms_to_frames(render_ms, rate) as f32 * prefill_periods).round() as usize;
    let bounds = RingBounds::new(prefill, ms_to_frames(capture_ms, rate).max(1));
    let capacity = bounds.capacity(render_buffer_frames.max(2 * ms_to_frames(render_ms, rate)));
    let (mut main_producer, main_consumer) = RingBuffer::<f32>::new(capacity);
    let (monitor_producer, monitor_consumer) = RingBuffer::<f32>::new(capacity);
    for _ in 0..bounds.target {
        let _ = main_producer.push(0.0);
    }
    Rings { main_producer, main_consumer, monitor_producer, monitor_consumer, bounds }
}

pub struct StatusParams {
    pub shared: Arc<Shared>,
    pub links: SessionLinks,
    pub rate: u32,
    pub capture_ms: f32,
    pub render_ms: f32,
}

pub fn spawn_status(params: StatusParams) -> Option<JoinHandle<()>> {
    thread::Builder::new().name("hushcore-status".into()).spawn(move || status_loop(params)).ok()
}

/// Emits `stopped` exactly once per session, from whichever path gets there first.
#[derive(Clone, Default)]
pub struct StoppedOnce(Arc<AtomicBool>);

impl StoppedOnce {
    pub fn emit(&self, events: &Sender<Event>) {
        if !self.0.swap(true, AcqRel) {
            let _ = events.send(Event::Stopped);
        }
    }

    /// Marks the session as already announced, so teardown stays silent (failed start, restart).
    pub fn suppress(&self) {
        self.0.store(true, Release);
    }
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

        let failed = p.shared.failed.swap(FAIL_NONE, Relaxed);
        if failed & FAIL_MONITOR != 0 {
            let _ = p.links.events.send(Event::Error { message: "monitor output device lost".into(), fatal: false });
        }
        if failed & (FAIL_CAPTURE | FAIL_RENDER) != 0 {
            let which = match (failed & FAIL_CAPTURE != 0, failed & FAIL_RENDER != 0) {
                (true, true) => "input and output devices",
                (true, false) => "input device",
                _ => "output device",
            };
            // The streams are unusable; stop them and let the supervisor reopen the devices.
            p.shared.stop.store(true, Release);
            let _ = p.links.control.send(Inbound::DeviceLost {
                session: p.links.session_id,
                reason: format!("{which} lost"),
            });
            break;
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
        let latency_ms =
            p.capture_ms + p.render_ms + frames_to_ms(fill_frames, p.rate) + frames_to_ms(chain_samples, p.rate);

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
        if p.links.events.send(Event::Status(status)).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn rings_are_prefilled_to_target() {
        let rings = make_rings(48_000, 10.0, 10.0, 0.5);
        assert_eq!(rings.bounds.target, 240);
        assert_eq!(rings.main_consumer.slots(), 240);
        assert_eq!(rings.monitor_consumer.slots(), 0);
    }

    #[test]
    fn device_failure_goes_to_the_supervisor_not_as_stopped() {
        let (etx, erx) = channel();
        let (ctx, crx) = channel();
        let shared = Arc::new(Shared::new(false));
        shared.fail(FAIL_RENDER);
        let handle = spawn_status(StatusParams {
            shared: shared.clone(),
            links: SessionLinks { events: etx, control: ctx, session_id: 7 },
            rate: 48_000,
            capture_ms: 10.0,
            render_ms: 10.0,
        })
        .unwrap();
        handle.join().unwrap();
        assert!(shared.stop.load(Relaxed));
        match crx.try_recv() {
            Ok(Inbound::DeviceLost { session, reason }) => {
                assert_eq!(session, 7);
                assert_eq!(reason, "output device lost");
            }
            other => panic!("expected DeviceLost, got {other:?}"),
        }
        assert!(erx.try_iter().all(|e| !matches!(e, Event::Stopped)));
    }

    #[test]
    fn monitor_failure_does_not_hide_a_main_failure() {
        let (etx, erx) = channel();
        let (ctx, crx) = channel();
        let shared = Arc::new(Shared::new(false));
        shared.fail(FAIL_MONITOR);
        shared.fail(FAIL_CAPTURE);
        let handle = spawn_status(StatusParams {
            shared,
            links: SessionLinks { events: etx, control: ctx, session_id: 3 },
            rate: 48_000,
            capture_ms: 10.0,
            render_ms: 10.0,
        })
        .unwrap();
        handle.join().unwrap();
        assert!(matches!(crx.try_recv(), Ok(Inbound::DeviceLost { reason, .. }) if reason == "input device lost"));
        assert!(erx.try_iter().any(|e| matches!(e, Event::Error { message, .. } if message == "monitor output device lost")));
    }

    #[test]
    fn stopped_is_emitted_once() {
        let (tx, rx) = channel();
        let once = StoppedOnce::default();
        once.emit(&tx);
        once.emit(&tx);
        assert_eq!(rx.try_iter().count(), 1);
        let silent = StoppedOnce::default();
        silent.suppress();
        silent.emit(&tx);
        assert_eq!(rx.try_iter().count(), 0);
    }
}
