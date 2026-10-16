//! Device discovery through cpal's default host: the endpoint lists `devices` reports, name resolution for
//! `start` and `monitor`, and the lines `probe` prints.

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, Host, SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange};

use super::format;
use crate::audio::core::{names, DeviceSummary};

/// The input (capture) or output (render) side of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Input,
    Output,
}

impl Flow {
    fn label(self) -> &'static str {
        match self {
            Flow::Input => "input",
            Flow::Output => "output",
        }
    }
}

/// An active device with the name the UI uses for it.
pub struct Endpoint {
    pub device: Device,
    pub name: String,
    pub is_default: bool,
}

/// The default cpal host. On Core Audio it holds no state, so creating one per call is cheap.
pub fn host() -> Host {
    cpal::default_host()
}

/// Display name. `Device`'s `Display` appends manufacturer and type, which would break name matching, so the
/// bare name comes from the description.
fn device_name(device: &Device) -> Result<String> {
    Ok(DeviceTrait::description(device)?.name().to_string())
}

/// Every device that supports `flow`, in the order Core Audio lists them. Devices whose name cannot be read
/// (disconnected while listing) are skipped.
pub fn endpoints(host: &Host, flow: Flow) -> Result<Vec<Endpoint>> {
    let default = match flow {
        Flow::Input => host.default_input_device(),
        Flow::Output => host.default_output_device(),
    };
    let devices: Vec<Device> = match flow {
        Flow::Input => host.input_devices()?.collect(),
        Flow::Output => host.output_devices()?.collect(),
    };
    let mut out = Vec::with_capacity(devices.len());
    for device in devices {
        let Ok(name) = device_name(&device) else {
            continue;
        };
        let is_default = default.as_ref() == Some(&device);
        out.push(Endpoint { device, name, is_default });
    }
    Ok(out)
}

/// Resolves the device a request names. An empty request means the default: for output the HushCore Sink
/// when `prefer_virtual` is set; for input never the HushCore microphone, which would feed the engine's own
/// output back into its input (PROTOCOL.md).
pub fn resolve(host: &Host, flow: Flow, requested: &str, prefer_virtual: bool) -> Result<Endpoint> {
    let mut endpoints = endpoints(host, flow)?;
    let names: Vec<String> = endpoints.iter().map(|e| e.name.clone()).collect();
    let wanted = names::strip_host_suffix(requested);

    let index = if wanted.is_empty() {
        let default_index = endpoints.iter().position(|e| e.is_default);
        let found = match flow {
            Flow::Input => names::default_input_index(&names, default_index),
            Flow::Output if prefer_virtual => names::default_output_index(&names, default_index),
            Flow::Output => default_index,
        };
        found.ok_or_else(|| anyhow!("no default {} device", flow.label()))?
    } else {
        let index = names::match_device(wanted, &names)
            .ok_or_else(|| anyhow!("no {} device matches \"{wanted}\"", flow.label()))?;
        if flow == Flow::Input && names::is_hushcore_microphone(&names[index]) {
            bail!("HushCore Microphone is HushCore's own output; choose a physical microphone");
        }
        index
    };
    Ok(endpoints.swap_remove(index))
}

/// The device's default configuration for `flow`: its native rate, channel count and buffer range.
pub fn default_config(device: &Device, flow: Flow) -> Result<SupportedStreamConfig> {
    let config = match flow {
        Flow::Input => DeviceTrait::default_input_config(device),
        Flow::Output => DeviceTrait::default_output_config(device),
    }?;
    Ok(config)
}

/// Every configuration range the device accepts for `flow`.
pub fn supported_configs(device: &Device, flow: Flow) -> Result<Vec<SupportedStreamConfigRange>> {
    Ok(match flow {
        Flow::Input => DeviceTrait::supported_input_configs(device)?.collect(),
        Flow::Output => DeviceTrait::supported_output_configs(device)?.collect(),
    })
}

fn buffer_min_period_ms(buffer: &SupportedBufferSize, rate: u32) -> f32 {
    if let SupportedBufferSize::Range { min, .. } = buffer {
        format::period_ms(*min, rate)
    } else {
        0.0
    }
}

fn describe_buffer(buffer: &SupportedBufferSize) -> String {
    if let SupportedBufferSize::Range { min, max } = buffer {
        format!("{min}..={max} frames")
    } else {
        "unknown".to_string()
    }
}

/// One entry per device for the `devices` event.
pub fn summaries(host: &Host, flow: Flow) -> Result<Vec<DeviceSummary>> {
    let mut out = Vec::new();
    for endpoint in endpoints(host, flow)? {
        let (sample_rate, min_period_ms) = match default_config(&endpoint.device, flow) {
            Ok(config) => (config.sample_rate(), buffer_min_period_ms(config.buffer_size(), config.sample_rate())),
            Err(_) => (0, 0.0),
        };
        out.push(DeviceSummary { name: endpoint.name, is_default: endpoint.is_default, min_period_ms, sample_rate });
    }
    Ok(out)
}

/// One line per device: name, default rate, channels and buffer-size range. `*` marks the default device.
pub fn probe_lines(host: &Host) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for flow in [Flow::Input, Flow::Output] {
        for endpoint in endpoints(host, flow)? {
            let marker = if endpoint.is_default { "*" } else { " " };
            lines.push(match default_config(&endpoint.device, flow) {
                Ok(config) => format!(
                    "{:<6} {marker} {:<48} {:>6} Hz  {} ch  buffer {}",
                    flow.label(),
                    endpoint.name,
                    config.sample_rate(),
                    config.channels(),
                    describe_buffer(config.buffer_size()),
                ),
                Err(e) => format!("{:<6} {marker} {:<48} probe failed: {e}", flow.label(), endpoint.name),
            });
        }
    }
    Ok(lines)
}
