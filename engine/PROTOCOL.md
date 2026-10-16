# hushcore-engine control protocol

The engine is a child process of the Python backend. Control is newline-delimited JSON (UTF-8):
the backend writes commands to the engine's stdin, the engine writes events to stdout.
stderr is free-form logging. Every message is one JSON object on one line. Command lines longer than 64 KiB are skipped and answered with a non-fatal `error`.

## Commands (backend -> engine)

| cmd | fields | effect |
| --- | --- | --- |
| `devices` | | emit a `devices` event |
| `start` | `input`, `output`, `monitor`, `monitor_enabled`, `mode`, `settings` | open streams and run. `input`/`output`/`monitor` are device names as shown by the UI, e.g. `"Mikrofon (Realtek(R) Audio) [Windows WASAPI]"`; a trailing ` [...]` host-API suffix is ignored and matching is by case-insensitive equality, then substring. Empty string = default: for output, `HushCore Sink` when installed, else the default render endpoint; for input, the default capture endpoint unless it is `HushCore Microphone` (that would capture the engine's own output), else the first other input. `mode` is `"shared"` (default) or `"exclusive"` (WASAPI only; other platforms run shared). `settings` is the flat settings object. |
| `settings` | `settings` | replace the DSP settings on the fly (lock-free) |
| `monitor` | `enabled`, `device` | start/stop the monitor stream while running |
| `stop` | | close streams, stay alive |
| `quit` | | stop and exit |

## Events (engine -> backend)

| event | fields |
| --- | --- |
| `ready` | `version`, emitted once at process start |
| `devices` | `inputs`, `outputs`: lists of `{ "name", "default": bool, "min_period_ms", "sample_rate" }`; `virtual_device`: `{ "installed": bool, "provider", "sink", "microphone", "detail" }` where `provider` is `windows-driver`, `macos-plugin`, `pipewire`, `pulseaudio`, `on-demand` or `none`, and `sink`/`microphone` are endpoint names or `null` |
| `started` | `sample_rate`, `mode` (`"shared-low-latency"`, `"shared"` or `"exclusive"`), `input_period_ms`, `output_period_ms`, `latency_ms` |
| `stopped` | |
| `reconnecting` | `reason`, `attempt` (1-based), `retry_in_ms`. A device was lost (unplug, sample-rate change, sleep/resume, audio service restart); the engine closed the streams and retries the last `start` with backoff (250 ms doubling to 2 s) until it succeeds (`started`) or the backend sends `stop` (`stopped`). No `status` events in between. |
| `status` | at ~30 Hz while running: `input_rms`, `output_rms` (dBFS), `gate_state`, `fft_data` (64 floats 0..1), `clipping`, `cpu_load` (0..100), `latency_ms`, `sample_rate`, `xruns` (glitches since start: render underruns, capture-side ring overflows, render-side overflow discards, and capture discontinuities reported by the OS; deliberate one-sample drift corrections are not counted) |
| `error` | `message`, `fatal` (bool) |

Device failures never end in `stopped` on their own: the engine keeps reconnecting until told to stop.

The backend maps `status` to the existing WebSocket payload and adds `is_running`, `df_available`, `vst_loaded`, `vst_failed`, `engine`.
