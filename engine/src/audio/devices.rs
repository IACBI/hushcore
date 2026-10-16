//! Endpoint enumeration, name resolution and the period probes shown in `devices` and `probe`.

use anyhow::{anyhow, Context, Result};
use windows::core::{Interface, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioClient, IAudioClient3, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, STGM_READ};

use super::com::{mix_format, take_pwstr, OwnedFormat};
use super::names;

/// The engine's device enumerator. Requires COM to be initialised on the calling thread.
pub struct Enumerator {
    inner: IMMDeviceEnumerator,
}

pub struct Endpoint {
    pub device: IMMDevice,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSummary {
    pub name: String,
    pub is_default: bool,
    pub min_period_ms: f32,
    pub sample_rate: u32,
}

/// Periods of one endpoint in milliseconds. `shared_*` are None when IAudioClient3 is unavailable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Periods {
    pub sample_rate: u32,
    pub shared_default_ms: Option<f32>,
    pub shared_fundamental_ms: Option<f32>,
    pub shared_min_ms: Option<f32>,
    pub shared_max_ms: Option<f32>,
    pub device_default_ms: f32,
    pub device_min_ms: f32,
}

impl Periods {
    /// The smallest period the engine will run at for this endpoint in shared mode.
    pub fn best_shared_min_ms(&self) -> f32 {
        self.shared_min_ms.unwrap_or(self.device_min_ms)
    }
}

const HNS_PER_MS: f32 = 10_000.0;

fn hns_to_ms(hns: i64) -> f32 {
    hns as f32 / HNS_PER_MS
}

fn frames_to_ms(frames: u32, sample_rate: u32) -> f32 {
    frames as f32 * 1000.0 / sample_rate as f32
}

impl Enumerator {
    pub fn new() -> Result<Self> {
        // SAFETY: the MMDeviceEnumerator class is registered on every Windows audio system.
        let inner: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("no audio device enumerator")?;
        Ok(Self { inner })
    }

    fn default_id(&self, flow: EDataFlow) -> Option<String> {
        // SAFETY: plain query on the enumerator owned by `self`.
        let device = unsafe { self.inner.GetDefaultAudioEndpoint(flow, eConsole) }.ok()?;
        // SAFETY: the device is valid; GetId allocates the returned string with CoTaskMemAlloc.
        let id = unsafe { device.GetId() }.ok()?;
        Some(take_pwstr(id))
    }

    /// Active endpoints of one flow, in the order Windows lists them.
    pub fn endpoints(&self, flow: EDataFlow) -> Result<Vec<Endpoint>> {
        let default_id = self.default_id(flow);
        // SAFETY: enumerates active endpoints of the given flow on the owned enumerator.
        let collection = unsafe { self.inner.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }
            .context("EnumAudioEndpoints failed")?;
        // SAFETY: plain count query on a valid collection.
        let count = unsafe { collection.GetCount() }.context("GetCount failed")?;
        let mut out = Vec::with_capacity(count as usize);
        for index in 0..count {
            // SAFETY: index < count.
            let device = unsafe { collection.Item(index) }.context("Item failed")?;
            // SAFETY: GetId returns a COM-allocated string, released by take_pwstr.
            let id = take_pwstr(unsafe { device.GetId() }.unwrap_or(PWSTR::null()));
            let name = friendly_name(&device).unwrap_or_else(|| format!("endpoint {index}"));
            let is_default = default_id.as_deref() == Some(id.as_str());
            out.push(Endpoint { device, name, is_default });
        }
        Ok(out)
    }
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: opening a read-only property store on a valid device.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    // SAFETY: the key is a process-wide constant; GetValue copies the PROPVARIANT out.
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    Some(value.to_string())
}

fn activate<T: Interface>(device: &IMMDevice) -> Result<T> {
    // SAFETY: activation with no activation parameters, as documented for the audio endpoint interfaces.
    unsafe { device.Activate::<T>(CLSCTX_ALL, None) }.map_err(|e| anyhow!("Activate failed: {e}"))
}

/// Shared-mode engine periods (default, fundamental, min, max) in frames, if IAudioClient3 is available.
pub(crate) fn shared_engine_periods(
    client3: &IAudioClient3,
    format: &OwnedFormat,
) -> Option<(u32, u32, u32, u32)> {
    let (mut default, mut fundamental, mut min, mut max) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: `format` is a valid WAVEFORMATEX and the out pointers reference live locals.
    unsafe {
        client3
            .GetSharedModeEnginePeriod(format.as_ptr(), &mut default, &mut fundamental, &mut min, &mut max)
            .ok()?;
    }
    Some((default, fundamental, min, max))
}

pub fn periods(device: &IMMDevice) -> Result<Periods> {
    let client: IAudioClient = activate(device)?;
    let format = mix_format(&client)?;
    let sample_rate = format.stream_format()?.sample_rate;

    let (mut default_hns, mut min_hns) = (0i64, 0i64);
    // SAFETY: both out pointers reference live locals.
    unsafe { client.GetDevicePeriod(Some(&mut default_hns), Some(&mut min_hns)) }.context("GetDevicePeriod failed")?;

    let shared = activate::<IAudioClient3>(device)
        .ok()
        .and_then(|c3| shared_engine_periods(&c3, &format));

    Ok(Periods {
        sample_rate,
        shared_default_ms: shared.map(|(d, _, _, _)| frames_to_ms(d, sample_rate)),
        shared_fundamental_ms: shared.map(|(_, f, _, _)| frames_to_ms(f, sample_rate)),
        shared_min_ms: shared.map(|(_, _, m, _)| frames_to_ms(m, sample_rate)),
        shared_max_ms: shared.map(|(_, _, _, x)| frames_to_ms(x, sample_rate)),
        device_default_ms: hns_to_ms(default_hns),
        device_min_ms: hns_to_ms(min_hns),
    })
}

/// Reports every active endpoint of one flow for the `devices` event.
pub fn summaries(enumerator: &Enumerator, flow: EDataFlow) -> Result<Vec<DeviceSummary>> {
    let mut out = Vec::new();
    for endpoint in enumerator.endpoints(flow)? {
        let (sample_rate, min_period_ms) = match periods(&endpoint.device) {
            Ok(p) => (p.sample_rate, p.best_shared_min_ms()),
            Err(_) => (0, 0.0),
        };
        out.push(DeviceSummary {
            name: endpoint.name,
            is_default: endpoint.is_default,
            min_period_ms,
            sample_rate,
        });
    }
    Ok(out)
}

/// Resolves a UI device name to an endpoint. An empty request means the default: for output, a
/// virtual cable is preferred when `prefer_virtual` is set (PROTOCOL.md).
pub fn resolve(enumerator: &Enumerator, flow: EDataFlow, requested: &str, prefer_virtual: bool) -> Result<Endpoint> {
    let mut endpoints = enumerator.endpoints(flow)?;
    let names: Vec<String> = endpoints.iter().map(|e| e.name.clone()).collect();
    let wanted = names::strip_host_suffix(requested);

    if wanted.is_empty() {
        let default_index = endpoints.iter().position(|e| e.is_default);
        let index = if flow == eRender && prefer_virtual {
            names::default_output_index(&names, default_index)
        } else {
            default_index
        };
        let index = index.ok_or_else(|| anyhow!("no default {} device", flow_name(flow)))?;
        return Ok(endpoints.swap_remove(index));
    }

    let index = names::match_device(wanted, &names)
        .ok_or_else(|| anyhow!("no {} device matches \"{}\"", flow_name(flow), wanted))?;
    Ok(endpoints.swap_remove(index))
}

pub fn flow_name(flow: EDataFlow) -> &'static str {
    if flow == eCapture {
        "input"
    } else {
        "output"
    }
}
