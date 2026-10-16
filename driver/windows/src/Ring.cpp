// HushCore Virtual Audio: loopback ring and the shared adapter object.
//
// The ring is touched from the high-resolution timer (DISPATCH_LEVEL) and from GetPosition,
// so it lives in non-paged pool and is guarded by a spin lock. Every operation is a bounded
// memory copy; nothing here allocates or waits.
#include "Common.h"

namespace {

constexpr ULONG RingBytes = HushRingFrames * HushFrameBytes;
constexpr ULONG RingMask = HushRingFrames - 1;

} // namespace

NTSTATUS HushRing::Initialize()
{
    KeInitializeSpinLock(&m_Lock);
    m_WriteFrame = 0;
    m_ReadFrame = 0;
    m_OverrunCount = 0;
    m_UnderrunCount = 0;

    m_Storage = static_cast<PUCHAR>(ExAllocatePool2(POOL_FLAG_NON_PAGED, RingBytes, HUSH_RING_POOLTAG));
    if (m_Storage == nullptr) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    return STATUS_SUCCESS;
}

VOID HushRing::Destroy()
{
    if (m_Storage != nullptr) {
        ExFreePoolWithTag(m_Storage, HUSH_RING_POOLTAG);
        m_Storage = nullptr;
    }
}

VOID HushRing::Write(_In_reads_bytes_(Frames * HushFrameBytes) const UCHAR* Source, _In_ ULONG Frames)
{
    if (Frames == 0 || m_Storage == nullptr) {
        return;
    }

    // A block larger than the ring keeps only its newest HushRingFrames frames.
    if (Frames > HushRingFrames) {
        Source += static_cast<SIZE_T>(Frames - HushRingFrames) * HushFrameBytes;
        Frames = HushRingFrames;
    }

    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);

    // Never let unread data exceed the ring: discard the oldest frames and count the overrun.
    const ULONGLONG unread = m_WriteFrame - m_ReadFrame;
    if (unread + Frames > HushRingFrames) {
        m_ReadFrame = m_WriteFrame + Frames - HushRingFrames;
        m_OverrunCount++;
    }

    const ULONG index = static_cast<ULONG>(m_WriteFrame & RingMask);
    const ULONG first = (Frames < HushRingFrames - index) ? Frames : (HushRingFrames - index);
    RtlCopyMemory(m_Storage + static_cast<SIZE_T>(index) * HushFrameBytes, Source,
                  static_cast<SIZE_T>(first) * HushFrameBytes);
    RtlCopyMemory(m_Storage, Source + static_cast<SIZE_T>(first) * HushFrameBytes,
                  static_cast<SIZE_T>(Frames - first) * HushFrameBytes);
    m_WriteFrame += Frames;

    KeReleaseSpinLock(&m_Lock, irql);
}

VOID HushRing::Read(_Out_writes_bytes_(Frames * HushFrameBytes) UCHAR* Destination, _In_ ULONG Frames)
{
    if (Frames == 0) {
        return;
    }
    if (m_Storage == nullptr) {
        RtlZeroMemory(Destination, static_cast<SIZE_T>(Frames) * HushFrameBytes);
        return;
    }

    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);

    // The reader never passes the writer: anything not yet written is silence.
    const ULONGLONG available = m_WriteFrame - m_ReadFrame;
    const ULONG count = (available >= Frames) ? Frames : static_cast<ULONG>(available);

    const ULONG index = static_cast<ULONG>(m_ReadFrame & RingMask);
    const ULONG first = (count < HushRingFrames - index) ? count : (HushRingFrames - index);
    RtlCopyMemory(Destination, m_Storage + static_cast<SIZE_T>(index) * HushFrameBytes,
                  static_cast<SIZE_T>(first) * HushFrameBytes);
    RtlCopyMemory(Destination + static_cast<SIZE_T>(first) * HushFrameBytes, m_Storage,
                  static_cast<SIZE_T>(count - first) * HushFrameBytes);
    m_ReadFrame += count;

    if (count < Frames) {
        RtlZeroMemory(Destination + static_cast<SIZE_T>(count) * HushFrameBytes,
                      static_cast<SIZE_T>(Frames - count) * HushFrameBytes);
        if (count > 0) {
            m_UnderrunCount++;
        }
    }

    KeReleaseSpinLock(&m_Lock, irql);
}

VOID HushRing::Drain()
{
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    m_ReadFrame = m_WriteFrame;
    KeReleaseSpinLock(&m_Lock, irql);
}

VOID HushRing::PositionReader(_In_ ULONG LagFrames)
{
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    if (LagFrames > HushRingFrames / 2) {
        LagFrames = HushRingFrames / 2;
    }
    m_ReadFrame = (m_WriteFrame >= LagFrames) ? (m_WriteFrame - LagFrames) : m_WriteFrame;
    KeReleaseSpinLock(&m_Lock, irql);
}

VOID HushRing::GetCounters(_Out_ PULONG Overruns, _Out_ PULONG Underruns)
{
    KIRQL irql;
    KeAcquireSpinLock(&m_Lock, &irql);
    *Overruns = m_OverrunCount;
    *Underruns = m_UnderrunCount;
    KeReleaseSpinLock(&m_Lock, irql);
}

NTSTATUS HushAdapter::Initialize()
{
    return Ring.Initialize();
}

STDMETHODIMP_(NTSTATUS) HushAdapter::QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface)
{
    if (Interface == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    if (IsEqualGUID(InterfaceId, IID_IUnknown)) {
        *Interface = static_cast<IUnknown*>(this);
        AddRef();
        return STATUS_SUCCESS;
    }
    *Interface = nullptr;
    return STATUS_NOINTERFACE;
}

STDMETHODIMP_(ULONG) HushAdapter::AddRef()
{
    return static_cast<ULONG>(InterlockedIncrement(&m_RefCount));
}

STDMETHODIMP_(ULONG) HushAdapter::Release()
{
    const LONG count = InterlockedDecrement(&m_RefCount);
    if (count == 0) {
        Ring.Destroy();
        delete this;
    }
    return static_cast<ULONG>(count);
}
