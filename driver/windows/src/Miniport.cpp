// HushCore Virtual Audio: filter descriptors, format rules and the WaveRT and topology miniports.
//
// The four filters are fixed at compile time, in the shape the audio endpoint builder walks:
//   render:  [wave: host IN (0) -> bridge OUT (1)] => [topology: bridge IN (0) -> speaker OUT (1)]
//   capture: [topology: microphone IN (0) -> bridge OUT (1)] => [wave: bridge IN (1) -> host OUT (0)]
// "=>" is a PcRegisterPhysicalConnection. The physical (outermost) topology pin carries the
// endpoint name GUID; the INF maps it to "HushCore Sink" / "HushCore Microphone" under
// MediaCategories.
#include "Common.h"
#include "Endpoints.h"

namespace {

// Endpoint name keys. The INF maps each GUID to its display name under
// HKLM\SYSTEM\CurrentControlSet\Control\MediaCategories.
const GUID g_RenderEndpointName =
    { 0x802beb5d, 0x5857, 0x4c68, { 0x8c, 0x96, 0x26, 0x8a, 0x90, 0xf9, 0x99, 0x8c } };
const GUID g_CaptureEndpointName =
    { 0x9a2675be, 0xefce, 0x4cec, { 0xbc, 0x09, 0x93, 0xcd, 0xbc, 0x80, 0x64, 0x95 } };

// Wave data range: 48 kHz, stereo, 32-bit float. STATIC_ GUIDs are used because the
// static initializers below must be compile-time constants.
KSDATARANGE_AUDIO g_WaveDataRange = {
    { sizeof(KSDATARANGE_AUDIO), 0, 0, 0,
      STATIC_KSDATAFORMAT_TYPE_AUDIO,
      STATIC_KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
      STATIC_KSDATAFORMAT_SPECIFIER_WAVEFORMATEX },
    HushChannels, 32, 32, HushSampleRate, HushSampleRate
};

// Topology bridge range. Bridge pins carry no samples; the wave pins do.
KSDATARANGE g_TopologyDataRange = {
    sizeof(KSDATARANGE), 0, 0, 0,
    STATIC_KSDATAFORMAT_TYPE_AUDIO,
    STATIC_KSDATAFORMAT_SUBTYPE_ANALOG,
    STATIC_KSDATAFORMAT_SPECIFIER_NONE
};

PKSDATARANGE g_WavePinRanges[] = { &g_WaveDataRange.DataRange };
PKSDATARANGE g_TopologyPinRanges[] = { &g_TopologyDataRange };

// The fixed format that DataRangeIntersection returns and SetFormat-equivalent checks accept.
KSDATAFORMAT_WAVEFORMATEXTENSIBLE g_FixedFormat = {
    { sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE), 0, 0, 0,
      STATIC_KSDATAFORMAT_TYPE_AUDIO,
      STATIC_KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
      STATIC_KSDATAFORMAT_SPECIFIER_WAVEFORMATEX },
    { { WAVE_FORMAT_EXTENSIBLE, 2, 48000, 384000, 8, 32, 22 },
      { 32 },
      KSAUDIO_SPEAKER_STEREO,
      STATIC_KSDATAFORMAT_SUBTYPE_IEEE_FLOAT }
};

// Wave filters: pin 0 is the host streaming pin (one instance), pin 1 the bridge to topology.
const PCPIN_DESCRIPTOR g_WaveRenderPins[] = {
    { 1, 1, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_WavePinRanges, KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_SINK,
        &KSCATEGORY_AUDIO, nullptr, 0 } },
    { 0, 0, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_TopologyPinRanges, KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_NONE,
        &KSCATEGORY_AUDIO, nullptr, 0 } }
};

const PCPIN_DESCRIPTOR g_WaveCapturePins[] = {
    { 1, 1, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_WavePinRanges, KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_SINK,
        &KSCATEGORY_AUDIO, nullptr, 0 } },
    { 0, 0, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_TopologyPinRanges, KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_NONE,
        &KSCATEGORY_AUDIO, nullptr, 0 } }
};

// Topology filters: a bridge pin facing the wave filter and the named physical pin.
const PCPIN_DESCRIPTOR g_TopologyRenderPins[] = {
    { 0, 0, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_TopologyPinRanges, KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_NONE,
        &KSCATEGORY_AUDIO, nullptr, 0 } },
    { 0, 0, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_TopologyPinRanges, KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_NONE,
        &KSNODETYPE_SPEAKER, &g_RenderEndpointName, 0 } }
};

const PCPIN_DESCRIPTOR g_TopologyCapturePins[] = {
    { 0, 0, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_TopologyPinRanges, KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_NONE,
        &KSNODETYPE_MICROPHONE, &g_CaptureEndpointName, 0 } },
    { 0, 0, 0, nullptr,
      { 0, nullptr, 0, nullptr,
        1, g_TopologyPinRanges, KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_NONE,
        &KSCATEGORY_AUDIO, nullptr, 0 } }
};

// No nodes: each filter passes straight from its input pin to its output pin.
const PCCONNECTION_DESCRIPTOR g_PinToPinOneWay[] = {
    { PCFILTER_NODE, 0, PCFILTER_NODE, 1 }
};
const PCCONNECTION_DESCRIPTOR g_WaveCaptureConnections[] = {
    { PCFILTER_NODE, 1, PCFILTER_NODE, 0 }
};

const GUID g_WaveRenderCategories[] = {
    { STATIC_KSCATEGORY_AUDIO }, { STATIC_KSCATEGORY_RENDER }, { STATIC_KSCATEGORY_REALTIME }
};
const GUID g_WaveCaptureCategories[] = {
    { STATIC_KSCATEGORY_AUDIO }, { STATIC_KSCATEGORY_CAPTURE }, { STATIC_KSCATEGORY_REALTIME }
};
const GUID g_TopologyCategories[] = {
    { STATIC_KSCATEGORY_AUDIO }, { STATIC_KSCATEGORY_TOPOLOGY }
};

} // namespace

PCFILTER_DESCRIPTOR g_WaveRenderDescriptor = {
    0, nullptr,
    sizeof(PCPIN_DESCRIPTOR), SIZEOF_ARRAY(g_WaveRenderPins), g_WaveRenderPins,
    sizeof(PCNODE_DESCRIPTOR), 0, nullptr,
    SIZEOF_ARRAY(g_PinToPinOneWay), g_PinToPinOneWay,
    SIZEOF_ARRAY(g_WaveRenderCategories), g_WaveRenderCategories
};

PCFILTER_DESCRIPTOR g_WaveCaptureDescriptor = {
    0, nullptr,
    sizeof(PCPIN_DESCRIPTOR), SIZEOF_ARRAY(g_WaveCapturePins), g_WaveCapturePins,
    sizeof(PCNODE_DESCRIPTOR), 0, nullptr,
    SIZEOF_ARRAY(g_WaveCaptureConnections), g_WaveCaptureConnections,
    SIZEOF_ARRAY(g_WaveCaptureCategories), g_WaveCaptureCategories
};

PCFILTER_DESCRIPTOR g_TopologyRenderDescriptor = {
    0, nullptr,
    sizeof(PCPIN_DESCRIPTOR), SIZEOF_ARRAY(g_TopologyRenderPins), g_TopologyRenderPins,
    sizeof(PCNODE_DESCRIPTOR), 0, nullptr,
    SIZEOF_ARRAY(g_PinToPinOneWay), g_PinToPinOneWay,
    SIZEOF_ARRAY(g_TopologyCategories), g_TopologyCategories
};

PCFILTER_DESCRIPTOR g_TopologyCaptureDescriptor = {
    0, nullptr,
    sizeof(PCPIN_DESCRIPTOR), SIZEOF_ARRAY(g_TopologyCapturePins), g_TopologyCapturePins,
    sizeof(PCNODE_DESCRIPTOR), 0, nullptr,
    SIZEOF_ARRAY(g_PinToPinOneWay), g_PinToPinOneWay,
    SIZEOF_ARRAY(g_TopologyCategories), g_TopologyCategories
};

BOOLEAN HushIsFixedFormat(_In_ PKSDATAFORMAT DataFormat, _In_ ULONG Size)
{
    if (DataFormat == nullptr || Size < sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE)) {
        return FALSE;
    }

    // Validate a private copy: the caller's header can change after the check.
    KSDATAFORMAT_WAVEFORMATEXTENSIBLE format;
    RtlCopyMemory(&format, DataFormat, sizeof(format));

    if (format.DataFormat.FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE)) {
        return FALSE;
    }
    if (!IsEqualGUID(format.DataFormat.MajorFormat, KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUID(format.DataFormat.SubFormat, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) ||
        !IsEqualGUID(format.DataFormat.Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return FALSE;
    }

    const WAVEFORMATEX& wfx = format.WaveFormatExt.Format;
    if (wfx.wFormatTag != WAVE_FORMAT_EXTENSIBLE || wfx.cbSize < 22 ||
        wfx.nChannels != HushChannels || wfx.nSamplesPerSec != HushSampleRate ||
        wfx.wBitsPerSample != 32 || wfx.nBlockAlign != HushFrameBytes ||
        wfx.nAvgBytesPerSec != HushSampleRate * HushFrameBytes) {
        return FALSE;
    }
    if (!IsEqualGUID(format.WaveFormatExt.SubFormat, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) ||
        format.WaveFormatExt.dwChannelMask != KSAUDIO_SPEAKER_STEREO) {
        return FALSE;
    }
    return TRUE;
}

NTSTATUS HushDataRangeIntersection(_In_ PKSDATARANGE DataRange,
                                   _In_ PKSDATARANGE MatchingDataRange,
                                   _In_ ULONG OutputBufferLength,
                                   _Out_writes_bytes_opt_(OutputBufferLength) PVOID ResultantFormat,
                                   _Out_ PULONG ResultantFormatLength)
{
    UNREFERENCED_PARAMETER(MatchingDataRange);

    if (DataRange == nullptr || ResultantFormatLength == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    if (DataRange->FormatSize < sizeof(KSDATARANGE_AUDIO)) {
        return STATUS_NO_MATCH;
    }

    const KSDATARANGE_AUDIO* range = reinterpret_cast<const KSDATARANGE_AUDIO*>(DataRange);
    if (!IsEqualGUID(range->DataRange.MajorFormat, KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUID(range->DataRange.SubFormat, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) ||
        !IsEqualGUID(range->DataRange.Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return STATUS_NO_MATCH;
    }
    if (range->MaximumChannels < HushChannels ||
        range->MinimumBitsPerSample > 32 || range->MaximumBitsPerSample < 32 ||
        range->MinimumSampleFrequency > HushSampleRate ||
        range->MaximumSampleFrequency < HushSampleRate) {
        return STATUS_NO_MATCH;
    }

    const ULONG size = sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE);
    *ResultantFormatLength = size;
    if (ResultantFormat == nullptr || OutputBufferLength < size) {
        return STATUS_BUFFER_OVERFLOW;
    }

    RtlCopyMemory(ResultantFormat, &g_FixedFormat, size);
    return STATUS_SUCCESS;
}

// ---- HushWaveMiniport -----------------------------------------------------------------------

HushWaveMiniport::HushWaveMiniport(_In_ HushAdapter* Adapter, _In_ BOOLEAN Capture)
{
    m_RefCount = 1;
    m_Adapter = Adapter;
    m_Capture = Capture;
    m_Adapter->AddRef();
}

HushWaveMiniport::~HushWaveMiniport()
{
    m_Adapter->Release();
}

STDMETHODIMP_(NTSTATUS) HushWaveMiniport::QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface)
{
    if (Interface == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    if (IsEqualGUID(InterfaceId, IID_IUnknown) ||
        IsEqualGUID(InterfaceId, IID_IMiniport) ||
        IsEqualGUID(InterfaceId, IID_IMiniportWaveRT)) {
        *Interface = static_cast<IMiniportWaveRT*>(this);
        AddRef();
        return STATUS_SUCCESS;
    }
    *Interface = nullptr;
    return STATUS_NOINTERFACE;
}

STDMETHODIMP_(ULONG) HushWaveMiniport::AddRef()
{
    return static_cast<ULONG>(InterlockedIncrement(&m_RefCount));
}

STDMETHODIMP_(ULONG) HushWaveMiniport::Release()
{
    const LONG count = InterlockedDecrement(&m_RefCount);
    if (count == 0) {
        delete this;
    }
    return static_cast<ULONG>(count);
}

STDMETHODIMP_(NTSTATUS) HushWaveMiniport::Init(_In_ PUNKNOWN UnknownAdapter,
                                               _In_opt_ PRESOURCELIST ResourceList,
                                               _In_ PPORTWAVERT Port)
{
    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);
    // The endpoint has no hardware resources to claim.
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) HushWaveMiniport::GetDescription(_Out_ PPCFILTER_DESCRIPTOR* Description)
{
    if (Description == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    *Description = m_Capture ? &g_WaveCaptureDescriptor : &g_WaveRenderDescriptor;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) HushWaveMiniport::DataRangeIntersection(_In_ ULONG PinId,
                                                                _In_ PKSDATARANGE DataRange,
                                                                _In_ PKSDATARANGE MatchingDataRange,
                                                                _In_ ULONG OutputBufferLength,
                                                                _Out_writes_bytes_opt_(OutputBufferLength) PVOID ResultantFormat,
                                                                _Out_ PULONG ResultantFormatLength)
{
    // The bridge pin never streams; PortCls handles it with its default intersection.
    if (PinId != 0) {
        return STATUS_NOT_IMPLEMENTED;
    }
    return HushDataRangeIntersection(DataRange, MatchingDataRange, OutputBufferLength,
                                     ResultantFormat, ResultantFormatLength);
}

// There is no DMA hardware; PortCls still asks, and expects a bus-master description.
STDMETHODIMP_(NTSTATUS) HushWaveMiniport::GetDeviceDescription(_Out_ PDEVICE_DESCRIPTION DeviceDescription)
{
    PAGED_CODE();
    if (DeviceDescription == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(DeviceDescription, sizeof(DEVICE_DESCRIPTION));
    DeviceDescription->Master = TRUE;
    DeviceDescription->ScatterGather = TRUE;
    DeviceDescription->Dma32BitAddresses = TRUE;
    DeviceDescription->InterfaceType = PCIBus;
    DeviceDescription->MaximumLength = 0xFFFFFFFF;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) HushWaveMiniport::NewStream(_Out_ PMINIPORTWAVERTSTREAM* Stream,
                                                    _In_ PPORTWAVERTSTREAM PortStream,
                                                    _In_ ULONG Pin,
                                                    _In_ BOOLEAN Capture,
                                                    _In_ PKSDATAFORMAT DataFormat)
{
    UNREFERENCED_PARAMETER(PortStream);

    if (Stream == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    *Stream = nullptr;

    if (Pin != 0 || Capture != m_Capture) {
        return STATUS_INVALID_PARAMETER;
    }
    if (DataFormat == nullptr ||
        !HushIsFixedFormat(DataFormat, DataFormat->FormatSize)) {
        return STATUS_INVALID_PARAMETER;
    }

    HushStream* stream = new (POOL_FLAG_NON_PAGED, HUSH_POOLTAG) HushStream(m_Adapter, m_Capture);
    if (stream == nullptr) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    *Stream = static_cast<PMINIPORTWAVERTSTREAM>(stream);
    return STATUS_SUCCESS;
}

// ---- HushTopologyMiniport --------------------------------------------------------------------

HushTopologyMiniport::HushTopologyMiniport(_In_ HushAdapter* Adapter, _In_ BOOLEAN Capture)
{
    m_RefCount = 1;
    m_Adapter = Adapter;
    m_Capture = Capture;
    m_Adapter->AddRef();
}

HushTopologyMiniport::~HushTopologyMiniport()
{
    m_Adapter->Release();
}

STDMETHODIMP_(NTSTATUS) HushTopologyMiniport::QueryInterface(_In_ REFIID InterfaceId, _Out_ PVOID* Interface)
{
    if (Interface == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    if (IsEqualGUID(InterfaceId, IID_IUnknown) ||
        IsEqualGUID(InterfaceId, IID_IMiniport) ||
        IsEqualGUID(InterfaceId, IID_IMiniportTopology)) {
        *Interface = static_cast<IMiniportTopology*>(this);
        AddRef();
        return STATUS_SUCCESS;
    }
    *Interface = nullptr;
    return STATUS_NOINTERFACE;
}

STDMETHODIMP_(ULONG) HushTopologyMiniport::AddRef()
{
    return static_cast<ULONG>(InterlockedIncrement(&m_RefCount));
}

STDMETHODIMP_(ULONG) HushTopologyMiniport::Release()
{
    const LONG count = InterlockedDecrement(&m_RefCount);
    if (count == 0) {
        delete this;
    }
    return static_cast<ULONG>(count);
}

STDMETHODIMP_(NTSTATUS) HushTopologyMiniport::Init(_In_ PUNKNOWN UnknownAdapter,
                                                   _In_opt_ PRESOURCELIST ResourceList,
                                                   _In_ PPORTTOPOLOGY Port)
{
    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) HushTopologyMiniport::GetDescription(_Out_ PPCFILTER_DESCRIPTOR* Description)
{
    if (Description == nullptr) {
        return STATUS_INVALID_PARAMETER;
    }
    *Description = m_Capture ? &g_TopologyCaptureDescriptor : &g_TopologyRenderDescriptor;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS) HushTopologyMiniport::DataRangeIntersection(_In_ ULONG PinId,
                                                                    _In_ PKSDATARANGE DataRange,
                                                                    _In_ PKSDATARANGE MatchingDataRange,
                                                                    _In_ ULONG OutputBufferLength,
                                                                    _Out_writes_bytes_opt_(OutputBufferLength) PVOID ResultantFormat,
                                                                    _Out_ PULONG ResultantFormatLength)
{
    // Topology pins never stream samples, so the only answer is "no format".
    UNREFERENCED_PARAMETER(PinId);
    UNREFERENCED_PARAMETER(DataRange);
    UNREFERENCED_PARAMETER(MatchingDataRange);
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(ResultantFormat);
    if (ResultantFormatLength != nullptr) {
        *ResultantFormatLength = 0;
    }
    return STATUS_NO_MATCH;
}
