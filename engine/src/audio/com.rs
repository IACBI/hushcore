//! Small RAII wrappers over the COM, MMCSS and event-handle calls the audio code needs.
//! Every `unsafe` block here is a single OS call with its preconditions stated next to it.

use std::ffi::c_void;

use anyhow::{Context, Result};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{IAudioClient, WAVEFORMATEX};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, CreateEventW,
    GetCurrentThread, SetThreadPriority, WaitForSingleObject, AVRT_PRIORITY_CRITICAL, THREAD_PRIORITY_TIME_CRITICAL,
};

use super::format::StreamFormat;

/// Balances one successful `CoInitializeEx(MTA)` on the current thread.
pub struct ComInit;

impl ComInit {
    pub fn new() -> Result<Self> {
        // S_FALSE (already initialised as MTA) counts as success and still needs the matching uninit.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().context("CoInitializeEx failed")?;
        Ok(Self)
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Registers the calling thread with MMCSS as "Pro Audio" and raises its priority.
/// Failure is not fatal: the thread still runs, just without the scheduler class.
pub struct MmcssGuard {
    handle: Option<HANDLE>,
}

impl MmcssGuard {
    pub fn enter_pro_audio() -> Self {
        let mut task_index = 0u32;
        let name = wide("Pro Audio");
        // SAFETY: `name` is NUL-terminated and outlives the call; `task_index` is a valid out pointer.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(PCWSTR(name.as_ptr()), &mut task_index) }.ok();
        if let Some(h) = handle {
            // SAFETY: `h` is the handle MMCSS just returned for this thread.
            let _ = unsafe { AvSetMmThreadPriority(h, AVRT_PRIORITY_CRITICAL) };
        }
        // SAFETY: GetCurrentThread returns a pseudo-handle that is valid on this thread.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
        Self { handle }
    }
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // SAFETY: `h` came from AvSetMmThreadCharacteristicsW on this same thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
        }
    }
}

pub enum Wait {
    Signaled,
    TimedOut,
    Failed,
}

/// Auto-reset event handed to IAudioClient::SetEventHandle.
pub struct AutoEvent(HANDLE);

impl AutoEvent {
    pub fn new() -> Result<Self> {
        // SAFETY: default security, unnamed, auto-reset, initially unsignalled.
        let handle = unsafe { CreateEventW(None, false, false, None) }.context("CreateEventW failed")?;
        Ok(Self(handle))
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }

    pub fn wait(&self, timeout_ms: u32) -> Wait {
        // SAFETY: the handle is owned by `self` and still open.
        match unsafe { WaitForSingleObject(self.0, timeout_ms) } {
            WAIT_OBJECT_0 => Wait::Signaled,
            t if t.0 == 0x102 => Wait::TimedOut,
            _ => Wait::Failed,
        }
    }
}

impl Drop for AutoEvent {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateEventW and is closed exactly once here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: a Win32 event handle may be waited on and closed from any thread.
unsafe impl Send for AutoEvent {}

/// Owned WAVEFORMATEX returned by GetMixFormat / GetCurrentSharedModeEnginePeriod.
pub struct OwnedFormat(*mut WAVEFORMATEX);

impl OwnedFormat {
    /// # Safety
    /// `ptr` must come from a COM call that allocates with CoTaskMemAlloc.
    pub unsafe fn from_raw(ptr: *mut WAVEFORMATEX) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.0
    }

    pub fn stream_format(&self) -> Result<StreamFormat> {
        // SAFETY: the pointer is valid for as long as `self` lives.
        unsafe { StreamFormat::from_waveformatex(self.0) }
    }
}

impl Drop for OwnedFormat {
    fn drop(&mut self) {
        // SAFETY: the buffer was allocated by COM with CoTaskMemAlloc and is freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) };
    }
}

pub fn mix_format(client: &IAudioClient) -> Result<OwnedFormat> {
    // SAFETY: `client` is an initialised-or-not IAudioClient; GetMixFormat is valid on any activated client.
    let ptr = unsafe { client.GetMixFormat() }.context("GetMixFormat failed")?;
    // SAFETY: GetMixFormat allocates with CoTaskMemAlloc, which OwnedFormat frees.
    Ok(unsafe { OwnedFormat::from_raw(ptr) })
}

/// Takes ownership of a PWSTR from COM, returning it as a Rust string.
pub fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is a NUL-terminated wide string allocated by COM; it is freed exactly once below.
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Casts an activated IAudioClient3 down to the IAudioClient interface the rest of the code uses.
pub fn as_audio_client<T: Interface>(object: &T) -> Result<IAudioClient> {
    object.cast::<IAudioClient>().context("IAudioClient cast failed")
}

/// Moves COM objects into a worker thread. Objects created on an MTA thread may be used from any MTA
/// thread, and each payload is handed to exactly one worker and never touched again by its creator.
pub struct MtaPayload<T>(pub T);

// SAFETY: see the type docs; the only use is a single move into a spawned thread.
unsafe impl<T> Send for MtaPayload<T> {}

/// Raw byte view over a WASAPI buffer.
///
/// # Safety
/// `ptr` must be valid for `bytes` bytes of writes for the lifetime of the returned slice, which is
/// the duration of one GetBuffer/ReleaseBuffer pair.
pub unsafe fn buffer_slice<'a>(ptr: *mut u8, bytes: usize) -> Option<&'a mut [u8]> {
    if ptr.is_null() || bytes == 0 {
        None
    } else {
        Some(std::slice::from_raw_parts_mut(ptr, bytes))
    }
}
