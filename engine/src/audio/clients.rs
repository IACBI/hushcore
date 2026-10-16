//! Opens WASAPI clients in the three modes and reports the periods they actually run at.
//! Order of preference: shared low-latency (IAudioClient3), shared classic, exclusive (on request).

use anyhow::{anyhow, bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioClient3, IAudioRenderClient, IMMDevice, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::CLSCTX_ALL;

use super::com::{as_audio_client, mix_format, AutoEvent, OwnedFormat};
use super::devices::shared_engine_periods;
use super::format::StreamFormat;
use super::ring::frames_to_ms;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    Shared,
    Exclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitKind {
    LowLatency,
    Classic,
    Exclusive,
}

/// An initialised client, not yet started.
pub struct Opened {
    pub client: IAudioClient,
    pub event: AutoEvent,
    pub format: StreamFormat,
    pub init: InitKind,
    /// The period the engine signals at, measured from the stream that was actually initialised.
    pub period_ms: f32,
    pub buffer_frames: u32,
}

pub fn open_capture(device: &IMMDevice, mode: StreamMode) -> Result<(Opened, IAudioCaptureClient)> {
    let opened = match mode {
        StreamMode::Shared => open_shared(device, None)?,
        StreamMode::Exclusive => open_exclusive(device, None)?,
    };
    // SAFETY: the client is initialised and the service interface is supported for capture endpoints.
    let service = unsafe { opened.client.GetService::<IAudioCaptureClient>() }
        .context("capture service unavailable")?;
    Ok((opened, service))
}

/// Opens a render client. `rate` is the engine rate; shared mode converts to it, exclusive mode needs it natively.
pub fn open_render(device: &IMMDevice, mode: StreamMode, rate: u32) -> Result<(Opened, IAudioRenderClient)> {
    let opened = match mode {
        StreamMode::Shared => open_shared(device, Some(rate))?,
        StreamMode::Exclusive => open_exclusive(device, Some(rate))?,
    };
    // SAFETY: the client is initialised and the service interface is supported for render endpoints.
    let service = unsafe { opened.client.GetService::<IAudioRenderClient>() }
        .context("render service unavailable")?;
    Ok((opened, service))
}

fn activate<T: Interface>(device: &IMMDevice) -> Result<T> {
    // SAFETY: activation with no activation parameters, as for every audio endpoint interface.
    unsafe { device.Activate::<T>(CLSCTX_ALL, None) }.map_err(|e| anyhow!("Activate failed: {e}"))
}

fn open_shared(device: &IMMDevice, rate: Option<u32>) -> Result<Opened> {
    let probe: IAudioClient = activate(device)?;
    let mix = mix_format(&probe)?;
    let mix_fmt = mix.stream_format()?;
    let want = rate.unwrap_or(mix_fmt.sample_rate);
    if want == mix_fmt.sample_rate {
        if let Some(opened) = try_low_latency(device, &mix, mix_fmt) {
            return Ok(opened);
        }
    }
    open_shared_classic(device, &mix, mix_fmt, want)
}

/// Shared low-latency: the engine's minimum period, event-driven. Any failure falls back to classic.
fn try_low_latency(device: &IMMDevice, mix: &OwnedFormat, mix_fmt: StreamFormat) -> Option<Opened> {
    let c3: IAudioClient3 = activate(device).ok()?;
    let (_, _, min_frames, _) = shared_engine_periods(&c3, mix)?;
    // SAFETY: `mix` is the format the engine reports for this device; the minimum period came from the same format.
    unsafe { c3.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min_frames, mix.as_ptr(), None) }
        .ok()?;
    let event = AutoEvent::new().ok()?;
    // SAFETY: the stream is initialised; the handle stays alive in the returned Opened.
    unsafe { c3.SetEventHandle(event.raw()) }.ok()?;

    let mut current_format: *mut WAVEFORMATEX = std::ptr::null_mut();
    let mut current_period = 0u32;
    // SAFETY: both out pointers reference live locals; the returned format is owned and freed below.
    unsafe { c3.GetCurrentSharedModeEnginePeriod(&mut current_format, &mut current_period) }.ok()?;
    // SAFETY: GetCurrentSharedModeEnginePeriod allocated `current_format` with CoTaskMemAlloc.
    drop(unsafe { OwnedFormat::from_raw(current_format) });

    // SAFETY: the initialised client is queried for its buffer size.
    let buffer_frames = unsafe { c3.GetBufferSize() }.ok()?;
    let client = as_audio_client(&c3).ok()?;
    Some(Opened {
        client,
        event,
        format: mix_fmt,
        init: InitKind::LowLatency,
        period_ms: frames_to_ms(current_period as f32, mix_fmt.sample_rate),
        buffer_frames,
    })
}

fn open_shared_classic(device: &IMMDevice, mix: &OwnedFormat, mix_fmt: StreamFormat, want: u32) -> Result<Opened> {
    let client: IAudioClient = activate(device)?;
    let (mut default_hns, mut min_hns) = (0i64, 0i64);
    // SAFETY: both out pointers reference live locals.
    unsafe { client.GetDevicePeriod(Some(&mut default_hns), Some(&mut min_hns)) }
        .context("GetDevicePeriod failed")?;

    let converted_fmt = StreamFormat { sample_rate: want, ..mix_fmt };
    let converted = converted_fmt.to_waveformatex();
    let (format, format_ptr, flags) = if want == mix_fmt.sample_rate {
        (mix_fmt, mix.as_ptr(), AUDCLNT_STREAMFLAGS_EVENTCALLBACK)
    } else {
        (
            converted_fmt,
            &converted as *const WAVEFORMATEX,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        )
    };
    // Event-driven shared streams must use zero buffer duration and periodicity (the engine decides).
    // SAFETY: `format_ptr` points at `mix` or at `converted`, both alive for the call.
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 0, 0, format_ptr, None) }
        .context("shared Initialize failed")?;
    let event = AutoEvent::new()?;
    // SAFETY: the stream is initialised; the handle is kept alive by the returned Opened.
    unsafe { client.SetEventHandle(event.raw()) }.context("SetEventHandle failed")?;
    // SAFETY: the stream is initialised.
    let buffer_frames = unsafe { client.GetBufferSize() }.context("GetBufferSize failed")?;
    Ok(Opened {
        client,
        event,
        format,
        init: InitKind::Classic,
        period_ms: default_hns as f32 / 10_000.0,
        buffer_frames,
    })
}

fn open_exclusive(device: &IMMDevice, rate: Option<u32>) -> Result<Opened> {
    let mut client: IAudioClient = activate(device)?;
    let mix = mix_format(&client)?.stream_format()?;
    let format = pick_exclusive_format(&client, rate.unwrap_or(mix.sample_rate), mix.channels)?;
    let wfx = format.to_waveformatex();

    let mut min_hns = 0i64;
    // SAFETY: the out pointer references a live local.
    unsafe { client.GetDevicePeriod(None, Some(&mut min_hns)) }.context("GetDevicePeriod failed")?;

    // Event-driven exclusive streams need buffer == periodicity. If the device rejects the minimum as
    // unaligned, it reports the aligned buffer size; the client must be re-activated to retry.
    let mut period_hns = min_hns;
    let mut attempt = 0;
    loop {
        attempt += 1;
        // SAFETY: `wfx` is a valid WAVEFORMATEX that was accepted by IsFormatSupported.
        match unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period_hns, period_hns, &wfx, None)
        } {
            Ok(()) => break,
            Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED && attempt < 2 => {
                // SAFETY: the failed client is still valid for a size query.
                let frames = unsafe { client.GetBufferSize() }.context("GetBufferSize failed")?;
                period_hns = (frames as f64 * 10_000_000.0 / format.sample_rate as f64).ceil() as i64;
                client = activate(device)?;
            }
            Err(e) => return Err(anyhow!(e).context("exclusive Initialize failed")),
        }
    }

    let event = AutoEvent::new()?;
    // SAFETY: the stream is initialised; the handle is kept alive by the returned Opened.
    unsafe { client.SetEventHandle(event.raw()) }.context("SetEventHandle failed")?;
    // SAFETY: the stream is initialised.
    let buffer_frames = unsafe { client.GetBufferSize() }.context("GetBufferSize failed")?;
    Ok(Opened {
        client,
        event,
        format,
        init: InitKind::Exclusive,
        period_ms: period_hns as f32 / 10_000.0,
        buffer_frames,
    })
}

/// First exclusive format the device accepts at `rate`, preferring float and then wider integers.
fn pick_exclusive_format(client: &IAudioClient, rate: u32, channels: u16) -> Result<StreamFormat> {
    use super::format::SampleKind::{F32, I16, I24, I32};
    for kind in [F32, I32, I24, I16] {
        let candidate = StreamFormat { sample_rate: rate, channels, kind };
        let wfx = candidate.to_waveformatex();
        // SAFETY: `wfx` is a valid WAVEFORMATEX for the call; no closest-match out pointer is requested.
        if unsafe { client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &wfx, None) }.is_ok() {
            return Ok(candidate);
        }
    }
    bail!("exclusive mode rejects every float and PCM format at {rate} Hz, {channels} channels")
}

/// Silence bit as the u32 that ReleaseBuffer expects.
pub const SILENT_FLAG: u32 = AUDCLNT_BUFFERFLAGS_SILENT.0 as u32;
