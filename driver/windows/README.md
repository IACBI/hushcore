# HushCore Virtual Audio (Windows driver)

A PortCls WaveRT audio driver that gives HushCore its own virtual microphone, so no third-party
virtual audio software is needed. The engine writes to **HushCore Sink** (render) and applications read from
**HushCore Microphone** (capture). The driver copies frames between the two.

Status: the code is written but **not compiled locally** (the WDK is not installed on the
development machine). It is compiled and InfVerif-checked by CI on `windows-2022`. See
"Not verified" below.

## Files

| File | Purpose |
| --- | --- |
| `src/Adapter.cpp` | `DriverEntry`, `AddDevice`, `StartDevice`; creates the shared adapter, the four subdevices and the two physical connections |
| `src/Common.h` | Constants (fixed format, ring size), pool helpers, `HushRing` and `HushAdapter` declarations |
| `src/Ring.cpp` | Loopback ring (non-paged, spin-locked) and the shared adapter's `IUnknown` |
| `src/Endpoints.h` | Declarations for `HushStream`, `HushWaveMiniport`, `HushTopologyMiniport` |
| `src/Stream.cpp` | WaveRT stream: position, high-resolution timer, buffer and notification handling |
| `src/Miniport.cpp` | Pin, filter and data-range descriptors; format check; the two miniport classes |
| `HushCoreAudio.inf` | Driver package: KS and WDMAUDIO registration, interfaces, MediaCategories names, service |
| `HushCoreAudio.vcxproj`, `HushCoreAudio.sln` | WDK project (x64 and ARM64, Debug and Release) |

Install, uninstall and build scripts are in `scripts/windows/`.

## Architecture

```
 engine ──WaveRender stream──► ring (16384 frames, non-paged) ──WaveCapture stream──► apps
          TopologyRender "HushCore Sink"                        TopologyCapture "HushCore Microphone"
```

- **Subdevices.** `WaveRender`, `TopologyRender`, `WaveCapture`, `TopologyCapture`, registered in
  `StartDevice`. Each filter has two pins. Render: wave host IN (0) and bridge OUT (1) connect to
  topology bridge IN (0) and speaker OUT (1). Capture is the mirror image. The physical connections
  join the bridge pins: wave 1 → topology 0 for render, topology 1 ↔ wave 1 for capture.
- **Endpoint names.** The outermost topology pin carries a `Name` GUID: the speaker OUT pin on render,
  the microphone IN pin on capture. The INF maps those GUIDs under
  `HKLM\SYSTEM\CurrentControlSet\Control\MediaCategories` to "HushCore Sink" and
  "HushCore Microphone". The render category is `KSNODETYPE_SPEAKER`; the capture category is
  `KSNODETYPE_MICROPHONE`.
- **Format.** Pins and `DataRangeIntersection` accept only 48 kHz, 2 channels, 32-bit IEEE float,
  `WAVEFORMATEXTENSIBLE`, `KSAUDIO_SPEAKER_STEREO`. `NewStream` rejects any other format. There is
  no conversion in the driver.
- **Loopback ring.** 16384 frames (8 bytes each, 128 KiB), allocated with `ExAllocatePool2`
  (`POOL_FLAG_NON_PAGED`, tag `eroH`) and guarded by a `KSPIN_LOCK`. 16384 frames is 341 ms, which
  covers the 200 ms requirement. Policy:
  - The writer (render) never overruns unread data by more than the ring size. If it would, the
    reader is moved forward and an overrun is counted.
  - The reader (capture) never passes the writer. Missing data is replaced with silence and a
    partial read is counted as an underrun.
  - Capture start positions the reader 20 ms behind the writer (or two notification periods,
    whichever is larger).
  - Render stop or pause drains the ring, so capture hears silence when no render stream runs.
- **Timing.** Position is `base + elapsed_QPC * 48000 / QPC_frequency`, computed with
  integer arithmetic only. It is recorded from `KSSTATE_RUN`; PAUSE preserves the accumulated base.
  A 1 ms high-resolution timer (`ExAllocateTimer` with `EX_TIMER_HIGH_RESOLUTION`) copies the frames
  that have elapsed since the last tick. `GetPosition` also advances the stream, so polling readers
  see current data. It reports `PlayOffset` and `WriteOffset` as byte offsets inside the cyclic
  buffer (wrapped by its size). The timer is allocated on RUN and deleted, waiting for callbacks, on
  PAUSE, STOP and buffer free.
- **Limits.** One render stream and one capture stream at a time (`MaxGlobal`/`MaxFilter` pin
  instance counts = 1). The ring has a single reader position, so a second capture reader would
  steal frames. Multi-reader support is a later change. Cyclic buffers are capped at 4 MiB.
- **IRQL.** Only the ring and the stream's `m_Lock` are touched at DISPATCH_LEVEL. Initialization
  code (`DriverEntry`, `HushAddDevice`, `HushStartDevice`) is in the INIT/PAGE sections; everything
  the timer touches is non-paged. Pool allocation is zeroed. Every failure path releases what it has
  acquired.
- **No IOCTLs, no device interfaces, no user buffers.** The only interfaces are the KS categories
  `KSCATEGORY_AUDIO`, `_RENDER`, `_CAPTURE`, `_REALTIME` and `_TOPOLOGY`. There are no KS property
  handlers; no volume or mute nodes.

## Build

Requirements: Visual Studio 2022 with the Windows Driver Kit extension (WDK 10.0.26100).

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\build-driver.ps1 -Configuration Release -Platform x64
```

Equivalent raw command (CI uses this):

```bat
msbuild driver\windows\HushCoreAudio.vcxproj /p:Configuration=Release /p:Platform=x64
```

Outputs:
- MSBuild writes to `build\driver\windows\<Platform>\<Configuration>\`.
- `build-driver.ps1` copies `HushCoreAudio.sys`, `.inf` and `.cat` to `dist\driver\windows\<Platform>\`,
  prints their SHA-256 hashes, and writes the manifest `HushCoreAudio.sha256` beside them. The manifest
  has one line per file, in the form `<SHA256>  <file name>`.

`build\` and `dist\` must be git-ignored. The root `.gitignore` is not owned by this change.

## Test-signing workflow (developer VM)

Only run these yourself, from an elevated shell. The scripts never change boot settings.

1. `bcdedit /set testsigning on`, then reboot.
2. Build (above), then from an elevated PowerShell:
   `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1 -AllowTestSigned`
   The installer checks the package first (see "Package integrity" below), warns if test signing is
   off, and prints the command above. Exit codes are listed in the script header.
3. Verify (see below).
4. Uninstall: `scripts\windows\uninstall-driver.ps1` (elevated). It is idempotent.
5. Optional, to restore: `bcdedit /set testsigning off`, then reboot.

The CI runner must have a code-signing certificate that the WDK TestSign mode can use. This is set
up in the CI steps below.

### Package integrity

`install-driver.ps1` refuses to call pnputil unless the package passes both checks:

- **Hash manifest.** Every file in the folder (`HushCoreAudio.inf`, `.sys`, `.cat`) must match its
  SHA-256 entry in `HushCoreAudio.sha256`. A missing manifest, a malformed or duplicate line, a
  missing entry, or a mismatch exits 7 with the file name.
- **Catalog signature.** `Get-AuthenticodeSignature` runs on `HushCoreAudio.cat`. A status of
  `NotSigned`, `HashMismatch` or `Incompatible` exits 7. A catalog that is not `Valid`, or whose
  signer subject contains "test", counts as test-signed or untrusted. The installer prints the status
  and signer, then exits 7 unless `-AllowTestSigned` is given. With the switch it continues and says
  so. A `Valid` catalog from a non-test signer is accepted without the switch.

The manifest detects copy errors and changes made after the build. The catalog signature is the
trust anchor: someone who can rewrite the package and its manifest can still change the files, but
the signature check then fails unless that signer is trusted on the machine.

## Verify

1. Open **Settings → System → Sound**. "HushCore Sink" should appear under output devices and
   "HushCore Microphone" under input devices.
2. Set the default output to HushCore Sink. Play a tone in the engine, or run
   `hushcore-engine selftest 10`, and check that the **Microphone** input meter in another app (for
   example Voice Recorder) moves. `hushcore-engine selftest 10` should report `xruns` = 0 after the
   first second.
3. `hushcore-engine virtual-device` should report the device as installed.
4. Uninstall, then check that no "HushCore" endpoint, device (`Get-PnpDevice` with the hardware ID
   `ROOT\HushCoreVirtualAudio`), driver package (`Get-WindowsDriver -Online -All`, entries whose
   `OriginalFileName` ends with `HushCoreAudio.inf`), or endpoint name key remains:
   `HKLM\SYSTEM\CurrentControlSet\Control\MediaCategories\{802BEB5D-5857-4C68-8C96-268A90F9998C}` and
   `{9A2675BE-EFCE-4CEC-BC09-93CDBC806495}`.

## Known limitations

- Single render stream, single capture stream (see Limits).
- Position is a byte offset wrapped to the cyclic buffer size, reported as `PlayOffset` and
  `WriteOffset`. The audio engine must accept that (see Not verified).
- Latency is roughly the ring fill, 20 ms in steady state, plus the engine's own buffering.
  `GetHWLatency` reports zero because there is no hardware FIFO.
- `DriverVer` in the INF is a literal (`01/01/2026,0.0.2.0`). The build stamps it through the WDK
  StampInf step, which is expected but not verified. If the built `.inf` still shows the literal, the
  version is not stamped and the installer's `pnputil` still works.
- The uninstaller finds driver packages through `Get-WindowsDriver`, which does not depend on the
  display language. The installer and uninstaller print `pnputil` output as-is and do not parse it.
- Endpoint names are fixed at build time. Changing them needs a new INF and a reinstall.

## Not verified

Nothing in this driver has been compiled or loaded. The following are unverified, by item:

- **PortCls interface shapes.** `IMiniportWaveRT::Init`, `NewStream`, `GetDeviceDescription`,
  `IMiniportWaveRTStreamNotification` (`AllocateBufferWithNotification(NotificationCount, RequestedSize, ...)`,
  `FreeBufferWithNotification`, `RegisterNotificationEvent`, `UnregisterNotificationEvent`) and the
  `IMiniportWaveRTStream` method list (`SetFormat`, `SetState`, `GetPosition(KSAUDIO_POSITION*)`,
  `AllocateAudioBuffer`, `FreeAudioBuffer`, `GetHWLatency`, `GetPositionRegister`, `GetClockRegister`)
  were written from memory of the WDK `portcls.h`. Any missing pure virtual or parameter mismatch shows
  up as a compile error in `src/Stream.cpp` and `src/Endpoints.h`. Check the installed `portcls.h` first.
- **`ExDeleteTimer`.** Called with three arguments (`timer, TRUE, TRUE`). Check whether the WDK
  declares a fourth parameter.
- **`ExAllocateTimer` / `ExSetTimer`.** Used as `(callback, context, EX_TIMER_HIGH_RESOLUTION)` and
  `(timer, dueTime, period, nullptr)`.
- **`MmAllocatePagesForMdl`, `MmMapLockedPagesSpecifyCache`, `MmFreePagesFromMdl`, and
  `ExFreePool(mdl)`.** The MDL free pattern follows the public samples.
- **Static GUID macros.** `STATIC_KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`, `STATIC_KSCATEGORY_REALTIME`,
  `STATIC_KSINTERFACE_STANDARD_LOOPED_STREAMING`, `STATIC_KSMEDIUM_STANDARD_DEVIO`,
  `STATIC_KSDATAFORMAT_SUBTYPE_ANALOG` and `STATIC_KSDATAFORMAT_SPECIFIER_NONE` are assumed to exist.
- **`WAVEFORMATEXTENSIBLE` visibility.** Assumed to come in through `ksmedia.h`.
- **Topology physical connections.** The bridge pin indices (wave 1 and topology 0 for render,
  topology 1 and wave 1 for capture) are assumed correct for `PcRegisterPhysicalConnection`.
- **Sample-rate support.** Only 48 kHz is advertised. Windows shared-mode resampling is expected to
  handle other app formats.
- **Endpoint naming.** The "HushCore Sink (HushCore Virtual Audio)" form depends on PortCls and
  MMDevices behaving as described.
- **`PnpLockdown`, `Inf2Cat` property names, `DriverSign.SignMode`.** Set in the INF and vcxproj as
  the WDK documents them; not checked against the installed WDK.
- **InfVerif.** Not run. Needs `infverif /w` on CI.
- **Runtime behavior.** The timer cadence, the notification contract, the audio engine's use of
  `GetPosition` (wrapped byte offsets), and the lack of position/clock registers have not been tested.
  Those need a test-signed VM.

## CI steps (windows-2022)

Run these in order on `windows-2022` (VS 2022 and WDK 10.0.26100 are preinstalled, or come from the
WDK extension).

```powershell
# 1. Test signing certificate for TestSign mode (used by the msbuild signing step).
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=WDKTestCert HushCore" `
    -CertStoreLocation Cert:\LocalMachine\My -KeyUsage DigitalSignature -NotAfter (Get-Date).AddYears(2)
Export-Certificate -Cert $cert -FilePath "$env:RUNNER_TEMP\hushcore-test.cer" | Out-Null
Import-Certificate -FilePath "$env:RUNNER_TEMP\hushcore-test.cer" -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath "$env:RUNNER_TEMP\hushcore-test.cer" -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null

# 2. Build x64 and ARM64, Release.
msbuild driver\windows\HushCoreAudio.vcxproj /p:Configuration=Release /p:Platform=x64
msbuild driver\windows\HushCoreAudio.vcxproj /p:Configuration=Release /p:Platform=ARM64

# 3. Stage the packages and write HushCoreAudio.sha256 (no elevation needed).
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\build-driver.ps1 -Platform x64
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\build-driver.ps1 -Platform ARM64

# 4. InfVerif on both packages.
$infverif = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Tools\*\x64\infverif.exe" | Sort-Object FullName | Select-Object -Last 1
& $infverif.FullName /w /u "dist\driver\windows\x64\HushCoreAudio.inf"
& $infverif.FullName /w /u "dist\driver\windows\ARM64\HushCoreAudio.inf"

# 5. Static analysis (optional, recommended): run staticdv or the Code Analysis with the WDK rules
#    on HushCoreAudio.vcxproj (EnableMicrosoftCodeAnalysis) before any release.
```

Expected: the builds produce `.sys`, `.inf` and `.cat`; `infverif` exits 0 for both. Installing and
running on a VM remains manual (see Verify).
