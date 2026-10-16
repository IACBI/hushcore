// HushCore Virtual Audio: PortCls miniports and the WaveRT stream.
#pragma once

#include "Common.h"

// One cyclic WaveRT stream. A render stream copies from its buffer into the ring as its play
// position advances. A capture stream copies from the ring into its buffer as its record
// position advances. Position is derived from KeQueryPerformanceCounter, so the stream
// needs no hardware.
class HushStream final : public IMiniportWaveRTStreamNotification {
public:
    HushStream(_In_ HushAdapter* Adapter, _In_ BOOLEAN Capture);
    ~HushStream();

    STDMETHODIMP_(NTSTATUS) QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    STDMETHODIMP_(NTSTATUS) SetFormat(_In_ PKSDATAFORMAT DataFormat);
    STDMETHODIMP_(NTSTATUS) SetState(_In_ KSSTATE NewState);
    STDMETHODIMP_(NTSTATUS) GetPosition(_Out_ PKSAUDIO_POSITION Position);
    STDMETHODIMP_(NTSTATUS) AllocateAudioBuffer(_In_ ULONG RequestedSize,
                                                _Out_ PMDL* AudioBufferMdl,
                                                _Out_ ULONG* ActualSize,
                                                _Out_ ULONG* OffsetFromFirstPage,
                                                _Out_ MEMORY_CACHING_TYPE* CacheType);
    STDMETHODIMP_(void) FreeAudioBuffer(_In_opt_ PMDL AudioBufferMdl, _In_ ULONG BufferSize);
    STDMETHODIMP_(void) GetHWLatency(_Out_ KSRTAUDIO_HWLATENCY* Latency);
    STDMETHODIMP_(NTSTATUS) GetPositionRegister(_Out_ PKSRTAUDIO_HWREGISTER Register);
    STDMETHODIMP_(NTSTATUS) GetClockRegister(_Out_ PKSRTAUDIO_HWREGISTER Register);

    STDMETHODIMP_(NTSTATUS) AllocateBufferWithNotification(_In_ ULONG NotificationCount,
                                                           _In_ ULONG RequestedSize,
                                                           _Out_ PMDL* AudioBufferMdl,
                                                           _Out_ ULONG* ActualSize,
                                                           _Out_ ULONG* OffsetFromFirstPage,
                                                           _Out_ MEMORY_CACHING_TYPE* CacheType);
    STDMETHODIMP_(void) FreeBufferWithNotification(_In_ PMDL AudioBufferMdl, _In_ ULONG BufferSize);
    STDMETHODIMP_(NTSTATUS) RegisterNotificationEvent(_In_ PKEVENT NotificationEvent);
    STDMETHODIMP_(NTSTATUS) UnregisterNotificationEvent(_In_ PKEVENT NotificationEvent);

    // Called from the high-resolution timer at DISPATCH_LEVEL.
    VOID OnTimer();

private:
    NTSTATUS StartRun();
    VOID StopTimer();
    VOID AdvanceLocked();
    ULONGLONG CurrentFramesLocked() const;
    NTSTATUS AllocateBuffer(_In_ ULONG RequestedSize, _Out_ PMDL* AudioBufferMdl,
                            _Out_ ULONG* ActualSize, _Out_ ULONG* OffsetFromFirstPage,
                            _Out_ MEMORY_CACHING_TYPE* CacheType, _In_ ULONG Period);
    VOID ReleaseBuffer();

    LONG m_RefCount;
    HushAdapter* m_Adapter;
    BOOLEAN m_Capture;

    KSPIN_LOCK m_Lock;
    KSSTATE m_State;
    BOOLEAN m_Running;

    PMDL m_Mdl;
    PUCHAR m_Buffer;
    ULONG m_BufferBytes;
    ULONGLONG m_BufferFrames;

    ULONGLONG m_BaseFrames;      // frames played or recorded before the current RUN
    ULONGLONG m_ProcessedFrames; // absolute frame index already copied to or from the ring
    LARGE_INTEGER m_StartTicks;
    LONGLONG m_TickFrequency;

    ULONG m_PeriodFrames;
    ULONG m_LagFrames;
    PKEVENT m_NotifyEvent;
    ULONGLONG m_LastNotifyIndex;

    PEX_TIMER m_Timer;
};

// WaveRT miniport: one streaming pin, one stream at a time, fixed format.
class HushWaveMiniport final : public IMiniportWaveRT {
public:
    HushWaveMiniport(_In_ HushAdapter* Adapter, _In_ BOOLEAN Capture);
    ~HushWaveMiniport();

    STDMETHODIMP_(NTSTATUS) QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    STDMETHODIMP_(NTSTATUS) Init(_In_ PUNKNOWN UnknownAdapter,
                                 _In_opt_ PRESOURCELIST ResourceList,
                                 _In_ PPORTWAVERT Port);
    STDMETHODIMP_(NTSTATUS) GetDescription(_Out_ PPCFILTER_DESCRIPTOR* Description);
    STDMETHODIMP_(NTSTATUS) DataRangeIntersection(_In_ ULONG PinId,
                                                  _In_ PKSDATARANGE DataRange,
                                                  _In_ PKSDATARANGE MatchingDataRange,
                                                  _In_ ULONG OutputBufferLength,
                                                  _Out_writes_bytes_opt_(OutputBufferLength) PVOID ResultantFormat,
                                                  _Out_ PULONG ResultantFormatLength);
    STDMETHODIMP_(NTSTATUS) GetDeviceDescription(_Out_ PDEVICE_DESCRIPTION DeviceDescription);
    STDMETHODIMP_(NTSTATUS) NewStream(_Out_ PMINIPORTWAVERTSTREAM* Stream,
                                      _In_ PPORTWAVERTSTREAM PortStream,
                                      _In_ ULONG Pin,
                                      _In_ BOOLEAN Capture,
                                      _In_ PKSDATAFORMAT DataFormat);

private:
    LONG m_RefCount;
    HushAdapter* m_Adapter;
    BOOLEAN m_Capture;
};

// Topology miniport: one bridge pin that names the endpoint (see MediaCategories in the INF).
class HushTopologyMiniport final : public IMiniportTopology {
public:
    HushTopologyMiniport(_In_ HushAdapter* Adapter, _In_ BOOLEAN Capture);
    ~HushTopologyMiniport();

    STDMETHODIMP_(NTSTATUS) QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    STDMETHODIMP_(NTSTATUS) Init(_In_ PUNKNOWN UnknownAdapter,
                                 _In_opt_ PRESOURCELIST ResourceList,
                                 _In_ PPORTTOPOLOGY Port);
    STDMETHODIMP_(NTSTATUS) GetDescription(_Out_ PPCFILTER_DESCRIPTOR* Description);
    STDMETHODIMP_(NTSTATUS) DataRangeIntersection(_In_ ULONG PinId,
                                                  _In_ PKSDATARANGE DataRange,
                                                  _In_ PKSDATARANGE MatchingDataRange,
                                                  _In_ ULONG OutputBufferLength,
                                                  _Out_writes_bytes_opt_(OutputBufferLength) PVOID ResultantFormat,
                                                  _Out_ PULONG ResultantFormatLength);

private:
    LONG m_RefCount;
    HushAdapter* m_Adapter;
    BOOLEAN m_Capture;
};
