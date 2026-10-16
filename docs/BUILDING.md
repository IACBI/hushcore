# Building HushCore

HushCore has four parts that build independently:

| Part | Language | Output | Platforms |
| --- | --- | --- | --- |
| `engine/` | Rust | `hushcore-engine` | Windows (WASAPI), macOS (Core Audio), Linux (PulseAudio API, PipeWire or PulseAudio server) |
| `backend/` + `frontend/` | Python 3.10+, JavaScript (Vite) | FastAPI app + dashboard in `backend/static` | all |
| `driver/windows/` | C++ (WDK) | `HushCoreAudio.sys/.inf/.cat` | Windows 10/11 x64, ARM64 |
| `driver/macos/` | C | `HushCoreAudio.driver` | macOS 11+, universal |
| `driver/linux/` | config | PipeWire drop-in / PulseAudio script | Linux |

## Common: engine, backend, dashboard

```bash
cargo test --release --locked --manifest-path engine/Cargo.toml
cargo build --release --locked --manifest-path engine/Cargo.toml
python -m venv venv
venv/bin/pip install -r backend/requirements-dev.txt     # Windows: venv\Scripts\pip
venv/bin/python -m pytest backend/tests -q
cd frontend && npm ci && npm run build
```

`run.bat` (Windows) and `run.sh` (macOS, Linux) do all of this on first start and then launch the
backend on `http://127.0.0.1:8000`.

Engine diagnostics, all with output muted where audio runs:

| Command | What it shows |
| --- | --- |
| `hushcore-engine devices` | the `devices` event, including `virtual_device` |
| `hushcore-engine virtual-device` | virtual-device status as JSON; exit code 0 only when installed |
| `hushcore-engine probe` | per-endpoint periods and rates |
| `hushcore-engine selftest 10 [shared\|exclusive] [output]` | 10 s run with latency, CPU and every xrun counter |
| `cargo run --release --example bench_chain` | DSP chain cost per block |
| `cargo test --release --test stress_core -- --nocapture` | 10-minute simulated drift, jitter and period-mismatch runs |

## Windows

- Visual Studio 2022 or newer with the C++ workload, Rust stable (MSVC).
- For the driver: the **Windows Driver Kit** matching your SDK (10.0.26100) and the WDK Visual
  Studio extension. GitHub's `windows-2022` image has both.

```powershell
powershell -ExecutionPolicy Bypass -File scripts\windows\build-driver.ps1 -Platform x64
# package in dist\driver\windows\x64\ (test-signed); use -Platform ARM64 for dist\driver\windows\ARM64\
```

Loading a test-signed driver needs test signing on a development machine or VM. That is a boot
setting you change yourself (`bcdedit /set testsigning on`, reboot); the scripts never change it.
Then, as administrator. The package is test-signed, so the installer needs `-AllowTestSigned`:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1 -AllowTestSigned
powershell -ExecutionPolicy Bypass -File scripts\windows\uninstall-driver.ps1
```

Details, data flow and limits: [driver/windows/README.md](../driver/windows/README.md).
Release signing: [SIGNING.md](SIGNING.md).

## macOS

- Xcode command line tools (`xcode-select --install`), Rust stable.

```bash
bash scripts/macos/build-plugin.sh          # dist/driver/macos/HushCoreAudio.driver, ad-hoc signed
sudo bash scripts/macos/install-plugin.sh   # copies to /Library/Audio/Plug-Ins/HAL, restarts coreaudiod
make -C driver/macos test                   # tone through HushCore Sink, read back from HushCore Microphone (plug-in must be installed)
sudo bash scripts/macos/uninstall-plugin.sh
```

The backend's terminal (or app) needs microphone permission: System Settings → Privacy &
Security → Microphone. Details: [driver/macos/README.md](../driver/macos/README.md).

## Linux

- `libpulse-dev` and `pkg-config` to build the engine; a running PipeWire (with `pipewire-pulse`)
  or PulseAudio session to run it. `libportaudio2` for the Python engine.

```bash
sudo apt-get install libpulse-dev pkg-config libportaudio2   # Debian/Ubuntu
scripts/linux/install-virtual-mic.sh                         # persistent HushCore Sink + Microphone, no root
scripts/linux/uninstall-virtual-mic.sh
```

Without the persistent config the engine creates the pair on demand when it starts and removes it
when it exits. Details: [driver/linux/README.md](../driver/linux/README.md).

The whole Linux path runs headless in Docker (PipeWire, a fake microphone, the engine, a recording
of HushCore Microphone and an FFT check):

```bash
bash tools/linux-docker/run.sh
```

## CI

`.github/workflows/ci.yml` runs on every push and pull request:

| Job | Runner | What it proves |
| --- | --- | --- |
| `no-third-party-audio` | ubuntu | no source refers to third-party virtual audio software |
| `backend` | ubuntu, Python 3.10 and 3.12 | API, engine client, device rules |
| `frontend` | ubuntu | dashboard builds |
| `engine` | windows, macos-14, ubuntu-24.04 | build, tests, clippy `-D warnings`, chain benchmark |
| `linux-e2e` | ubuntu-24.04 + Docker | audio reaches HushCore Microphone; reconnect; cleanup |
| `windows-driver` | windows-2022 | driver builds for x64 and ARM64 (test-signed), InfVerif passes on both |
| `macos-plugin` | macos-14 | plug-in builds, installs into coreaudiod, runs the loopback test (buffer-delivery check only if the runner has no microphone permission), uninstalls |

Hardware-only checks (a real unplug, sleep/resume, a test-signed driver on a VM) are listed in
[VIRTUAL_DEVICE.md](VIRTUAL_DEVICE.md#manual-release-checks). What CI does not prove is listed in the
driver READMEs: [Windows](../driver/windows/README.md#not-verified), [macOS](../driver/macos/README.md#verification-status),
[Linux](../driver/linux/README.md#verified-and-not-verified).

## Release

Pushing a tag `v*` starts `.github/workflows/release.yml`. It builds the engine for Windows, macOS
(universal) and Linux, the Windows driver (x64 and ARM64), the macOS plug-in and the Linux
virtual-microphone files, then creates a **draft** GitHub release with them. The tag must match the
version in `engine/Cargo.toml`. Starting the workflow by hand (`workflow_dispatch`) runs the same builds
and keeps the files as the `release-dry-run` artifact without publishing anything.

Assets, for a tag such as `v0.0.99`:

| Asset | Contents |
| --- | --- |
| `hushcore-engine-v0.0.99-windows-x64.zip` | `hushcore-engine.exe` |
| `hushcore-engine-v0.0.99-macos-universal.zip` | `hushcore-engine` (arm64 and x86_64) |
| `hushcore-engine-v0.0.99-linux-x64.zip` | `hushcore-engine` |
| `hushcore-driver-v0.0.99-windows-testsigned.zip` | `scripts\windows\install-driver.ps1`, `uninstall-driver.ps1`, `dist\driver\windows\` (x64 and ARM64) |
| `hushcore-plugin-v0.0.99-macos-adhoc.zip` | `scripts/macos/install-plugin.sh`, `uninstall-plugin.sh`, `dist/driver/macos/` |
| `hushcore-virtual-mic-v0.0.99-linux.zip` | `driver/linux/`, `scripts/linux/` |
| `SHA256SUMS` | SHA-256 of every archive |

The virtual-device archives keep the repository layout, because the scripts find their files two
levels up from their own location. Extract an archive and run the script from the extracted folder.
Install steps are in [VIRTUAL_DEVICE.md](VIRTUAL_DEVICE.md#installing-from-a-release). Signing state of
each asset is in [SIGNING.md](SIGNING.md#current-release-state).
