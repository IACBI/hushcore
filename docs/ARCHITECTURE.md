# HushCore architecture

HushCore processes a physical microphone in real time and publishes the result as its own
virtual microphone, **HushCore Microphone**, on Windows, macOS and Linux. No third-party virtual
audio software (VB-Cable, Voicemeeter, BlackHole, Loopback, …) is needed or used.

```
 physical mic ──► hushcore-engine (capture → DSP chain → ring) ──► HushCore Sink ─┐
                     ▲ JSON lines (stdin/stdout)                                    │ loopback owned by
                     │                                                              │ HushCore's own
                  backend (FastAPI, 127.0.0.1) ◄── dashboard                        │ platform component
                                                                                    ▼
                                   Discord / OBS / Zoom ◄── HushCore Microphone ◄───┘
```

## One core, three platform backends

| Layer | Shared? | Location |
| --- | --- | --- |
| DSP chain, settings, metrics | shared, unchanged | `engine/src/dsp`, `engine/src/settings.rs` |
| Control protocol | shared | `engine/src/ipc.rs`, `engine/PROTOCOL.md` |
| Engine core: controller, counters, ring policy, capture processor, render planner, status loop, supervisor | shared | `engine/src/audio/core/` |
| Device I/O | per platform | `engine/src/audio/wasapi/` (Windows), `engine/src/audio/coreaudio/` (macOS), `engine/src/audio/pulse/` (Linux) |
| Virtual device | per platform | `driver/windows/` (kernel WaveRT driver), `driver/macos/` (AudioServerPlugIn), `driver/linux/` (PipeWire/PulseAudio config) |
| Backend, dashboard | shared | `backend/`, `frontend/` |

Every platform backend exports the same surface, consumed only by `engine/src/main.rs`:

```rust
pub struct PlatformInit;                 // per-thread runtime init (COM on Windows, no-op elsewhere)
pub fn list_devices() -> Result<DeviceList>;
pub fn virtual_device(devices: &DeviceList) -> VirtualDeviceStatus;
pub fn probe() -> Result<Vec<String>>;
pub struct Session;                      // start / set_monitor / diagnostics / stop
```

Backends feed audio through `core::Processor` (capture side: gain, chain, ring push, metrics) and
drain it through `core::RingRenderer` (render side: overflow discard, drift slew, underrun
padding). Those two types hold the real-time policy, so the drift and xrun behaviour is identical
on every platform and covered by the same unit tests.

## Virtual device per platform

All three use the same shape: a **render endpoint the engine writes to** (`HushCore Sink`) and a
**capture endpoint apps read from** (`HushCore Microphone`), joined by a loopback owned by
HushCore's own component. The engine therefore uses plain, already-proven output paths on every
OS, and no custom kernel/user-space channel exists to attack.

| | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Mechanism | PortCls **WaveRT** kernel driver, root-enumerated (`ROOT\HushCoreVirtualAudio`) | **AudioServerPlugIn** loaded by `coreaudiod` | PipeWire `libpipewire-module-loopback` drop-in, or PulseAudio `module-null-sink` + `module-remap-source` |
| Official API | WDK PortCls / KS, INF | Core Audio `AudioServerPlugIn.h` | PipeWire / PulseAudio modules |
| Privilege | kernel; admin to install | runs sandboxed in `coreaudiod`; admin to install | none (user session) |
| Format | 48 kHz, stereo, 32-bit float, fixed | 44.1 / 48 kHz, stereo, 32-bit float | server-converted; engine writes mono f32 |
| Clock | QPC-driven position; loopback ring absorbs drift | host-clock zero-timestamps, one clock for both devices | graph clock (PipeWire) |
| Install | `scripts/windows/install-driver.ps1` (pnputil + SetupAPI) | `scripts/macos/install-plugin.sh` | `scripts/linux/install-virtual-mic.sh` |
| Signing | attestation (Partner Center) for release; test-signing for dev | Developer ID + notarization | none |

The engine detects the virtual device (`virtual_device` in the `devices` event) and, when the
output request is empty, prefers `HushCore Sink`. On Linux, when the persistent config is not
installed, the engine creates the pair on demand through `pactl` and removes it when it quits;
pairs left behind by a crash are found by name and reused, never duplicated.

## Failure handling

| Event | Behaviour |
| --- | --- |
| Device unplugged, invalidated, sample-rate change, sleep/resume, audio service restart | the backend reports a device failure; the supervisor in `main.rs` tears the session down, emits `reconnecting`, and retries the last `start` with backoff (250 ms doubling to 2 s) until it succeeds or the backend sends `stop` |
| Underrun / overrun / clock drift | handled in `core::RingRenderer`: silence padding, oldest-sample discard, one-sample slew; counted in `xruns` |
| Permission denied (macOS microphone TCC, Linux portal) | `error` with an actionable message, no retry storm (fatal for that start) |
| Engine crash | backend sees EOF, restarts the engine on the next start; the engine holds no system state except the Linux on-demand modules, which are reused by name |
| Backend crash | stdin EOF → engine stops streams and exits; Linux on-demand modules are unloaded |
| Virtual device missing | engine still runs to any output; dashboard shows install guidance |

Real-time threads never allocate, lock (except `try_lock` on the metrics mailbox) or log. Rings
are bounded (`RingBounds`), sized from the device periods at start.

### Ring policy (`engine/src/audio/core/ring.rs`)

Every decision uses the **margin**: frames queued beyond what the render callback wants now.

- **Underrun:** margin < 0 → play what is there, hold the last sample one frame, pad silence.
- **Slow capture clock:** margin below the adaptive cushion → hold one sample (stretch by one frame).
- **Fast capture clock / unlucky start phase:** *smoothed* margin above `cushion + target + capture/2`
  → skip one sample. Smoothing hides the sawtooth that unequal capture/render periods put on the
  instantaneous margin, so mismatched periods do not trigger corrections.
- **Overflow (stall, burst):** margin above `cushion + target + 4 × capture` → drop the oldest
  frames down to `target`.
- **Adaptive cushion:** two free-running clocks make the render instant cross capture packets; each
  crossing costs one packet of margin. The cushion starts at 0 and grows by half a capture packet
  per underrun after a 2-second warm-up, capped at one packet: at most two glitches per
  session on drifting hardware, no added latency on same-clock hardware.

One-sample slew corrects up to 0.2 % of drift. `engine/tests/stress_core.rs` holds the policy to
this over ten simulated minutes (see [BENCHMARKS.md](BENCHMARKS.md)).

## Acceptance criteria

The work is complete when all of these hold. Items marked *(CI)* cannot run on a developer PC
without the platform toolchain and are verified by GitHub Actions; *(manual)* needs real
hardware or a signed build.

**A. No third-party virtual audio**
1. No code path, default, hint or doc requires or recommends VB-Cable, Voicemeeter, BlackHole or
   similar. `grep -ri "vb-audio\|voicemeeter\|blackhole"` over source finds nothing outside the
   changelog/migration note.
2. With the HushCore virtual device installed, the default output is `HushCore Sink` and apps see
   `HushCore Microphone`.

**B. Engine core and backends**
1. `cargo test` passes on Windows *(local + CI)*, Linux *(Docker + CI)*, macOS *(CI)*.
2. `cargo build --release` produces `hushcore-engine` on all three *(CI)*.
3. The DSP golden tests are byte-for-byte unchanged.
4. `hushcore-engine selftest 10` runs on Windows and Linux (Docker with PipeWire) with
   `xruns` = 0 in steady state after the first second.
5. A device failure triggers `reconnecting` then `started` again without backend involvement
   (unit test with a fake backend + manual unplug test).

**C. Virtual device**
1. Windows driver builds with the WDK (`msbuild`) into `.sys` + `.inf` + `.cat` *(CI)*; INF passes
   `infverif` *(CI)*; install/uninstall scripts are idempotent *(manual, test-signed VM)*.
2. macOS plug-in builds as a universal bundle *(CI)*; installed on the runner, `HushCore
   Microphone` appears and a tone written to `HushCore Sink` is read back *(CI)*.
3. Linux: in Docker with PipeWire, the engine's tone reaches `HushCore Microphone` (recorded with
   `pw-record`/`parec`, RMS above −30 dBFS, correct frequency) *(local Docker + CI)*.
4. Uninstall leaves no device, file or module behind.

**D. Backend and dashboard**
1. `pytest backend/tests` passes on Python 3.10 and 3.12.
2. `/api/devices` reports virtual-device status; the dashboard shows "installed / not installed"
   with the platform's install command instead of cable hints.
3. `npm run build` succeeds.

**E. Quality gates**
1. `cargo clippy -- -D warnings` clean on each platform's code *(CI)*.
2. Benchmarks: `cargo run --release --example bench_chain` reports the chain cost; `selftest`
   reports CPU and latency. Results recorded in `docs/BENCHMARKS.md`.
3. Security review of driver, plug-in and engine recorded in `docs/SECURITY.md`.
4. Docs: `docs/BUILDING.md` (dev setup per OS), `docs/VIRTUAL_DEVICE.md` (install, uninstall,
   troubleshooting), `docs/SIGNING.md` (attestation signing, Developer ID, notarization).
