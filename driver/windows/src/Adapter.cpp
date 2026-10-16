// HushCore Virtual Audio: driver entry, AddDevice, StartDevice and subdevice registration.
//
// initguid.h defines the KS and PortCls GUIDs in this translation unit only. Every other
// file refers to them through ksguid.lib, which the project links.
#include <initguid.h>
#include "Common.h"
#include "Endpoints.h"

extern "C" NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath);
extern "C" NTSTATUS HushAddDevice(_In_ PDRIVER_OBJECT DriverObject, _In_ PDEVICE_OBJECT PhysicalDeviceObject);
extern "C" NTSTATUS HushStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                                    _In_opt_ PRESOURCELIST ResourceList);

#pragma alloc_text(INIT, DriverEntry)
#pragma alloc_text(PAGE, HushAddDevice)
#pragma alloc_text(PAGE, HushStartDevice)

extern "C" NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    return PcInitializeAdapterDriver(DriverObject, RegistryPath, HushAddDevice);
}

extern "C" NTSTATUS HushAddDevice(_In_ PDRIVER_OBJECT DriverObject, _In_ PDEVICE_OBJECT PhysicalDeviceObject)
{
    PAGED_CODE();
    return PcAddAdapterDevice(DriverObject, PhysicalDeviceObject,
                              static_cast<PCPFNSTARTDEVICE>(HushStartDevice),
                              HushMaxSubdevices, 0);
}

extern "C" NTSTATUS HushStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                                    _In_opt_ PRESOURCELIST ResourceList)
{
    PAGED_CODE();

    HushAdapter* adapter = new (POOL_FLAG_NON_PAGED, HUSH_POOLTAG) HushAdapter();
    if (adapter == nullptr) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    NTSTATUS status = adapter->Initialize();
    if (NT_SUCCESS(status)) {
        status = HushRegisterEndpoints(DeviceObject, Irp, ResourceList, adapter);
    }

    // Miniports and streams hold their own references, so the ring outlives this call
    // exactly as long as the subdevices that use it.
    adapter->Release();
    return status;
}

namespace {

// Creates one wave port and one topology port, registers both as subdevices, and joins them
// with a physical connection. Render data flows wave -> topology; capture data flows
// topology -> wave.
NTSTATUS RegisterDirection(_In_ PDEVICE_OBJECT DeviceObject,
                           _In_ PIRP Irp,
                           _In_opt_ PRESOURCELIST ResourceList,
                           _In_ HushAdapter* Adapter,
                           _In_ BOOLEAN Capture,
                           _Inout_z_ PWCHAR WaveName,
                           _Inout_z_ PWCHAR TopologyName)
{
    PPORT wavePort = nullptr;
    PPORT topologyPort = nullptr;

    NTSTATUS status = PcNewPort(&wavePort, CLSID_PortWaveRT);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = PcNewPort(&topologyPort, CLSID_PortTopology);
    if (!NT_SUCCESS(status)) {
        wavePort->Release();
        return status;
    }

    HushWaveMiniport* waveMiniport =
        new (POOL_FLAG_NON_PAGED, HUSH_POOLTAG) HushWaveMiniport(Adapter, Capture);
    if (waveMiniport == nullptr) {
        wavePort->Release();
        topologyPort->Release();
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    status = wavePort->Init(DeviceObject, Irp, static_cast<PMINIPORT>(waveMiniport),
                            static_cast<PUNKNOWN>(Adapter), ResourceList);
    waveMiniport->Release();
    if (!NT_SUCCESS(status)) {
        wavePort->Release();
        topologyPort->Release();
        return status;
    }

    HushTopologyMiniport* topologyMiniport =
        new (POOL_FLAG_NON_PAGED, HUSH_POOLTAG) HushTopologyMiniport(Adapter, Capture);
    if (topologyMiniport == nullptr) {
        wavePort->Release();
        topologyPort->Release();
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    status = topologyPort->Init(DeviceObject, Irp, static_cast<PMINIPORT>(topologyMiniport),
                                static_cast<PUNKNOWN>(Adapter), ResourceList);
    topologyMiniport->Release();
    if (!NT_SUCCESS(status)) {
        wavePort->Release();
        topologyPort->Release();
        return status;
    }

    status = PcRegisterSubdevice(DeviceObject, WaveName, static_cast<PUNKNOWN>(wavePort));
    if (NT_SUCCESS(status)) {
        status = PcRegisterSubdevice(DeviceObject, TopologyName, static_cast<PUNKNOWN>(topologyPort));
    }
    if (NT_SUCCESS(status)) {
        // Bridge pins, see the layout at the top of Miniport.cpp.
        if (Capture) {
            status = PcRegisterPhysicalConnection(DeviceObject,
                                                  static_cast<PUNKNOWN>(topologyPort), 1,
                                                  static_cast<PUNKNOWN>(wavePort), 1);
        } else {
            status = PcRegisterPhysicalConnection(DeviceObject,
                                                  static_cast<PUNKNOWN>(wavePort), 1,
                                                  static_cast<PUNKNOWN>(topologyPort), 0);
        }
    }

    // PortCls holds its own references to subdevices that were registered.
    wavePort->Release();
    topologyPort->Release();
    return status;
}

} // namespace

NTSTATUS HushRegisterEndpoints(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                               _In_opt_ PRESOURCELIST ResourceList, _In_ HushAdapter* Adapter)
{
    PAGED_CODE();

    // Subdevice names are writable buffers because PcRegisterSubdevice takes PWCHAR.
    wchar_t renderWave[] = L"WaveRender";
    wchar_t renderTopology[] = L"TopologyRender";
    wchar_t captureWave[] = L"WaveCapture";
    wchar_t captureTopology[] = L"TopologyCapture";

    NTSTATUS status = RegisterDirection(DeviceObject, Irp, ResourceList, Adapter, FALSE,
                                        renderWave, renderTopology);
    if (NT_SUCCESS(status)) {
        status = RegisterDirection(DeviceObject, Irp, ResourceList, Adapter, TRUE,
                                   captureWave, captureTopology);
    }
    return status;
}
