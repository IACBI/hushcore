# HushCore virtual device

HushCore publishes its processed voice as **HushCore Microphone**. The engine writes to
**HushCore Sink**, and HushCore's own platform component loops the sink back into the microphone.
In Discord, OBS, Zoom or any other app, choose *HushCore Microphone* as the input.

| | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Component | kernel WaveRT driver `HushCoreAudio.sys` | Core Audio plug-in `HushCoreAudio.driver` | PipeWire loopback module or PulseAudio null-sink + remap-source |
| Install | `scripts\windows\install-driver.ps1 -AllowTestSigned` (administrator) | `sudo scripts/macos/install-plugin.sh` | `scripts/linux/install-virtual-mic.sh` (no root) |
| Uninstall | `scripts\windows\uninstall-driver.ps1` | `sudo scripts/macos/uninstall-plugin.sh` | `scripts/linux/uninstall-virtual-mic.sh` |
| Without install | not available | not available | created by the engine on start, removed on exit |

The Windows package is test-signed until attestation signing is done, so `-AllowTestSigned` is needed,
and test signing must be on (`bcdedit /set testsigning on`, then reboot; the script never changes it).
Run the installer from an elevated PowerShell in the repository, after building the package with
`scripts\windows\build-driver.ps1`, or from the release archive.

## Installing from a release

Each release publishes one archive per component, plus `SHA256SUMS`. Compare the archive's hash with
its line in `SHA256SUMS` before extracting. Each archive keeps the repository layout, so run the script
from the extracted folder:

| Platform | Archive (`<tag>` is for example `v0.0.99`) | Then |
| --- | --- | --- |
| Windows, engine | `hushcore-engine-<tag>-windows-x64.zip` | copy `hushcore-engine.exe` into `backend\bin` |
| Windows, driver | `hushcore-driver-<tag>-windows-testsigned.zip` | `powershell -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1 -AllowTestSigned` as administrator |
| macOS, engine | `hushcore-engine-<tag>-macos-universal.zip` | copy `hushcore-engine` into `backend/bin` |
| macOS, plug-in | `hushcore-plugin-<tag>-macos-adhoc.zip` | `sudo scripts/macos/install-plugin.sh` |
| Linux, engine | `hushcore-engine-<tag>-linux-x64.zip` | copy `hushcore-engine` into `backend/bin` |
| Linux, virtual microphone | `hushcore-virtual-mic-<tag>-linux.zip` | `scripts/linux/install-virtual-mic.sh` |

The release archives are not signed for public distribution yet; see
[SIGNING.md](SIGNING.md#current-release-state) for the state of each one.

Check the status from a terminal; it exits 0 only when both endpoints are present:

```bash
hushcore-engine virtual-device
```

The dashboard shows the same status next to the output selector, with the install command for your
platform.

## Behaviour

- **Default routing.** With the output left on *Auto*, the engine plays into HushCore Sink when it
  exists. With the input on the system default, it never captures HushCore Microphone (that would
  feed its own output back).
- **Latency.** The virtual device adds one or two of its own periods (Windows: the WASAPI engine
  period, about 10 ms; macOS: two IO buffers; Linux: the PipeWire quantum).
- **Formats.** Windows: 48 kHz stereo float, fixed (Windows converts for apps). macOS: 44.1 or
  48 kHz stereo float; both HushCore devices switch rate together. Linux: whatever the server
  negotiates.
- **Device loss.** If the physical microphone or the sink disappears (unplug, rate change, sleep,
  audio service restart), the engine reports `reconnecting` and reopens the devices with backoff
  (250 ms up to 2 s) until they come back or you stop it. The dashboard shows RECONNECTING.
- **Clock drift.** The physical microphone and HushCore Sink run on different clocks. The engine
  corrects drift one sample at a time and, if the clocks drift apart, grows its safety margin by
  half a capture packet per glitch (at most two glitches per session). Same-clock setups keep the
  minimum latency.

## Known limitations

- **Linux:** `pa_simple_read` does not return when its source is removed, so the engine detects the
  loss with a watchdog (stall plus a presence check every 250 ms) and reconnects. The blocked
  capture thread is abandoned after 1 s and holds one server connection until the server closes it.
- **Windows:** one render stream (the engine) and one capture stream at a time per endpoint in
  this version; a second app opening HushCore Microphone shares it through the Windows audio
  engine in shared mode, which is how apps normally open microphones.
- **All:** drift beyond 0.2 % (a faulty device) is bounded but audible.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| HushCore Microphone missing (Windows) | Device Manager → *Sound, video and game controllers* → *HushCore Virtual Audio*; driver signing (test-signed builds need test signing on and `-AllowTestSigned`) |
| HushCore Microphone missing (macOS) | `ls /Library/Audio/Plug-Ins/HAL`; `log stream --predicate 'process == "coreaudiod"'` |
| HushCore Microphone missing (Linux) | `pactl list short sources`, `pw-cli ls Node`; rerun the install script |
| Apps hear silence | Is HushCore running and is the output *Auto* or *HushCore Sink*? The input meter must move |
| Apps hear robotic or clicking audio | `hushcore-engine selftest 30`: `xruns` should be 0 after the first second |
| macOS: input meter flat | Microphone permission for the terminal or app that started HushCore |
| Flatpak apps (Linux) | they see HushCore Microphone through the PulseAudio/PipeWire portal like any source |

## Manual release checks

These need real hardware or a signed build and are not covered by CI:

1. Windows VM with test signing: install, both endpoints appear in Sound settings, Discord records
   HushCore Microphone, uninstall leaves no device and no `oem*.inf` for HushCore.
2. Unplug and replug the USB microphone while running: `reconnecting`, then `started`, audio resumes.
3. Sleep and resume the machine while running: same as 2.
4. Change the microphone's sample rate in the OS sound settings while running: same as 2.
5. macOS on Apple silicon and Intel: install, record in QuickTime from HushCore Microphone, uninstall.
6. Linux on PipeWire and on PulseAudio: persistent install, reboot, devices still present; uninstall.
7. 30-minute run per platform: `xruns` stays at 0 or at most 2 after the first minute.
