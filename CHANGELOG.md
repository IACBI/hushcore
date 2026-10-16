# Changelog

All notable changes to HushCore are listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.0.99] - 2026-10-16

### Added

- **HushCore's own virtual microphone** on Windows, macOS and Linux, replacing VB-Cable. The engine
  writes to **HushCore Sink** and applications record **HushCore Microphone**.
  - Windows: PortCls WaveRT kernel driver for x64 and ARM64, installed with `scripts\windows\install-driver.ps1`.
  - macOS: Core Audio AudioServerPlugIn, installed with `scripts/macos/install-plugin.sh`.
  - Linux: PipeWire or PulseAudio loopback, installed with `scripts/linux/install-virtual-mic.sh` without
    root, or created by the engine while it runs.
- **Cross-platform native engine** (`hushcore-engine`): Core Audio on macOS and PulseAudio/PipeWire on
  Linux, next to WASAPI on Windows. The macOS build is a universal binary.
- **Automatic reconnect.** Unplugging a device, a sample-rate change, sleep and resume, or an audio
  service restart reopens the devices with backoff (250 ms doubling to 2 s). The engine reports
  `reconnecting`, and the dashboard shows RECONNECTING.
- **Virtual-device status** in the `devices` event, `hushcore-engine virtual-device` (exit code 0 only
  when installed) and `GET /api/virtual-device`. The dashboard shows the install command for the current
  platform.
- Engine diagnostics `probe`, `devices` and `selftest <seconds> [shared|exclusive] [output]`.
- **CI for every platform**: engine build, tests and clippy on Windows, macOS and Linux; Windows driver
  build for x64 and ARM64 with InfVerif; macOS plug-in install and loopback test; Linux end-to-end test
  in Docker; backend tests on Python 3.10 and 3.12; dashboard build; a check that no source refers to
  third-party virtual audio software.
- **Release workflow**: engine archives for each platform, the Windows driver and macOS plug-in
  packages, the Linux virtual-microphone files, `SHA256SUMS`, and a draft GitHub release. A manual run
  builds everything without publishing.
- Documentation: architecture, building, virtual device, signing, security, benchmarks, the engine
  control protocol, and a README for each driver.

### Changed

- **Drift-robust ring policy** in the shared engine core. Decisions use the margin of queued frames on a
  smoothed fill. Clock drift is corrected one sample at a time, and the safety cushion grows only after
  underruns, after a 2 s warm-up, up to one capture packet. In the 10-minute simulations, a same-clock
  pair has no underruns, and 300 ppm of drift gives at most two underruns per run (see
  [docs/BENCHMARKS.md](docs/BENCHMARKS.md)).
- The latency estimate uses the time-averaged queue depth instead of the depth before the callback's
  take, which counted the same frames twice.
- Control protocol: the `devices` event carries `virtual_device`, the `started` event reports
  `shared-low-latency` for WASAPI shared mode, and the `reconnecting` event is new. Command lines longer
  than 64 KiB are rejected with a non-fatal `error`.
- The dashboard shows virtual-device status and the platform's install command instead of cable hints.
- Input never captures HushCore Microphone by default, and the default output is HushCore Sink when it is
  installed.

### Fixed

- A monitor failure could hide a failure of the main stream; failures now accumulate.
- A processor overload on macOS disabled monitoring.
- Reconnect backoff restarted after every device loss. It now restarts only after 2 s of stable running.
- Windows uninstall worked only with English system output and left the endpoint name keys behind.

### Security

- Windows driver install checks the SHA-256 manifest and the catalog signature before `pnputil` runs.
  Test-signed packages need `-AllowTestSigned`.
- macOS plug-in install refuses bundles that are writable by others or owned by someone else, stages the
  copy in a root-owned folder, and verifies its signature before it goes live.
- Driver: buffer size capped at 4 MiB, format validated on a local copy, MDL always released, `PnpLockdown=1`.
- Engine: command lines capped at 64 KiB; NaN and infinite settings are replaced by safe values.
- Backend: the per-run API key is compared in constant time for HTTP and WebSocket; Host and Origin
  checks; CSP with a per-response nonce; at most four unauthenticated WebSockets (close code 1013); VST3
  paths must be absolute `.vst3` files; atomic file writes.
- The static review and its open items are in [docs/SECURITY.md](docs/SECURITY.md). The kernel driver and
  the plug-in have not yet run under Driver Verifier.

### Known limitations

- The Windows driver builds and passes InfVerif in CI, but it has not been loaded on a real machine. It is
  test-signed; attestation signing is not done.
- The macOS plug-in is installed and loop-tested only on a CI runner. It is ad-hoc signed and not notarized,
  and it has not been run on a physical Mac.
- Release engine binaries are not signed or notarized.
- Linux: when the input disappears, the blocked capture thread is abandoned after one second. The
  PulseAudio install path and the systemd restart path are not exercised in CI.
- Windows: one render stream and one capture stream at a time per virtual endpoint.
- Clock drift beyond about 0.2 % (a faulty device) stays bounded but is audible.
- HQ AI runs only at 48 kHz.

### Removed

- Use of VB-Cable and other third-party virtual audio software. Nothing in the code, defaults or docs
  requires it any more.

## [0.0.2] - 2026-10-16

### Added

- Native Rust audio engine (`engine/`) with a JSON-lines control protocol ([engine/PROTOCOL.md](engine/PROTOCOL.md)).
  The Python engine stays as a fallback.
- WASAPI exclusive low-latency mode on Windows, with a **Low-latency exclusive mode** switch in the dashboard.
- Engine selector: `HUSHCORE_ENGINE` (`auto`, `native` or `python`) and a choice in the dashboard's Advanced view.

### Changed

- The Python engine is used for VST3 hosting, and whenever the native engine binary is missing.

## [0.0.1] - 2026-10-16

### Added

- Initial release: a Python audio engine (sounddevice), a FastAPI backend and a web dashboard.
- Noise suppression in ECO DSP and HQ AI (DeepFilterNet, optional) modes.
- Studio FX rack: noise gate, 3-band EQ, compressor, de-esser, harmonic vocal exciter and peak limiter.
- VST3 host with a plug-in editor window.
- Built-in and user presets; manual latency buffer; automatic sample-rate negotiation (48 kHz or 44.1 kHz);
  device list refresh.
- Spectrum, pulse and 3D cascade visualizers, a tray launcher, and an API key for the dashboard.
- Packaging, CI and a bilingual README.
