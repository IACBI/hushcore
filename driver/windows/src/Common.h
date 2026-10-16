// HushCore Virtual Audio: shared declarations for the PortCls WaveRT driver.
//
// Everything here runs in kernel mode. The driver moves fixed-format frames between a
// render stream (HushCore Sink) and a capture stream (HushCore Microphone) through one
// non-paged ring buffer. No IOCTLs, no device interfaces beyond the KS audio categories.
#pragma once

#include <ntddk.h>
#include <portcls.h>
#include <ks.h>
#include <ksmedia.h>

#define HUSH_POOLTAG 'CVAH'
#define HUSH_RING_POOLTAG 'eroH'

#ifndef WAVE_FORMAT_EXTENSIBLE
#define WAVE_FORMAT_EXTENSIBLE 0xFFFE
#endif

#ifndef SIZEOF_ARRAY
#define SIZEOF_ARRAY(ar) (sizeof(ar) / sizeof((ar)[0]))
#endif

// Fixed format on both pins: 48 kHz, stereo, 32-bit IEEE float. The loopback is a byte copy.
constexpr ULONG HushSampleRate = 48000;
constexpr ULONG HushChannels = 2;
constexpr ULONG HushFrameBytes = 8;

// Ring capacity in frames. 16384 frames is 341 ms at 48 kHz: it holds the required 200 ms
// and keeps the capture reader's slack bounded.
constexpr ULONG HushRingFrames = 16384;

// Capture trails the writer by at least this many frames (20 ms), so ordinary timer jitter
// does not turn into silence.
constexpr ULONG HushMinLagFrames = 960;

// Upper bound for one cyclic stream buffer (about 21 s at this format). Larger requests are
// rejected rather than pinning unbounded non-paged memory.
constexpr SIZE_T HushMaxBufferBytes = 4 * 1024 * 1024;

constexpr ULONG HushMaxSubdevices = 4;

static_assert((HushRingFrames & (HushRingFrames - 1)) == 0, "ring size must be a power of two");
static_assert(HushRingFrames >= 9600, "ring must hold at least 200 ms at 48 kHz");

// Non-paged placement allocation. ExAllocatePool2 zeroes the memory. A failed allocation
// returns nullptr; exceptions are disabled in the project, so callers check the pointer.
inline void* __cdecl operator new(size_t Size, ULONG64 PoolFlags, ULONG Tag)
{
    return ExAllocatePool2(PoolFlags, Size, Tag);
}

// Replacement deletes may not be inline (C4595); they are defined in Adapter.cpp.
void __cdecl operator delete(void* Pointer);
void __cdecl operator delete(void* Pointer, size_t Size);

inline void __cdecl operator delete(void* Pointer, ULONG64, ULONG)
{
    if (Pointer != nullptr) {
        ExFreePoolWithTag(Pointer, HUSH_POOLTAG);
    }
}

class HushAdapter;
class HushStream;

// Single-writer, single-reader frame ring. Positions are absolute frame counters; the
// storage index is the counter masked by the capacity. Both sides take the same spin lock,
// so every call is safe at DISPATCH_LEVEL (the high-resolution timer runs there).
class HushRing {
public:
    NTSTATUS Initialize();
    VOID Destroy();

    // Appends frames. When the ring is full, the oldest unread frames are discarded and the
    // reader is moved forward (overrun).
    VOID Write(_In_reads_bytes_(Frames * HushFrameBytes) const UCHAR* Source, _In_ ULONG Frames);

    // Copies up to Frames frames to Destination and zero-fills the rest. Never reads past
    // the writer. A partial read counts as an underrun.
    VOID Read(_Out_writes_bytes_(Frames * HushFrameBytes) UCHAR* Destination, _In_ ULONG Frames);

    // Moves the reader to the writer so that no stale audio is replayed (render stop/pause).
    VOID Drain();

    // Moves the reader LagFrames behind the writer (capture start).
    VOID PositionReader(_In_ ULONG LagFrames);

    VOID GetCounters(_Out_ PULONG Overruns, _Out_ PULONG Underruns);

private:
    KSPIN_LOCK m_Lock;
    PUCHAR m_Storage;
    ULONGLONG m_WriteFrame;
    ULONGLONG m_ReadFrame;
    ULONG m_OverrunCount;
    ULONG m_UnderrunCount;
};

// Shared adapter state. Each miniport and stream holds a reference, so the ring outlives
// every subdevice. Implemented as a plain IUnknown: the object is not a PortCls class.
class HushAdapter final : public IUnknown {
public:
    HushAdapter() : m_RefCount(1) {}

    NTSTATUS Initialize();

    STDMETHODIMP_(NTSTATUS) QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    HushRing Ring;

private:
    LONG m_RefCount;
};

// Registers the four subdevices and the physical connections. Implemented in Adapter.cpp.
NTSTATUS HushRegisterEndpoints(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                               _In_opt_ PRESOURCELIST ResourceList, _In_ HushAdapter* Adapter);

// Descriptors and format checks. Implemented in Miniport.cpp.
extern PCFILTER_DESCRIPTOR g_WaveRenderDescriptor;
extern PCFILTER_DESCRIPTOR g_WaveCaptureDescriptor;
extern PCFILTER_DESCRIPTOR g_TopologyRenderDescriptor;
extern PCFILTER_DESCRIPTOR g_TopologyCaptureDescriptor;

NTSTATUS HushDataRangeIntersection(_In_ PKSDATARANGE DataRange,
                                   _In_ PKSDATARANGE MatchingDataRange,
                                   _In_ ULONG OutputBufferLength,
                                   _Out_writes_bytes_opt_(OutputBufferLength) PVOID ResultantFormat,
                                   _Out_ PULONG ResultantFormatLength);

BOOLEAN HushIsFixedFormat(_In_ PKSDATAFORMAT DataFormat, _In_ ULONG Size);
