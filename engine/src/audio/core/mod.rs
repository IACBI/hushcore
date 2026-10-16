//! Platform-independent engine core. Every backend (WASAPI, Core Audio, PulseAudio) feeds capture
//! through `Processor` and drains render through `RingRenderer`, so the real-time policy is shared.

pub mod names;
pub mod processor;
pub mod renderer;
pub mod ring;
pub mod session;
pub mod state;

pub use processor::Processor;
pub use renderer::RingRenderer;
pub use session::{make_rings, make_rings_for_buffer, ms_to_frames, spawn_status, Rings, SessionLinks, StatusParams, StoppedOnce};
pub use state::{
    Controller, DeviceList, DeviceSummary, Diagnostics, Shared, StartRequest, StreamMode, VirtualDeviceStatus,
    FAIL_CAPTURE, FAIL_MONITOR, FAIL_NONE, FAIL_RENDER,
};
