//! State shared by a session's threads, the start request, and the device descriptions every
//! platform backend reports. Nothing here touches an OS audio API.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::{Relaxed, Release}};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use serde::Serialize;

use crate::dsp::Metrics;
use crate::settings::Settings;

/// Failure bits in `Shared::failed`. They accumulate, so a monitor failure never hides a main one.
pub const FAIL_NONE: u8 = 0;
pub const FAIL_CAPTURE: u8 = 1;
pub const FAIL_RENDER: u8 = 2;
pub const FAIL_MONITOR: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    Shared,
    /// WASAPI exclusive mode. Other backends run shared and report it in `started.mode`.
    Exclusive,
}

/// Live settings. `generation` moves after every store so the capture thread can check cheaply.
pub struct Controller {
    pub settings: ArcSwap<Settings>,
    pub generation: AtomicU64,
}

impl Default for Controller {
    fn default() -> Self {
        Self { settings: ArcSwap::from_pointee(Settings::default()), generation: AtomicU64::new(1) }
    }
}

impl Controller {
    pub fn set_settings(&self, settings: Settings) {
        self.settings.store(Arc::new(settings));
        self.generation.fetch_add(1, Release);
    }
}

/// State shared by the session's threads. Audio threads touch only atomics and the try_lock mailbox.
pub struct Shared {
    pub stop: Arc<AtomicBool>,
    pub failed: AtomicU8,
    pub monitor_active: AtomicBool,
    pub mute_output: bool,
    pub mailbox: Mutex<Metrics>,
    pub proc_ns: AtomicU64,
    pub proc_frames: AtomicU64,
    pub render_fill_bits: AtomicU32,
    pub chain_latency_samples: AtomicU32,
    pub counts: Counts,
}

/// Event counters. Each is bumped from one audio thread with a relaxed atomic add.
#[derive(Default)]
pub struct Counts {
    /// Capture packets the OS flagged as discontinuous (lost input).
    pub discontinuities: AtomicU64,
    /// Capture-side pushes that found the ring full (newest samples dropped).
    pub capture_drops: AtomicU64,
    /// Render callbacks that dropped the oldest queued samples to bound latency.
    pub render_discards: AtomicU64,
    /// Drift corrections (one sample skipped or held); deliberate, so not xruns.
    pub slew_drops: AtomicU64,
    pub slew_repeats: AtomicU64,
    pub discarded_frames: AtomicU64,
    /// Render callbacks that found the ring short and padded with silence.
    pub underruns: AtomicU64,
    pub rendered_frames: AtomicU64,
    pub silence_frames: AtomicU64,
    pub capture_packets: AtomicU64,
    pub render_callbacks: AtomicU64,
}

/// Plain snapshot of `Counts` plus the frames the capture thread produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub produced_frames: u64,
    pub rendered_frames: u64,
    pub silence_frames: u64,
    pub discarded_frames: u64,
    pub underruns: u64,
    pub discontinuities: u64,
    pub capture_drops: u64,
    pub render_discards: u64,
    pub slew_drops: u64,
    pub slew_repeats: u64,
    pub capture_packets: u64,
    pub render_callbacks: u64,
}

impl Diagnostics {
    /// The protocol's `xruns`: every glitch source added together.
    pub fn xruns(&self) -> u64 {
        self.underruns + self.discontinuities + self.capture_drops + self.render_discards
    }
}

impl Shared {
    pub fn new(mute_output: bool) -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            failed: AtomicU8::new(FAIL_NONE),
            monitor_active: AtomicBool::new(false),
            mute_output,
            mailbox: Mutex::new(Metrics::default()),
            proc_ns: AtomicU64::new(0),
            proc_frames: AtomicU64::new(0),
            render_fill_bits: AtomicU32::new(0f32.to_bits()),
            chain_latency_samples: AtomicU32::new(0),
            counts: Counts::default(),
        }
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let c = &self.counts;
        Diagnostics {
            produced_frames: self.proc_frames.load(Relaxed),
            rendered_frames: c.rendered_frames.load(Relaxed),
            silence_frames: c.silence_frames.load(Relaxed),
            discarded_frames: c.discarded_frames.load(Relaxed),
            underruns: c.underruns.load(Relaxed),
            discontinuities: c.discontinuities.load(Relaxed),
            capture_drops: c.capture_drops.load(Relaxed),
            render_discards: c.render_discards.load(Relaxed),
            slew_drops: c.slew_drops.load(Relaxed),
            slew_repeats: c.slew_repeats.load(Relaxed),
            capture_packets: c.capture_packets.load(Relaxed),
            render_callbacks: c.render_callbacks.load(Relaxed),
        }
    }

    /// Records a device failure (one of the `FAIL_*` bits) for the status thread.
    pub fn fail(&self, which: u8) {
        self.failed.fetch_or(which, Relaxed);
    }
}

#[derive(Clone, Debug)]
pub struct StartRequest {
    pub input: String,
    pub output: String,
    pub monitor: String,
    pub monitor_enabled: bool,
    pub mode: StreamMode,
    /// Diagnostics only: render silence no matter what the chain produces.
    pub mute_output: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSummary {
    pub name: String,
    pub is_default: bool,
    pub min_period_ms: f32,
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceList {
    pub inputs: Vec<DeviceSummary>,
    pub outputs: Vec<DeviceSummary>,
}

/// Whether HushCore's own virtual device is usable, as reported in the `devices` event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VirtualDeviceStatus {
    /// True when both the sink and the microphone are present.
    pub installed: bool,
    /// Which component provides it: `windows-driver`, `macos-plugin`, `pipewire`, `pulseaudio`,
    /// `on-demand` (Linux, created by the engine at start) or `none`.
    pub provider: &'static str,
    /// Exact endpoint names as the OS reports them, when present.
    pub sink: Option<String>,
    pub microphone: Option<String>,
    /// One human-readable line: what is missing, or how it was found.
    pub detail: String,
}

impl VirtualDeviceStatus {
    /// Builds a status from the endpoint lists using the shared naming rules.
    pub fn from_devices(devices: &DeviceList, provider: &'static str) -> Self {
        use super::names::{is_hushcore_microphone, is_hushcore_sink};
        let sink = devices.outputs.iter().find(|d| is_hushcore_sink(&d.name)).map(|d| d.name.clone());
        let microphone = devices.inputs.iter().find(|d| is_hushcore_microphone(&d.name)).map(|d| d.name.clone());
        let installed = sink.is_some() && microphone.is_some();
        let detail = match (&sink, &microphone) {
            (Some(_), Some(_)) => "HushCore virtual device is ready".to_string(),
            (None, None) => "HushCore virtual device is not installed".to_string(),
            (None, Some(_)) => "HushCore Sink is missing; reinstall the virtual device".to_string(),
            (Some(_), None) => "HushCore Microphone is missing; reinstall the virtual device".to_string(),
        };
        Self { installed, provider: if installed { provider } else { "none" }, sink, microphone, detail }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str) -> DeviceSummary {
        DeviceSummary { name: name.into(), is_default: false, min_period_ms: 0.0, sample_rate: 48_000 }
    }

    #[test]
    fn virtual_status_needs_both_endpoints() {
        let mut list = DeviceList { inputs: vec![dev("Mic")], outputs: vec![dev("Speakers")] };
        let none = VirtualDeviceStatus::from_devices(&list, "windows-driver");
        assert!(!none.installed);
        assert_eq!(none.provider, "none");

        list.outputs.push(dev("HushCore Sink (HushCore Virtual Audio)"));
        assert!(!VirtualDeviceStatus::from_devices(&list, "windows-driver").installed);

        list.inputs.push(dev("HushCore Microphone (HushCore Virtual Audio)"));
        let ok = VirtualDeviceStatus::from_devices(&list, "windows-driver");
        assert!(ok.installed);
        assert_eq!(ok.provider, "windows-driver");
        assert_eq!(ok.sink.as_deref(), Some("HushCore Sink (HushCore Virtual Audio)"));
    }

    #[test]
    fn failures_accumulate() {
        let shared = Shared::new(false);
        shared.fail(FAIL_MONITOR);
        shared.fail(FAIL_RENDER);
        assert_eq!(shared.failed.load(Relaxed), FAIL_MONITOR | FAIL_RENDER);
    }
}
