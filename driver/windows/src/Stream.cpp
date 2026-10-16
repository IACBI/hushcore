// HushCore Virtual Audio: WaveRT stream.
//
// Position is derived from the performance counter since the stream entered KSSTATE_RUN, so
// the frame count advances at exactly 48 kHz with no hardware. A 1 ms high-resolution timer
// copies the frames that have elapsed since the last tick. GetPosition also advances the
// stream, so a reader that polls between ticks sees current data.
#include "Common.h"
#include "Endpoints.h"

namespace {

constexpr LONGLONG TimerDueTime100Ns = -10000;   // 1 ms, relative
constexpr LONGLONG TimerPeriod100Ns = 10000;     // 1 ms

VOID HushTimerCallback(_In_ PEX_TIMER Timer, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(Timer);
    static_cast<HushStream*>(Context)->OnTimer();
}

} // namespace

HushStream::HushStream(_In_ HushAdapter* Adapter, _In_ BOOLEAN Capture)
{
    m_RefCount = 1;
    m_Adapter = Adapter;
    m_Capture = Capture;
    KeInitializeSpinLock(&m_Lock);
    m_State = KSSTATE_STOP;
    m_Running = FALSE;
    m_Mdl = nullptr;
    m_Buffer = nullptr;
    m_BufferBytes = 0;
    m_BufferFrames = 0;
    m_BaseFrames = 0;
    m_ProcessedFrames = 0;
    m_StartTicks.QuadPart = 0;
    m_TickFrequency = 0;
    m_PeriodFrames = 0;
    m_LagFrames = HushMinLagFrames;
    m_NotifyEvent = nullptr;
    m_LastNotifyIndex = 0;
    m_Timer = nullptr;

    // The stream keeps the shared ring alive until it is destroyed.
    m_Adapter->AddRef();
}

HushStream::~HushStream()
{
    // ExDeleteTimer waits for a running callback, so the buffer is not freed under it.
    StopTimer();
    m_Running = FALSE;
    ReleaseBuffer();
    m_Adapter->Release();
}

STDMETHODIMP_(NTSTATUS) HushStream::QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface)
{
    if (Interface == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    if (IsEqualGUID(InterfaceId, IID_IUnknown) ||
        IsEqualGUID(InterfaceId, IID_IMiniportWaveRTStream) ||
        IsEqualGUID(InterfaceId, IID_IMiniportWaveRTStreamNotification)) {
        *Interface = static_cast<IMiniportWaveRTStreamNotification*>(this);
        AddRef();
        return STATUS_SUCCESS;
    }
    *Interface = nullptr;
    return STATUS_NOINTERFACE;
}

STDMETHODIMP_(ULONG) HushStream::AddRef()
{
    return static_cast<ULONG>(InterlockedIncrement(&m_RefCount));
}

STDMETHODIMP_(ULONG) HushStream::Release()
{
    const LONG count = InterlockedDecrement(&m_RefCount);
    if (count == 0) {
        delete this;
    }
    return static_cast<ULONG>(count);
}

STDMETHODIMP_(NTSTATUS) HushStream::SetState(_In_ KSSTATE NewState)
{
    NTSTATUS status = STATUS_SUCCESS;

    switch (NewState) {
    case KSSTATE_RUN:
        status = StartRun();
        break;

    case KSSTATE_ACQUIRE:
    case KSSTATE_PAUSE:
        // Freeze the position so a later RUN resumes from here.
        StopTimer();
        {
            KIRQL irql;
            KeAcquireSpinLock(&m_Lock, &irql);
            if (m_Running) {
                AdvanceLocked();
                m_BaseFrames = CurrentFramesLocked();
                m_Running = FALSE;
                if (!m_Capture) {
                    m_Adapter->Ring.Drain();
                }
            }
            KeReleaseSpinLock(&m_Lock, irql);
        }
        break;

    case KSSTATE_STOP:
        StopTimer();
        {
            KIRQL irql;
            KeAcquireSpinLock(&m_Lock, &irql);
            if (m_Running) {
                AdvanceLocked();
            }
            m_Running = FALSE;
            m_BaseFrames = 0;
            m_ProcessedFrames = 0;
            if (!m_Capture) {
                m_Adapter->Ring.Drain();
            }
            KeReleaseSpinLock(&m_Lock, irql);
        }
        break;

    default:
        status = STATUS_INVALID_PARAMETER;
        break;
    }

    if (NT_SUCCESS(status)) {
        m_State = NewState;
    }
    return status;
}

STDMETHODIMP_(NTSTATUS) HushStream::SetFormat(_In_ PKSDATAFORMAT DataFormat)
{
    if (DataFormat == nullptr || !HushIsFixedFormat(DataFormat, DataFormat->FormatSize)) {
        return STATUS_NOT_SUPPORTED;
    }
    return STATUS_SUCCESS;
}

// WaveRT reports positions as byte offsets inside the cyclic buffer; the audio engine tracks
// wrap-around itself. The play (render) or record (capture) offset doubles as the write offset.
STDMETHODIMP_(NTSTATUS) HushStream::GetPosition(_Out_ PKSAUDIO_POSITION Position)
{
    if (Position == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }

    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    if (m_Running) {
        AdvanceLocked();
    }
    ULONGLONG offset = 0;
    if (m_BufferBytes != 0) {
        offset = (CurrentFramesLocked() * HushFrameBytes) % m_BufferBytes;
    }
    Position->PlayOffset = offset;
    Position->WriteOffset = offset;
    KeReleaseSpinLock(&m_Lock, irql);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) HushStream::AllocateAudioBuffer(_In_ ULONG RequestedSize,
                                                        _Out_ PMDL* AudioBufferMdl,
                                                        _Out_ ULONG* ActualSize,
                                                        _Out_ ULONG* OffsetFromFirstPage,
                                                        _Out_ MEMORY_CACHING_TYPE* CacheType)
{
    return AllocateBuffer(RequestedSize, AudioBufferMdl, ActualSize, OffsetFromFirstPage, CacheType, 0);
}

STDMETHODIMP_(void) HushStream::FreeAudioBuffer(_In_opt_ PMDL AudioBufferMdl, _In_ ULONG BufferSize)
{
    UNREFERENCED_PARAMETER(BufferSize);
    // PortCls only hands back the MDL this stream returned from AllocateAudioBuffer. Free the
    // stream's own buffer either way, so a mismatch can never leak it.
    NT_ASSERT(AudioBufferMdl == m_Mdl);
    StopTimer();
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    m_Running = FALSE;
    KeReleaseSpinLock(&m_Lock, irql);
    ReleaseBuffer();
}

STDMETHODIMP_(void) HushStream::GetHWLatency(_Out_ KSRTAUDIO_HWLATENCY* Latency)
{
    if (Latency == nullptr) {
        return;
    }
    // No hardware FIFO or codec. The ring is the only buffering and is not reported here.
    Latency->FifoSize = 0;
    Latency->ChipsetDelay = 0;
    Latency->CodecDelay = 0;
}

STDMETHODIMP_(NTSTATUS) HushStream::GetPositionRegister(_Out_ PKSRTAUDIO_HWREGISTER Register)
{
    UNREFERENCED_PARAMETER(Register);
    return STATUS_NOT_SUPPORTED;
}

STDMETHODIMP_(NTSTATUS) HushStream::GetClockRegister(_Out_ PKSRTAUDIO_HWREGISTER Register)
{
    UNREFERENCED_PARAMETER(Register);
    return STATUS_NOT_SUPPORTED;
}

STDMETHODIMP_(NTSTATUS) HushStream::AllocateBufferWithNotification(_In_ ULONG NotificationCount,
                                                                   _In_ ULONG RequestedSize,
                                                                   _Out_ PMDL* AudioBufferMdl,
                                                                   _Out_ ULONG* ActualSize,
                                                                   _Out_ ULONG* OffsetFromFirstPage,
                                                                   _Out_ MEMORY_CACHING_TYPE* CacheType)
{
    if (NotificationCount == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    const ULONG frames = (RequestedSize / HushFrameBytes);
    const ULONG period = frames / NotificationCount;
    if (period == 0) {
        return STATUS_INVALID_PARAMETER;
    }
    return AllocateBuffer(RequestedSize, AudioBufferMdl, ActualSize, OffsetFromFirstPage, CacheType, period);
}

STDMETHODIMP_(void) HushStream::FreeBufferWithNotification(_In_ PMDL AudioBufferMdl, _In_ ULONG BufferSize)
{
    FreeAudioBuffer(AudioBufferMdl, BufferSize);
}

STDMETHODIMP_(NTSTATUS) HushStream::RegisterNotificationEvent(_In_ PKEVENT NotificationEvent)
{
    if (NotificationEvent == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    NTSTATUS status = STATUS_SUCCESS;
    if (m_NotifyEvent != nullptr && m_NotifyEvent != NotificationEvent) {
        status = STATUS_DEVICE_BUSY;
    } else {
        m_NotifyEvent = NotificationEvent;
    }
    KeReleaseSpinLock(&m_Lock, irql);
    return status;
}

STDMETHODIMP_(NTSTATUS) HushStream::UnregisterNotificationEvent(_In_ PKEVENT NotificationEvent)
{
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    if (m_NotifyEvent == NotificationEvent) {
        m_NotifyEvent = nullptr;
    }
    KeReleaseSpinLock(&m_Lock, irql);
    return STATUS_SUCCESS;
}

VOID HushStream::OnTimer()
{
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    AdvanceLocked();
    KeReleaseSpinLock(&m_Lock, irql);
}

NTSTATUS HushStream::StartRun()
{
    if (m_Buffer == nullptr) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    if (m_Running) {
        return STATUS_SUCCESS;
    }

    if (m_Timer == nullptr) {
        // EX_TIMER_HIGH_RESOLUTION gives roughly 1 ms ticks even when other components
        // have not raised the system timer resolution.
        m_Timer = ExAllocateTimer(HushTimerCallback, this, EX_TIMER_HIGH_RESOLUTION);
        if (m_Timer == nullptr) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }
    }

    LARGE_INTEGER frequency;
    LARGE_INTEGER now = KeQueryPerformanceCounter(&frequency);

    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    m_TickFrequency = frequency.QuadPart;
    m_StartTicks = now;
    m_ProcessedFrames = m_BaseFrames;
    m_LastNotifyIndex = (m_PeriodFrames != 0) ? (m_ProcessedFrames / m_PeriodFrames) : 0;
    m_Running = TRUE;
    if (m_Capture) {
        // Start the reader one lag behind the writer, not at the oldest ring content.
        m_Adapter->Ring.PositionReader(m_LagFrames);
    }
    KeReleaseSpinLock(&m_Lock, irql);

    ExSetTimer(m_Timer, TimerDueTime100Ns, TimerPeriod100Ns, nullptr);
    return STATUS_SUCCESS;
}

VOID HushStream::StopTimer()
{
    if (m_Timer != nullptr) {
        ExDeleteTimer(m_Timer, TRUE, TRUE);
        m_Timer = nullptr;
    }
}

// Caller holds m_Lock.
VOID HushStream::AdvanceLocked()
{
    if (!m_Running || m_Buffer == nullptr || m_BufferFrames == 0) {
        return;
    }

    const ULONGLONG now = CurrentFramesLocked();
    if (now <= m_ProcessedFrames) {
        return;
    }

    ULONGLONG pending = now - m_ProcessedFrames;
    if (pending > m_BufferFrames) {
        // Fell behind by more than a whole buffer: the older data is already gone.
        m_ProcessedFrames = now - m_BufferFrames;
        pending = m_BufferFrames;
    }

    ULONGLONG remaining = pending;
    while (remaining != 0) {
        const ULONG bufferIndex = static_cast<ULONG>(m_ProcessedFrames % m_BufferFrames);
        const ULONG untilWrap = static_cast<ULONG>(m_BufferFrames - bufferIndex);
        const ULONG chunk = static_cast<ULONG>((remaining < untilWrap) ? remaining : untilWrap);
        UCHAR* bytes = m_Buffer + static_cast<SIZE_T>(bufferIndex) * HushFrameBytes;

        if (m_Capture) {
            m_Adapter->Ring.Read(bytes, chunk);
        } else {
            m_Adapter->Ring.Write(bytes, chunk);
        }

        m_ProcessedFrames += chunk;
        remaining -= chunk;
    }

    if (m_NotifyEvent != nullptr && m_PeriodFrames != 0) {
        const ULONGLONG index = m_ProcessedFrames / m_PeriodFrames;
        if (index != m_LastNotifyIndex) {
            m_LastNotifyIndex = index;
            KeSetEvent(m_NotifyEvent, IO_NO_INCREMENT, FALSE);
        }
    }
}

// Caller holds m_Lock (or the stream is not yet visible to any other thread).
ULONGLONG HushStream::CurrentFramesLocked() const
{
    if (!m_Running || m_TickFrequency <= 0) {
        return m_BaseFrames;
    }

    const LARGE_INTEGER now = KeQueryPerformanceCounter(nullptr);
    const ULONGLONG elapsed = static_cast<ULONGLONG>(now.QuadPart - m_StartTicks.QuadPart);
    const ULONGLONG frequency = static_cast<ULONGLONG>(m_TickFrequency);

    // Split the division so elapsed * 48000 never overflows.
    return m_BaseFrames +
           (elapsed / frequency) * HushSampleRate +
           ((elapsed % frequency) * HushSampleRate) / frequency;
}

NTSTATUS HushStream::AllocateBuffer(_In_ ULONG RequestedSize, _Out_ PMDL* AudioBufferMdl,
                                    _Out_ ULONG* ActualSize, _Out_ ULONG* OffsetFromFirstPage,
                                    _Out_ MEMORY_CACHING_TYPE* CacheType, _In_ ULONG Period)
{
    PAGED_CODE();

    if (AudioBufferMdl == nullptr || ActualSize == nullptr ||
        OffsetFromFirstPage == nullptr || CacheType == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    if (RequestedSize == 0 || RequestedSize > HushMaxBufferBytes) {
        return STATUS_INVALID_PARAMETER;
    }
    if (m_Mdl != nullptr || m_Running) {
        return STATUS_DEVICE_BUSY;
    }

    const ULONG bytes = RequestedSize - (RequestedSize % HushFrameBytes);
    if (bytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    PHYSICAL_ADDRESS lowAddress;
    PHYSICAL_ADDRESS highAddress;
    PHYSICAL_ADDRESS skipBytes;
    lowAddress.QuadPart = 0;
    highAddress.QuadPart = -1;
    skipBytes.QuadPart = 0;

    PMDL mdl = MmAllocatePagesForMdl(lowAddress, highAddress, skipBytes, bytes);
    if (mdl == nullptr) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    if (MmGetMdlByteCount(mdl) < bytes) {
        // A partial allocation is unusable for a cyclic buffer.
        MmFreePagesFromMdl(mdl);
        ExFreePool(mdl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    PVOID systemAddress = MmMapLockedPagesSpecifyCache(mdl, KernelMode, MmCached, nullptr,
                                                       FALSE, NormalPagePriority);
    if (systemAddress == nullptr) {
        MmFreePagesFromMdl(mdl);
        ExFreePool(mdl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(systemAddress, bytes);

    m_Mdl = mdl;
    m_Buffer = static_cast<PUCHAR>(systemAddress);
    m_BufferBytes = bytes;
    m_BufferFrames = bytes / HushFrameBytes;
    m_PeriodFrames = Period;
    m_LagFrames = (2 * Period > HushMinLagFrames) ? (2 * Period) : HushMinLagFrames;

    *AudioBufferMdl = mdl;
    *ActualSize = bytes;
    *OffsetFromFirstPage = 0;
    *CacheType = MmCached;
    return STATUS_SUCCESS;
}

VOID HushStream::ReleaseBuffer()
{
    if (m_Mdl != nullptr) {
        if (m_Buffer != nullptr) {
            MmUnmapLockedPages(m_Buffer, m_Mdl);
        }
        MmFreePagesFromMdl(m_Mdl);
        ExFreePool(m_Mdl);
    }
    m_Mdl = nullptr;
    m_Buffer = nullptr;
    m_BufferBytes = 0;
    m_BufferFrames = 0;
}
