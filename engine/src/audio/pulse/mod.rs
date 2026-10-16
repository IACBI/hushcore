//! PulseAudio-protocol backend (Linux). PipeWire serves this protocol through `pipewire-pulse`, and
//! PulseAudio serves it natively, so one client covers both. Contract: docs/ARCHITECTURE.md.

mod connection;
mod devices;
mod modules;
mod session;
mod streams;

use anyhow::Result;

use crate::audio::core::{DeviceList, VirtualDeviceStatus};

pub use modules::PlatformInit;
pub use session::Session;

use connection::Server;
use modules::{provider, SINK_PULSE_NAME};

/// Every sink and every non-monitor source. Each call opens its own short connection.
pub fn list_devices() -> Result<DeviceList> {
    let mut server = Server::connect()?;
    let info = server.server_info()?;
    let sinks = server.sinks(&info)?;
    let sources = server.sources(&info)?;
    Ok(DeviceList {
        inputs: devices::input_summaries(&sources),
        outputs: devices::output_summaries(&sinks),
    })
}

/// Whether the HushCore pair is present, and which component provides it.
pub fn virtual_device(devices: &DeviceList) -> VirtualDeviceStatus {
    let (info, sink) = match server_view() {
        Ok(view) => view,
        Err(_) => return VirtualDeviceStatus::from_devices(devices, "none"),
    };
    let provider = provider(&info, sink.as_ref());
    let mut status = VirtualDeviceStatus::from_devices(devices, provider);
    if !status.installed {
        status.detail.push_str(
            ". Install it with scripts/linux/install-virtual-mic.sh, or start the engine with an empty output \
             to create it for that session",
        );
    }
    status
}

/// The server identity and HushCore's sink node, if it exists.
fn server_view() -> Result<(connection::ServerInfo, Option<connection::Node>)> {
    let mut server = Server::connect()?;
    let info = server.server_info()?;
    let sink = server.sinks(&info)?.into_iter().find(|n| n.pulse_name == SINK_PULSE_NAME);
    Ok((info, sink))
}

/// One line per sink and source, with its sample rate and state.
pub fn probe() -> Result<Vec<String>> {
    let mut server = Server::connect()?;
    let info = server.server_info()?;
    let mut lines = vec![format!("server: {}", info.server_name)];
    for node in server.sinks(&info)? {
        let marker = if node.is_default { "*" } else { " " };
        lines.push(format!("output {marker} {:<40} {:>6} Hz  {}", node.description, node.rate, node.pulse_name));
    }
    for node in server.sources(&info)? {
        let marker = if node.is_default { "*" } else { " " };
        let kind = if node.is_monitor { "monitor" } else { "input  " };
        lines.push(format!("{kind} {marker} {:<40} {:>6} Hz  {}", node.description, node.rate, node.pulse_name));
    }
    Ok(lines)
}
