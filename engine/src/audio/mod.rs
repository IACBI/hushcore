//! Audio I/O. `core` is shared; exactly one platform backend is compiled in and re-exported as
//! `platform`. Each backend exports the same surface (docs/ARCHITECTURE.md):
//! `PlatformInit`, `list_devices`, `virtual_device`, `probe`, and `Session`.

pub mod core;

#[cfg(windows)]
pub mod wasapi;
#[cfg(windows)]
pub use wasapi as platform;

#[cfg(target_os = "macos")]
pub mod coreaudio;
#[cfg(target_os = "macos")]
pub use coreaudio as platform;

#[cfg(target_os = "linux")]
pub mod pulse;
#[cfg(target_os = "linux")]
pub use pulse as platform;

pub use self::core::{
    Controller, DeviceList, DeviceSummary, Diagnostics, SessionLinks, StartRequest, StreamMode, VirtualDeviceStatus,
};
pub use platform::{list_devices, probe, virtual_device, PlatformInit, Session};
