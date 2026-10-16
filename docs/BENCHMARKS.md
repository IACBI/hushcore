# Benchmarks

Measured with the commands in [BUILDING.md](BUILDING.md). CI repeats the chain benchmark on every
platform; its numbers are in each `engine` job log.

## DSP chain (all stages enabled)

`cargo run --release --example bench_chain`, Windows 11, x86-64 desktop:

| Block | Time per block | Real-time factor |
| --- | --- | --- |
| 128 samples (2.67 ms) | 9.45 µs | 0.354 % of one core |
| 480 samples (10 ms) | 30.86 µs | 0.309 % of one core |

## End-to-end engine (Windows, real hardware)

`hushcore-engine selftest`, Realtek microphone to default output, shared low-latency mode,
48 kHz, 10 ms periods, output muted:

| Run | Latency avg | CPU avg / max | xruns |
| --- | --- | --- | --- |
| 3 × 15 s | 35.6 ms (247 trim drops) | not recorded | 0 |
| 5 s, before the fill fix | 35.2 ms | 0.7 % / 1.2 % | 0 |
| 15 s while a Docker build finished, before the fill fix | 44.3 ms | 0.5 % / 0.6 % | 3 (cushion grew) |

Runs marked *before the fill fix* reported the ring fill before the render callback's take, which
counted the frames just handed to the device twice; the estimate now uses the time-average queue
depth (the midpoint before and after each callback), about half a render period (5 ms) lower. The
start phase (where the render instant falls between capture packets) moves the result by up to
half a capture packet; the smoothed-margin trim keeps that bound, where the old policy allowed a
full packet.

The running estimate is capture period + render period + ring fill + the DSP chain's look-ahead
(255 samples, 5.3 ms at 48 kHz, for the noise suppressor's STFT); the start-up figure in `started` leaves out the
look-ahead. The virtual device adds its own period on top (about 10 ms on Windows).

## End-to-end engine (Linux, Docker)

`bash tools/linux-docker/run.sh`: headless PipeWire with pipewire-pulse (Ubuntu 24.04 packages), a
440 Hz sine source as the microphone, the engine playing into HushCore Sink, and a recording of
HushCore Microphone:

| Check | Result |
| --- | --- |
| Recorded level / frequency | −15.1 dBFS, 439.9 Hz |
| Engine latency (`started`) | 8.6–11 ms |
| `selftest 10 shared` (before the fill fix) | 5 ms periods; start-up estimate 8.4–9.5 ms, running estimate 20.9–22.7 ms; CPU 0.5–0.6 % avg; 2–3 underruns per run (Docker scheduling), 0 in the streaming check's steady state |
| Reconnect after the microphone module is unloaded and reloaded | streaming again 0.4 s after reload |

## Ring policy under drift (simulation)

`cargo test --release --test stress_core -- --nocapture`. 10 ms capture and render periods unless
noted, 10 simulated minutes (60 s for the 1 % cases):

| Scenario | Underruns after 2 s | Max latency |
| --- | --- | --- |
| same clock | 0 | 15.0 ms |
| render 100 ppm fast | 2 (cushion growth, last at 150 s) | 20.0 ms |
| render 300 ppm fast | 2 (last at 50 s) | 20.0 ms |
| render 300 ppm slow | 0 | 29.9 ms |
| capture bursts of 2 packets | 0 | 20.0 ms |
| capture 3 ms, render 10 ms | 0 | 13.0 ms |
| render 3 ms, capture 10 ms | 0 | 12.4 ms |
| 2.67 ms both (128 frames) | 0 | 4.0 ms |
| render 1 % fast (broken device) | 58 / min | 15.0 ms |
| render 1 % slow (broken device) | 0 | 56.9 ms |

Before the margin-based policy, 300 ppm fast produced an underrun about every 33 s, and capture
3 ms with render 10 ms underran 19,885 times in 10 minutes. One-sample slew corrects up to 0.2 %
of drift; devices that are 1 % off stay bounded but audibly glitch.
