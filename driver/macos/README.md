# HushCore macOS virtual audio driver

`HushCoreAudio.driver` is a Core Audio AudioServerPlugIn loaded by `coreaudiod`. It provides
the two virtual devices HushCore needs on macOS, so no third-party virtual audio software is
required:

- **HushCore Sink** (output only): the engine plays processed audio into it.
- **HushCore Microphone** (input only): applications such as Discord, OBS or Zoom record from it.

Only the bundle lives here. The engine's device I/O is in `engine/src/audio/coreaudio/`, and the
install and build wrappers are in `scripts/macos/`.

## Status

The plug-in is built, installed and tested on the `macos-14` runner in CI (see
"Verification status" at the end). It has not been run on a physical Mac.

## Design

**Objects.** The plug-in object, two devices (`HushCore Sink`, `HushCore Microphone`) and one
stream per device. There are no boxes and no controls.

| Device | UID | Streams | Default-device flags |
| --- | --- | --- | --- |
| HushCore Sink | `HushCoreSink_UID` | output | can be default: 0, can be system default: 0 |
| HushCore Microphone | `HushCoreMicrophone_UID` | input | can be default: 1 (input scope), can be system default: 0 |

Both devices report manufacturer `HushCore`, transport `kAudioDeviceTransportTypeVirtual`,
the same clock domain, `ClockIsStable = 1`, `IsHidden = 0`, and safety offset 32 frames.
Latency is 0 on both devices and both streams.

**Format.** 32-bit float, interleaved, packed stereo. Nominal rates 44100 and 48000 Hz,
48000 by default. Virtual and physical stream formats are identical. A rate change on either
device goes through `RequestDeviceConfigurationChange`; the driver applies it in
`PerformDeviceConfigurationChange` to both devices and both streams at once, then notifies
the host. The loopback therefore never resamples.

**Clock.** One host clock for the whole plug-in. The anchor is taken at the first `StartIO` of
either device and reset when both have stopped. `GetZeroTimeStamp` returns the most recent
multiple of `HC_RING_FRAMES` (16384) frames, with the matching `mach_absolute_time` value
computed from the timebase. Both devices return the same zero timestamps, so their sample times
line up exactly. `ZeroTimeStampPeriod` is 16384 frames. The seed increments on every clock restart (first start
after both devices stopped, and every rate change), so clients see the timeline restart.
The clock state is double-buffered: the writer fills the inactive slot and publishes its index
with release order, and the IO thread reads the index with acquire order and never waits.

**Loopback ring.** A float32 stereo ring of 16384 frames indexed by sample time modulo its size.
The sink's `WriteMix` stores its buffer at `mOutputTime` and publishes the end of the written
range with release order. The microphone's `ReadInput` reads at `mInputTime` with acquire order.
Frames at or after the published end, or more than one ring length behind it, come back as
silence. The ring is cleared when the first device starts. The IO path does not allocate, lock
or log.

**Latency.** The microphone's input sample time lags the sink's output sample time by about two
IO buffers (the sink writes ahead, the microphone reads what has been written). This is a design estimate, not a measurement: about
1024 frames (about 21 ms at 48 kHz) at a 512-frame IO size. The safety offset is kept small to
keep this down. The trade-off is that the ring is written in place with no extra jitter buffer:
a stalled engine produces silence on the microphone rather than stale audio.

**Properties.** Every object answers the selectors listed in `DescribePlugIn`, `DescribeDevice`
and `DescribeStream` in `src/HushCoreAudio.c`. Size queries and reads share one description
function. Strings are returned retained, following the Copy rule.

## Build

Requirements: macOS 11 or later, Xcode command line tools (`clang`, `codesign`, `make`).

```sh
scripts/macos/build-plugin.sh          # builds driver/macos/build and copies to dist/driver/macos/
make -C driver/macos test              # builds tests/loopback_test and runs it (needs the plug-in installed)
make -C driver/macos clean
```

`make -C driver/macos all` compiles `src/HushCoreAudio.c` for `arm64` and `x86_64` with
`-mmacosx-version-min=11.0 -bundle -O2 -Wall -Wextra -Werror`, links CoreAudio and
CoreFoundation, writes `build/HushCoreAudio.driver/Contents/{Info.plist,MacOS/HushCoreAudio}`
and ad-hoc signs the bundle with `codesign --force --sign -`.

## Install and uninstall

```sh
sudo scripts/macos/install-plugin.sh          # copies to /Library/Audio/Plug-Ins/HAL/, restarts coreaudiod
sudo scripts/macos/uninstall-plugin.sh        # removes the bundle, restarts coreaudiod
```

Install fails closed if the source bundle contains symbolic links, has group- or
world-writable entries, or has entries owned by anyone other than root or the invoking user. It
copies the bundle into a root-owned staging folder inside the HAL folder, runs
`codesign --verify --deep --strict` on the copy, and only then moves it into place. It sets
`root:wheel` ownership and restarts `coreaudiod` with
`launchctl kickstart -kp system/com.apple.audio.coreaudiod` (falling back to `killall
coreaudiod`), and waits up to 20 seconds for both device names to appear in
`system_profiler SPAudioDataType`. Both scripts re-run themselves under `sudo` when started
without root. Both are idempotent.

The release archive `hushcore-plugin-<tag>-macos-adhoc.zip` keeps the repository layout. Extract
it and run the same commands from the extracted folder.

## Test

`tests/loopback_test.c` finds both devices by UID through
`kAudioHardwarePropertyTranslateUIDToDevice`, switches them to 48000 Hz, and then:

1. plays a 440 Hz tone at -12 dBFS into HushCore Sink, with one 48-sample click 0.5 s in;
2. records HushCore Microphone for 2.5 s on the same sample timeline;
3. checks the tone level within ±2 dB of the expected -15.0 dBFS RMS, the frequency within ±5 Hz
   (positive zero crossings), and reports the click latency in frames.

```sh
./driver/macos/build/loopback_test --list              # print both devices, rates and latencies
./driver/macos/build/loopback_test                     # full check
./driver/macos/build/loopback_test --delivery-only     # both IO procs ran and mic delivered buffers
```

Recording from a microphone device can be blocked by the TCC microphone permission. A process
that has no permission receives silence, and the full check reports "recorded silence". In that
case use `--delivery-only` to confirm the driver path, and grant the permission to the terminal
or CI agent for the full signal check. In CI the full check runs first; if it fails, the job emits a
warning and runs `--delivery-only`.

## Developer ID signing and notarization

The ad-hoc signature is enough for a local install. A distributed build needs a Developer ID
signature and notarization:

1. Sign the bundle with the Developer ID Application identity and the hardened runtime:
   `codesign --force --options runtime --timestamp --sign "Developer ID Application: <team>" build/HushCoreAudio.driver`
2. Package it: `pkgbuild --component build/HushCoreAudio.driver --install-location /Library/Audio/Plug-Ins/HAL HushCoreAudio.pkg`,
   then `productbuild --distribution <dist.xml> --package-path . signed.pkg` after signing the
   package with a Developer ID Installer identity (`productsign`).
3. Notarize and staple: `xcrun notarytool submit signed.pkg --wait --keychain-profile <profile>`,
   then `xcrun stapler staple signed.pkg`.

The identities, the keychain profile and the `dist.xml` layout belong to the release process and
are not stored in this repository.

## Troubleshooting

- Devices missing after install: `coreaudiod` logs the plug-in load and any rejected property.
  Watch them with `log stream --predicate 'process == "coreaudiod"'` (or `log show --last 5m` as
  above).
- Check the bundle structure: `codesign --verify --strict --verbose=2 /Library/Audio/Plug-Ins/HAL/HushCoreAudio.driver`.
- After reinstalling, `coreaudiod` must restart before new code is loaded. The install script
  does this; if you copy the bundle by hand, run `sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod`.
- The loopback reports silence at the start of a run: the first about 0.1 s is silent while the
  ring fills. Latency is only reported after the click is found.

## Verification status

Checked in CI on the `macos-14` runner (`macos-plugin` job in `.github/workflows/ci.yml`):

- the bundle compiles for arm64 and x86_64 with `-Werror` and passes `codesign --verify --strict`;
- `sudo scripts/macos/install-plugin.sh` installs it, `coreaudiod` loads it, and both devices appear;
- the loopback test runs. If the runner lacks microphone permission, the job warns and checks buffer
  delivery only (`--delivery-only`);
- `uninstall-plugin.sh` removes the bundle again.

Not verified:

- physical Macs of any kind (Apple silicon or Intel), and macOS versions other than the runner's;
- a real microphone, and real apps (Discord, OBS, Zoom, QuickTime) recording from HushCore Microphone;
- sleep, wake and device changes, and long-running stability;
- Developer ID signing and notarization. The release bundle is ad-hoc signed (see
  [docs/SIGNING.md](../../docs/SIGNING.md)).
