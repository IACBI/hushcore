//! WASAPI backend (Windows): endpoint discovery, capture and render threads, and the running session.

pub mod clients;
pub mod com;
pub mod devices;
pub mod format;
pub mod session;

mod capture;
mod render;

use anyhow::Result;
use windows::Win32::Media::Audio::{eCapture, eRender};

pub use com::ComInit as PlatformInit;
pub use devices::{Enumerator, Periods};
pub use session::Session;

use crate::audio::core::{DeviceList, VirtualDeviceStatus};

/// Every active capture and render endpoint. Requires `PlatformInit` on the calling thread.
pub fn list_devices() -> Result<DeviceList> {
    let enumerator = Enumerator::new()?;
    Ok(DeviceList {
        inputs: devices::summaries(&enumerator, eCapture)?,
        outputs: devices::summaries(&enumerator, eRender)?,
    })
}

/// The HushCore WaveRT driver exposes both endpoints; anything else counts as not installed.
pub fn virtual_device(devices: &DeviceList) -> VirtualDeviceStatus {
    let mut status = VirtualDeviceStatus::from_devices(devices, "windows-driver");
    if !status.installed {
        status.detail.push_str(". Install it with scripts\\windows\\install-driver.ps1 (as administrator)");
    }
    status
}

fn fmt_ms(value: Option<f32>) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}"))
}

/// One line per endpoint with the periods the engine can run at.
pub fn probe() -> Result<Vec<String>> {
    let enumerator = Enumerator::new()?;
    let mut lines = Vec::new();
    for (label, flow) in [("capture", eCapture), ("render ", eRender)] {
        for endpoint in enumerator.endpoints(flow)? {
            let marker = if endpoint.is_default { "*" } else { " " };
            lines.push(match devices::periods(&endpoint.device) {
                Ok(p) => format!(
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
                Err(e) => format!("{label} {marker} {:<52} probe failed: {e:#}", endpoint.name),
            });
        }
    }
    Ok(lines)
}
