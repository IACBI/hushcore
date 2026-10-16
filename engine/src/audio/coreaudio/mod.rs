//! Core Audio backend (macOS): endpoint discovery through cpal, the input and output streams, and the
//! monitor. The HushCore virtual device is the AudioServerPlugIn installed by scripts/macos/install-plugin.sh.
//!
//! Real-time priority: cpal's Core Audio streams run their callbacks on the HAL's IO threads, which are
//! already real-time, so no thread priority is requested (unlike WASAPI's MMCSS).

mod devices;
mod format;
mod session;

use anyhow::Result;

use crate::audio::core::{DeviceList, VirtualDeviceStatus};

pub use session::Session;

/// Core Audio needs no per-thread setup.
pub struct PlatformInit;

impl PlatformInit {
    /// Nothing to initialise: Core Audio has no per-thread state that cpal requires.
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

/// Every input and output device the default host reports.
pub fn list_devices() -> Result<DeviceList> {
    let host = devices::host();
    Ok(DeviceList {
        inputs: devices::summaries(&host, devices::Flow::Input)?,
        outputs: devices::summaries(&host, devices::Flow::Output)?,
    })
}

/// The HushCore AudioServerPlugIn exposes both endpoints; anything else counts as not installed.
pub fn virtual_device(devices: &DeviceList) -> VirtualDeviceStatus {
    let mut status = VirtualDeviceStatus::from_devices(devices, "macos-plugin");
    if !status.installed {
        status.detail.push_str(". Install it with scripts/macos/install-plugin.sh");
    }
    status
}

/// One line per device: name, default rate, channels and buffer-size range.
pub fn probe() -> Result<Vec<String>> {
    devices::probe_lines(&devices::host())
}
