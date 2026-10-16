//! hushcore-engine: native low-latency audio engine (WASAPI on Windows, Core Audio on macOS,
//! PulseAudio/PipeWire on Linux).
//!
//! Without arguments (or `serve`) it speaks the JSON-lines protocol on stdin/stdout (see PROTOCOL.md).
//! Diagnostics: `devices`, `probe`, `virtual-device`, and `selftest <seconds> [shared|exclusive]`
//! (output is always muted).

use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use hushcore_engine::audio::{self, Controller, DeviceList, PlatformInit, Session, SessionLinks, StartRequest};
use hushcore_engine::ipc::{self, DeviceInfo, Event, Inbound};
use hushcore_engine::settings::Settings;
use hushcore_engine::supervisor::{Backend, Supervisor};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => serve(),
        Some("devices") => devices_cmd(),
        Some("probe") => probe_cmd(),
        Some("virtual-device") => virtual_device_cmd(),
        Some("selftest") => selftest_cmd(&args[1..]),
        Some(other) => Err(anyhow!(
            "unknown subcommand \"{other}\" (expected serve, devices, probe, virtual-device or selftest <seconds> [shared|exclusive])"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hushcore-engine: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The compiled-in platform backend behind the supervisor.
struct Platform;

impl Backend for Platform {
    type Session = Session;

    fn start(&mut self, req: &StartRequest, controller: &Arc<Controller>, links: &SessionLinks) -> Result<Session> {
        Session::start(req, controller, links)
    }

    fn set_monitor(&mut self, session: &mut Session, enabled: bool, device: &str) -> Result<()> {
        session.set_monitor(enabled, device)
    }

    fn stop(&mut self, session: Session) {
        session.stop();
    }

    fn abandon(&mut self, session: Session) {
        session.abandon();
    }

    fn devices_event(&mut self) -> Result<Event> {
        devices_event()
    }
}

fn serve() -> Result<()> {
    let _init = PlatformInit::new()?;
    let (out_tx, out_rx) = mpsc::channel::<Event>();
    let writer = ipc::spawn_writer(out_rx).context("cannot start stdout writer")?;
    let (in_tx, in_rx) = mpsc::channel::<Inbound>();
    ipc::spawn_reader(in_tx.clone()).context("cannot start stdin reader")?;

    let mut supervisor = Supervisor::new(Platform, Arc::new(Controller::default()), out_tx.clone(), in_tx);
    let _ = out_tx.send(Event::Ready { version: ipc::ENGINE_VERSION });

    loop {
        let message = match supervisor.deadline() {
            Some(due) => match in_rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match in_rx.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };
        match message {
            None => supervisor.tick(Instant::now()),
            Some(Inbound::Eof) => break,
            Some(Inbound::TooLong) => {
                let message = format!("command line longer than {} bytes ignored", ipc::MAX_LINE_BYTES);
                let _ = out_tx.send(Event::Error { message, fatal: false });
            }
            Some(Inbound::DeviceLost { session, reason }) => supervisor.device_lost(session, reason, Instant::now()),
            Some(Inbound::Line(line)) => match ipc::parse_command(&line) {
                Ok(ipc::Command::Quit) => break,
                Ok(command) => supervisor.handle(command),
                Err(message) => {
                    let _ = out_tx.send(Event::Error { message, fatal: false });
                }
            },
        }
    }

    supervisor.shutdown();
    drop(supervisor);
    drop(out_tx);
    let _ = writer.join();
    Ok(())
}

fn devices_event() -> Result<Event> {
    let list = audio::list_devices()?;
    let virtual_device = audio::virtual_device(&list);
    let DeviceList { inputs, outputs } = list;
    Ok(Event::Devices {
        inputs: inputs.into_iter().map(DeviceInfo::from).collect(),
        outputs: outputs.into_iter().map(DeviceInfo::from).collect(),
        virtual_device,
    })
}

fn devices_cmd() -> Result<()> {
    let _init = PlatformInit::new()?;
    println!("{}", ipc::encode(&devices_event()?));
    Ok(())
}

fn probe_cmd() -> Result<()> {
    let _init = PlatformInit::new()?;
    for line in audio::probe()? {
        println!("{line}");
    }
    Ok(())
}

/// Prints the virtual-device status as one JSON object; exits non-zero when it is not installed,
/// so install scripts and CI can use it as a check.
fn virtual_device_cmd() -> Result<()> {
    let _init = PlatformInit::new()?;
    let status = audio::virtual_device(&audio::list_devices()?);
    println!("{}", serde_json::to_string(&status)?);
    if status.installed {
        Ok(())
    } else {
        Err(anyhow!("{}", status.detail))
    }
}

/// Collects events from a selftest run for the summary line.
#[derive(Default)]
struct Tally {
    started: Option<(u32, &'static str, f32, f32, f32)>,
    cpu: Vec<f32>,
    latency: Vec<f32>,
    errors: Vec<String>,
}

impl Tally {
    fn absorb(&mut self, event: Event) {
        match event {
            Event::Started { sample_rate, mode, input_period_ms, output_period_ms, latency_ms } => {
                self.started = Some((sample_rate, mode, input_period_ms, output_period_ms, latency_ms));
            }
            Event::Status(status) => {
                self.cpu.push(status.cpu_load);
                self.latency.push(status.latency_ms);
            }
            Event::Error { message, .. } => self.errors.push(message),
            _ => {}
        }
    }
}

fn selftest_cmd(args: &[String]) -> Result<()> {
    let seconds: f64 = args
        .first()
        .ok_or_else(|| anyhow!("usage: selftest <seconds> [shared|exclusive] [output device]"))?
        .parse()
        .context("seconds must be a number")?;
    let mode = ipc::parse_mode(args.get(1).map_or("", String::as_str)).map_err(|e| anyhow!(e))?;
    let output = args.get(2).cloned().unwrap_or_default();

    let _init = PlatformInit::new()?;
    let controller = Arc::new(Controller::default());
    // Output is muted twice: gain 0 in the settings, and the render thread writes silence (mute_output).
    controller.set_settings(Settings { output_gain: 0.0, ..Settings::default() });
    let (tx, rx) = mpsc::channel::<Event>();
    let (control_tx, control_rx) = mpsc::channel::<Inbound>();
    let links = SessionLinks { events: tx, control: control_tx, session_id: 1 };
    let request = StartRequest {
        input: String::new(),
        output,
        monitor: String::new(),
        monitor_enabled: false,
        mode,
        mute_output: true,
    };

    let mut tally = Tally::default();
    let session = Session::start(&request, &controller, &links).context("selftest could not start")?;
    for event in rx.try_iter() {
        tally.absorb(event);
    }

    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(event) => tally.absorb(event),
            Err(_) => break,
        }
    }
    let diag = session.diagnostics();
    session.stop();
    for event in rx.try_iter() {
        tally.absorb(event);
    }
    for message in control_rx.try_iter() {
        if let Inbound::DeviceLost { reason, .. } = message {
            tally.errors.push(reason);
        }
    }

    let (sample_rate, mode_name, input_ms, output_ms, started_latency) =
        tally.started.ok_or_else(|| anyhow!("engine never reported started"))?;
    let avg = |v: &[f32]| if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
    let cpu_max = tally.cpu.iter().copied().fold(0.0f32, f32::max);
    println!(
        "selftest: mode={mode_name} sample_rate={sample_rate} input_period_ms={input_ms:.2} output_period_ms={output_ms:.2} \
         latency_ms={started_latency:.1} latency_ms_avg={:.1} cpu_load_avg={:.1} cpu_load_max={cpu_max:.1} \
         xruns={} seconds={seconds}",
        avg(&tally.latency),
        avg(&tally.cpu),
        diag.xruns(),
    );
    println!(
        "selftest: underruns={} discontinuities={} capture_drops={} render_discards={} slew_drops={} slew_repeats={} \
         produced_frames={} rendered_frames={} silence_frames={} discarded_frames={} status_events={} \
         capture_packets={} render_callbacks={}",
        diag.underruns,
        diag.discontinuities,
        diag.capture_drops,
        diag.render_discards,
        diag.slew_drops,
        diag.slew_repeats,
        diag.produced_frames,
        diag.rendered_frames,
        diag.silence_frames,
        diag.discarded_frames,
        tally.cpu.len(),
        diag.capture_packets,
        diag.render_callbacks,
    );
    for message in &tally.errors {
        println!("selftest error event: {message}");
    }
    Ok(())
}
