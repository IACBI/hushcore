# hushcore-engine control protocol

The engine is a child process of the Python backend. Control is newline-delimited JSON (UTF-8):
the backend writes commands to the engine's stdin, the engine writes events to stdout.
stderr is free-form logging. Every message is one JSON object on one line.

## Commands (backend -> engine)

| cmd | fields | effect |
| --- | --- | --- |
| `devices` | | emit a `devices` event |
| `start` | `input`, `output`, `monitor`, `monitor_enabled`, `mode`, `settings` | open streams and run. `input`/`output`/`monitor` are device names as shown by the UI, e.g. `"Mikrofon (Realtek(R) Audio) [Windows WASAPI]"`; a trailing ` [...]` host-API suffix is ignored and matching is by case-insensitive equality, then substring. Empty string = default (for output: a device whose name contains `cable`, `virtual` or `vb-audio` is preferred, else the default render endpoint). `mode` is `"shared"` (default) or `"exclusive"`. `settings` is the flat settings object. |
| `settings` | `settings` | replace the DSP settings on the fly (lock-free) |
| `monitor` | `enabled`, `device` | start/stop the monitor stream while running |
| `stop` | | close streams, stay alive |
| `quit` | | stop and exit |

## Events (engine -> backend)

| event | fields |
| --- | --- |
| `ready` | `version`, emitted once at process start |
| `devices` | `inputs`, `outputs`: lists of `{ "name", "default": bool, "min_period_ms", "sample_rate" }` |
| `started` | `sample_rate`, `mode` (`"shared-low-latency"`, `"shared"` or `"exclusive"`), `input_period_ms`, `output_period_ms`, `latency_ms` |
| `stopped` | |
| `status` | at ~30 Hz while running: `input_rms`, `output_rms` (dBFS), `gate_state`, `fft_data` (64 floats 0..1), `clipping`, `cpu_load` (0..100), `latency_ms`, `sample_rate`, `xruns` (render underruns + capture overruns since start) |
| `error` | `message`, `fatal` (bool) |

The backend maps `status` to the existing WebSocket payload and adds `is_running`, `df_available`, `vst_loaded`, `vst_failed`, `engine`.
