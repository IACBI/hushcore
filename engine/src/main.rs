//! hushcore-engine: native WASAPI audio engine.
//!
//! Without arguments (or `serve`) it speaks the JSON-lines protocol on stdin/stdout (see PROTOCOL.md).
//! Diagnostics: `devices`, `probe`, and `selftest <seconds> [shared|exclusive]` (output is always muted).

use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use windows::Win32::Media::Audio::{eCapture, eRender};

use hushcore_engine::audio::{self, ComInit, Controller, Enumerator, Session, StartRequest};
use hushcore_engine::ipc::{self, Command, DeviceInfo, Event, Inbound, StartParams};
use hushcore_engine::settings::Settings;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => serve(),
        Some("devices") => devices_cmd(),
        Some("probe") => probe_cmd(),
        Some("selftest") => selftest_cmd(&args[1..]),
        Some(other) => Err(anyhow!(
            "unknown subcommand \"{other}\" (expected serve, devices, probe or selftest <seconds> [shared|exclusive])"
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

fn serve() -> Result<()> {
    let _com = ComInit::new()?;
    let (out_tx, out_rx) = mpsc::channel::<Event>();
    let writer = ipc::spawn_writer(out_rx).context("cannot start stdout writer")?;
    let (in_tx, in_rx) = mpsc::channel::<Inbound>();
    ipc::spawn_reader(in_tx).context("cannot start stdin reader")?;

    let mut server = Server { controller: Arc::new(Controller::default()), session: None, events: out_tx.clone() };
    server.emit(Event::Ready { version: ipc::ENGINE_VERSION });

    while let Ok(message) = in_rx.recv() {
        match message {
            Inbound::Eof => break,
            Inbound::Line(line) => match ipc::parse_command(&line) {
                Ok(Command::Quit) => break,
                Ok(command) => server.handle(command),
                Err(message) => server.error(message),
            },
        }
    }

    if let Some(session) = server.session.take() {
        session.stop();
    }
    drop(server);
    drop(out_tx);
    let _ = writer.join();
    Ok(())
}

/// Owns the live session. Runs only on the control thread; audio threads never touch it.
struct Server {
    controller: Arc<Controller>,
    session: Option<Session>,
    events: Sender<Event>,
}

impl Server {
    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn error(&self, message: impl Into<String>) {
        self.emit(Event::Error { message: message.into(), fatal: false });
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Devices => match devices_event() {
                Ok(event) => self.emit(event),
                Err(e) => self.error(format!("device enumeration failed: {e:#}")),
            },
            Command::Start(params) => self.start(params),
            Command::Settings { settings } => self.controller.set_settings(settings),
            Command::Monitor { enabled, device } => {
                let result = match self.session.as_mut() {
                    Some(session) => session.set_monitor(enabled, &device),
                    // Monitoring is part of `start` while stopped; a stray monitor command is not an error.
                    None => Ok(()),
                };
                match result {
                    Ok(()) => eprintln!("monitor: enabled={enabled} device=\"{device}\""),
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            Command::Stop => self.stop(),
            Command::Quit => {}
        }
    }

    fn start(&mut self, params: StartParams) {
        if let Some(session) = self.session.take() {
            session.stop();
        }
        if let Some(settings) = params.settings {
            self.controller.set_settings(settings);
        }
        let mode = match ipc::parse_mode(&params.mode) {
            Ok(mode) => mode,
            Err(message) => return self.error(message),
        };
        let request = StartRequest {
            input: params.input,
            output: params.output,
            monitor: params.monitor,
            monitor_enabled: params.monitor_enabled,
            mode,
            mute_output: false,
        };
        match Session::start(&request, &self.controller, &self.events) {
            Ok(mut session) => {
                if request.monitor_enabled {
                    if let Err(e) = session.set_monitor(true, &request.monitor) {
                        self.error(format!("{e:#}"));
                    }
                }
                self.session = Some(session);
            }
            Err(e) => self.error(format!("could not start audio: {e:#}")),
        }
    }

    /// Stops the session if one is running. Always leaves the backend with a `stopped` event.
    fn stop(&mut self) {
        match self.session.take() {
            Some(session) => session.stop(),
            None => self.emit(Event::Stopped),
        }
    }
}

fn devices_event() -> Result<Event> {
    let enumerator = Enumerator::new()?;
    let inputs = audio::devices::summaries(&enumerator, eCapture)?.into_iter().map(DeviceInfo::from).collect();
    let outputs = audio::devices::summaries(&enumerator, eRender)?.into_iter().map(DeviceInfo::from).collect();
    Ok(Event::Devices { inputs, outputs })
}

fn devices_cmd() -> Result<()> {
    let _com = ComInit::new()?;
    println!("{}", ipc::encode(&devices_event()?));
    Ok(())
}

fn fmt_ms(value: Option<f32>) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}"))
}

fn probe_cmd() -> Result<()> {
    let _com = ComInit::new()?;
    let enumerator = Enumerator::new()?;
    for (label, flow) in [("capture", eCapture), ("render ", eRender)] {
        for endpoint in enumerator.endpoints(flow)? {
            let marker = if endpoint.is_default { "*" } else { " " };
            match audio::devices::periods(&endpoint.device) {
                Ok(p) => println!(
                    "{label} {marker} {:<52} {:>6} Hz  shared ms: default {} fundamental {} min {} max {}  device ms: default {:.2} min {:.2}",
                    endpoint.name,
                    p.sample_rate,
                    fmt_ms(p.shared_default_ms),
                    fmt_ms(p.shared_fundamental_ms),
                    fmt_ms(p.shared_min_ms),
                    fmt_ms(p.shared_max_ms),
                    p.device_default_ms,
                    p.device_min_ms,
                ),
                Err(e) => println!("{label} {marker} {:<52} probe failed: {e:#}", endpoint.name),
            }
        }
    }
    Ok(())
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
        .ok_or_else(|| anyhow!("usage: selftest <seconds> [shared|exclusive]"))?
        .parse()
        .context("seconds must be a number")?;
    let mode = ipc::parse_mode(args.get(1).map_or("", String::as_str)).map_err(|e| anyhow!(e))?;

    let _com = ComInit::new()?;
    let controller = Arc::new(Controller::default());
    // Output is muted twice: gain 0 in the settings, and the render thread writes silence (mute_output).
    controller.set_settings(Settings { output_gain: 0.0, ..Settings::default() });
    let (tx, rx) = mpsc::channel::<Event>();
    let request = StartRequest {
        input: String::new(),
        output: String::new(),
        monitor: String::new(),
        monitor_enabled: false,
        mode,
        mute_output: true,
    };

    let mut tally = Tally::default();
    let session = Session::start(&request, &controller, &tx).context("selftest could not start")?;
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
