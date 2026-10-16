//! JSON-lines control protocol (engine/PROTOCOL.md). One object per line; commands on stdin, events on stdout.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};

use crate::audio::{DeviceSummary, StreamMode};
use crate::settings::Settings;

/// Parameters of `start`. Missing fields take the protocol defaults.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct StartParams {
    pub input: String,
    pub output: String,
    pub monitor: String,
    pub monitor_enabled: bool,
    pub mode: String,
    pub settings: Option<Settings>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Devices,
    Start(StartParams),
    Settings { settings: Settings },
    Monitor {
        enabled: bool,
        #[serde(default)]
        device: String,
    },
    Stop,
    Quit,
}

/// What the stdin reader delivers to the control loop.
#[derive(Debug)]
pub enum Inbound {
    Line(String),
    Eof,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub default: bool,
    pub min_period_ms: f32,
    pub sample_rate: u32,
}

impl From<DeviceSummary> for DeviceInfo {
    fn from(d: DeviceSummary) -> Self {
        Self { name: d.name, default: d.is_default, min_period_ms: d.min_period_ms, sample_rate: d.sample_rate }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub input_rms: f32,
    pub output_rms: f32,
    pub gate_state: bool,
    pub fft_data: Vec<f32>,
    pub clipping: bool,
    pub cpu_load: f32,
    pub latency_ms: f32,
    pub sample_rate: u32,
    pub xruns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Ready {
        version: &'static str,
    },
    Devices {
        inputs: Vec<DeviceInfo>,
        outputs: Vec<DeviceInfo>,
    },
    Started {
        sample_rate: u32,
        mode: &'static str,
        input_period_ms: f32,
        output_period_ms: f32,
        latency_ms: f32,
    },
    Stopped,
    Status(Status),
    Error {
        message: String,
        fatal: bool,
    },
}

pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parses one protocol line into a command. Errors are human-readable and go back as `error` events.
pub fn parse_command(line: &str) -> Result<Command, String> {
    serde_json::from_str::<Command>(line.trim()).map_err(|e| format!("bad command: {e}"))
}

/// Maps the `mode` field to a stream mode. Empty means shared.
pub fn parse_mode(mode: &str) -> Result<StreamMode, String> {
    match mode.trim() {
        "" | "shared" => Ok(StreamMode::Shared),
        "exclusive" => Ok(StreamMode::Exclusive),
        other => Err(format!("unknown mode \"{other}\" (expected \"shared\" or \"exclusive\")")),
    }
}

pub fn encode(event: &Event) -> String {
    match serde_json::to_string(event) {
        Ok(line) => line,
        Err(e) => format!(r#"{{"event":"error","message":"could not encode event: {e}","fatal":false}}"#),
    }
}

/// Writes one event per line and flushes after each, so the backend sees events immediately.
pub fn spawn_writer(events: Receiver<Event>) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("hushcore-ipc-out".into()).spawn(move || {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for event in events {
            let line = encode(&event);
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                break;
            }
        }
    })
}

/// Reads stdin on its own thread. Sends `Eof` when the backend closes the pipe.
pub fn spawn_reader(inbound: Sender<Inbound>) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("hushcore-ipc-in".into()).spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if inbound.send(Inbound::Line(line)).is_err() {
                return;
            }
        }
        let _ = inbound.send(Inbound::Eof);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command_shape() {
        assert_eq!(parse_command(r#"{"cmd":"devices"}"#), Ok(Command::Devices));
        assert_eq!(parse_command(r#"{"cmd":"stop"}"#), Ok(Command::Stop));
        assert_eq!(parse_command(r#"{"cmd":"quit"}"#), Ok(Command::Quit));
        assert_eq!(
            parse_command(r#"{"cmd":"monitor","enabled":true,"device":"Headphones"}"#),
            Ok(Command::Monitor { enabled: true, device: "Headphones".into() })
        );
        match parse_command(r#"{"cmd":"settings","settings":{"output_gain":0.0,"gate_enabled":false}}"#) {
            Ok(Command::Settings { settings }) => {
                assert_eq!(settings.output_gain, 0.0);
                assert!(!settings.gate_enabled);
                assert_eq!(settings.ns_mode, "eco");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn start_applies_defaults_and_ignores_unknown_keys() {
        let line = r#"{"cmd":"start","input":"Mic","mode":"exclusive","settings":{"input_gain":2.0},"extra":1}"#;
        match parse_command(line) {
            Ok(Command::Start(p)) => {
                assert_eq!(p.input, "Mic");
                assert_eq!(p.output, "");
                assert!(!p.monitor_enabled);
                assert_eq!(parse_mode(&p.mode), Ok(StreamMode::Exclusive));
                assert_eq!(p.settings.expect("settings").input_gain, 2.0);
            }
            other => panic!("unexpected {other:?}"),
        }
        match parse_command(r#"{"cmd":"start"}"#) {
            Ok(Command::Start(p)) => {
                assert_eq!(parse_mode(&p.mode), Ok(StreamMode::Shared));
                assert!(p.settings.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_commands_and_modes() {
        assert!(parse_command(r#"{"cmd":"explode"}"#).is_err());
        assert!(parse_command("not json").is_err());
        assert!(parse_mode("turbo").is_err());
    }

    #[test]
    fn events_serialise_with_protocol_field_names() {
        let started = encode(&Event::Started {
            sample_rate: 48_000,
            mode: "shared-low-latency",
            input_period_ms: 10.0,
            output_period_ms: 10.0,
            latency_ms: 30.0,
        });
        let v: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(v["event"], "started");
        assert_eq!(v["mode"], "shared-low-latency");
        assert_eq!(v["sample_rate"], 48_000);

        let status = encode(&Event::Status(Status {
            input_rms: -30.0,
            output_rms: -31.5,
            gate_state: true,
            fft_data: vec![0.5; 64],
            clipping: false,
            cpu_load: 4.2,
            latency_ms: 25.0,
            sample_rate: 48_000,
            xruns: 0,
        }));
        let v: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(v["event"], "status");
        assert_eq!(v["gate_state"], true);
        assert_eq!(v["fft_data"].as_array().unwrap().len(), 64);
        assert!(!status.contains('\n'));

        assert_eq!(encode(&Event::Stopped), r#"{"event":"stopped"}"#);
        let error: serde_json::Value =
            serde_json::from_str(&encode(&Event::Error { message: "x".into(), fatal: false })).unwrap();
        assert_eq!(error["fatal"], false);
    }

    #[test]
    fn devices_event_lists_both_flows() {
        let event = Event::Devices {
            inputs: vec![DeviceInfo { name: "Mic".into(), default: true, min_period_ms: 10.0, sample_rate: 48_000 }],
            outputs: vec![],
        };
        let v: serde_json::Value = serde_json::from_str(&encode(&event)).unwrap();
        assert_eq!(v["inputs"][0]["default"], true);
        assert_eq!(v["outputs"].as_array().unwrap().len(), 0);
    }
}
