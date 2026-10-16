# HushCore Virtual Audio (Windows driver)

A PortCls WaveRT audio driver that gives HushCore its own virtual microphone, so no third-party
virtual audio software is needed. The engine writes to **HushCore Sink** (render) and applications read from
**HushCore Microphone** (capture). The driver copies frames between the two.

Status: the driver builds in CI for **x64 and ARM64** (Release, test-signed), and `infverif` passes on
both packages (`windows-driver` job in `.github/workflows/ci.yml`). It has **not been loaded on a real
Windows machine**, so the audio path through it is unverified. See "Not verified" below.

## Files

| File | Purpose |
| --- | --- |
| `src/Adapter.cpp` | `DriverEntry`, `AddDevice`, `StartDevice`; creates the shared adapter, the four subdevices and the two physical connections |
| `src/Common.h` | Constants (fixed format, ring size, buffer cap), pool helpers, `HushRing` and `HushAdapter` declarations |
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
  `StartDevice`. Each filter has two pins, and the pins are joined through bridge pins.
  Render: wave host IN (0) and bridge OUT (1) connect to topology bridge IN (0) and speaker OUT (1).
  Capture is the mirror image. The physical connections join the bridge pins: wave 1 → topology 0 for
  render, topology 1 ↔ wave 1 for capture.
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
- **Driver settings.** `PnpLockdown=1` in the INF. The project builds with `/kernel`, `SDLCheck`,
  `WarningLevel Level4` and `TreatWarningAsError`.

## Build

Requirements: Visual Studio 2022 with the Windows Driver Kit extension (WDK 10.0.26100).

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\build-driver.ps1 -Configuration Release -Platform x64
```

`-Platform` accepts `x64` (default) and `ARM64`. The script runs MSBuild on the project; the raw
command is:

```bat
msbuild driver\windows\HushCoreAudio.vcxproj /p:Configuration=Release /p:Platform=x64
```

Outputs:
- MSBuild writes the WDK package to `build\driver\windows\<Platform>\<Configuration>\HushCoreAudio\`.
- `build-driver.ps1` copies `HushCoreAudio.sys`, `.inf` and `.cat` to `dist\driver\windows\<Platform>\`,
  prints their SHA-256 hashes, and writes the manifest `HushCoreAudio.sha256` beside them. The manifest
  has one line per file, in the form `<SHA256>  <file name>`.

Project settings that matter for packaging:
- `SkipPackageVerification=true`. The WDK's build-time package verifier cannot load `InfVerif.dll` on
  hosted runners, so CI runs `infverif.exe /w /u` on the staged `.inf` for both platforms instead.
- inf2cat target OS list: `10_X64` for x64, `10_RS3_ARM64` for ARM64 (Windows 10 on ARM64 starts at 1709).
- `ApiValidator` is disabled for ARM64 only: its analyzer is an x86 tool that cannot load ARM64
  binaries on x64 build hosts. The x64 build validates the same source.
- `FilesToPackage` includes the built `.sys`; the WDK assembles the `.inf` and `.cat`.

`build\` and `dist\` are git-ignored by the root `.gitignore`.

## Test-signing workflow (developer VM)

Only run these yourself, from an elevated shell. The scripts never change boot settings.

1. `bcdedit /set testsigning on`, then reboot.
2. Build (above), then from an elevated PowerShell:
   `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1 -AllowTestSigned`
   The installer checks the package first (see "Package integrity" below), warns if test signing is
   off, and prints the command above. Exit codes are listed in the script header.
3. Verify (see below).
4. Uninstall: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\uninstall-driver.ps1`
   from an elevated shell. It is idempotent.
5. Optional, to restore: `bcdedit /set testsigning off`, then reboot.

Packages from `build-driver.ps1` and from the release archive are test-signed, so `-AllowTestSigned` is
required until attestation signing is done (see [docs/SIGNING.md](../../docs/SIGNING.md)).

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
- `DriverVer` in the INF is `10/16/2026,0.0.99.0`. If the WDK StampInf step rewrites it during the
  build, the packaged value is the one `pnputil` reads; that behaviour is not checked.
- The uninstaller finds driver packages through `Get-WindowsDriver`, which does not depend on the
  display language. The installer and uninstaller print `pnputil` output as-is and do not parse it.
- Endpoint names are fixed at build time. Changing them needs a new INF and a reinstall.

## Not verified

The code compiles and passes `infverif` in CI (x64 and ARM64). Nothing else has been checked:

- **Loading.** The package has not been installed or loaded on a Windows machine or VM. No device
  has been created by the installer and no endpoint has been seen in Sound settings.
- **Runtime behaviour.** The timer cadence, the notification contract, the engine's use of
  `GetPosition` (wrapped byte offsets) and the lack of position/clock registers have not been tested.
- **Endpoint naming.** The "HushCore Sink (HushCore Virtual Audio)" form depends on PortCls and
  MMDevices behaving as described.
- **Topology connections.** The bridge pin indices (wave 1 and topology 0 for render, topology 1 and
  wave 1 for capture) are unverified at runtime, though they compile.
- **Sample rates.** Only 48 kHz is advertised. Windows shared-mode resampling is expected to handle
  other app formats; this is not tested.
- **Kernel tooling.** Driver Verifier, Static Driver Verifier and the WDK audio WHQL tests have not
  been run (see [docs/SECURITY.md](../../docs/SECURITY.md)).
- **Signing.** Test-signed only. Attestation signing is not done.

## CI steps (windows-2022)

The `windows-driver` job in `.github/workflows/ci.yml` does, in order:

1. Creates a self-signed code-signing certificate and trusts it for test signing.
2. Runs `scripts\windows\build-driver.ps1 -Platform x64`, then `-Platform ARM64`.
3. Runs `infverif.exe /w /u` on `dist\driver\windows\x64\HushCoreAudio.inf` and
   `dist\driver\windows\ARM64\HushCoreAudio.inf` (the job fails on a non-zero exit).
4. Uploads `dist\driver\windows\` as the `hushcore-driver-windows-testsigned` artifact.

The release workflow runs the same builds, then zips `scripts\windows\install-driver.ps1`,
`scripts\windows\uninstall-driver.ps1` and `dist\driver\windows` into
`hushcore-driver-<tag>-windows-testsigned.zip`, keeping the repository layout that the scripts expect.
