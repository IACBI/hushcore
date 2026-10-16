# HushCore virtual microphone on Linux

HushCore's engine writes the processed voice to **HushCore Sink**, and applications read it from
**HushCore Microphone**. No third-party virtual audio software is involved. The engine talks the
PulseAudio protocol, which both PipeWire (through `pipewire-pulse`) and PulseAudio serve.

## How it works

| Piece | What it is | Where |
| --- | --- | --- |
| PipeWire config (preferred) | loopback module: `hushcore_sink` (sink) feeds `hushcore_mic` (source) | `pipewire/90-hushcore-virtual-mic.conf` |
| PulseAudio config | `module-null-sink` + `module-remap-source` (`master=hushcore_sink.monitor`) | `pulseaudio/hushcore-virtual-mic.pa` |
| On demand | the engine loads the same pair itself when started with an empty output and no pair exists; it unloads only those modules when it exits | `engine/src/audio/pulse/modules.rs` |

The engine detects an existing pair by name and reuses it, so a persistent config and an on-demand
pair are never duplicated. `virtual-device` reports `provider`: `pipewire`, `pulseaudio`,
`on-demand` or `none`.

## Install and uninstall

No root is needed. Run from the repository root, or from the extracted `hushcore-virtual-mic-<tag>-linux.zip`
(it keeps the repository layout the scripts expect):

```sh
scripts/linux/install-virtual-mic.sh     # installs the config and verifies both devices appear
scripts/linux/uninstall-virtual-mic.sh   # removes it; safe to run twice
```

- **PipeWire:** the drop-in is copied to `${XDG_CONFIG_HOME:-~/.config}/pipewire/pipewire.conf.d/`
  and `systemctl --user restart pipewire pipewire-pulse wireplumber` is run. Without systemd user
  services the script says so, and you restart the audio services yourself (log out and back in
  also works).
- **PulseAudio:** `~/.config/pulse/hushcore-virtual-mic.pa` is written and included from
  `~/.config/pulse/default.pa` between marker lines. If `default.pa` does not exist, it is created
  with `.include /etc/pulse/default.pa` first. The modules are also loaded immediately.

Uninstall removes the drop-in or the include block and unloads the PulseAudio modules. On PipeWire
the devices disappear at the next audio restart.

## Troubleshooting

```sh
pactl info                              # "Server Name: PulseAudio (on PipeWire ...)" means PipeWire
pactl list short sinks                  # expect hushcore_sink
pactl list short sources                # expect hushcore_mic
pactl list short modules | grep hushcore
pw-cli ls Node | grep -i hushcore       # PipeWire node view
hushcore-engine virtual-device          # JSON status; exits 0 when both endpoints are present
hushcore-engine probe                   # every sink and source the engine can see
```

- **HushCore Microphone is silent in an app:** check that the engine is running (`started` event)
  and that the app's input is `HushCore Microphone`, not the monitor of `HushCore Sink`.
- **`no PulseAudio or PipeWire server is reachable`:** the scripts need a running user session;
  run them from a desktop login, not from a root or system service.
- **Device missing after a restart:** the on-demand pair is gone after the engine exits. Start the
  engine with an empty output to recreate it, or install the persistent config.

## Flatpak and portals

Sandboxed apps (Flatpak, Snap) reach audio through the desktop's portal and the PipeWire socket,
and their microphone access is controlled by the desktop, not by HushCore. If a sandboxed app
records silence from `HushCore Microphone`, check its microphone permission in the desktop's
privacy settings first. This was not tested in a sandbox; see below.

## Verified and not verified

Verified in the Docker harness (`bash tools/linux-docker/run.sh`, Ubuntu 24.04 packages for PipeWire,
WirePlumber and pipewire-pulse, no sound hardware):

- engine start on the default output creates the pair on demand; recording `HushCore Microphone`
  gives -15.1 dBFS RMS and 439.9 Hz for a 440 Hz tone at 0.5 peak amplitude fed to the fake mic
- steady state in the streaming check: no xruns. A 10 s `selftest` records 2 to 3 underruns per run
  under Docker scheduling (see [docs/BENCHMARKS.md](../../docs/BENCHMARKS.md))
- started latency 8.6 to 11 ms; engine CPU 0.5 to 0.6 % average
- reconnect: unloading the input gives `reconnecting`, and the engine streams again 0.4 s after
  the input returns
- persistent PipeWire config: `virtual-device` exits 0 with provider `pipewire`, no duplicate
  pair, and recording works through it
- on-demand modules are removed when the engine quits and when its selftest exits
- install and uninstall scripts run twice without error and leave no device after a restart

Not verified:

- the PulseAudio branch of the install and uninstall scripts and `hushcore-virtual-mic.pa` (no PulseAudio
  server in the container; pipewire-pulse only)
- the systemd restart path (the container has no systemd user session)
- a real microphone, real unplug, suspend and resume, or Bluetooth devices
- Flatpak and Snap sandboxing
- the monitor stream (`set_monitor`) is implemented but not exercised by the harness

Known limitation: the capture thread blocks inside `pa_simple_read`, which the library does not
interrupt. When the input is lost, the watchdog reports it and teardown leaves that one thread
behind after a one-second wait; it holds one connection until the server closes it.
