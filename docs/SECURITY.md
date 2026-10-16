# Security

HushCore processes live microphone audio and installs a privileged virtual device, so its attack
surface is kept small on purpose. This page lists the trust boundaries, what each one does, and
the results of the review done for the cross-platform release.

## Trust boundaries

| Boundary | Privilege | Exposure | Controls |
| --- | --- | --- | --- |
| Windows driver `HushCoreAudio.sys` | kernel | KS property and pin requests from user mode (through PortCls) | no custom IOCTLs or device interfaces; one fixed format, validated on a local copy; PortCls-managed MDLs only; buffer size capped at 4 MiB; non-paged ring behind one spin lock with a single lock order; timers deleted (waiting for callbacks) before buffers are freed; `PnpLockdown=1`; `/kernel`, SDL checks, warnings as errors |
| macOS plug-in `HushCoreAudio.driver` | runs inside `coreaudiod` (sandboxed) | property get/set from any Core Audio client | every size checked before reading or writing; `QueryInterface` answers only the driver and `IUnknown` interfaces; lock-free, allocation-free IO path; clock state double-buffered so the IO thread never waits |
| Linux virtual device | user session | none beyond the PipeWire/PulseAudio server | plain modules (loopback, null-sink, remap-source); no code runs in the server |
| `hushcore-engine` | user | JSON lines on stdin from its parent only | command lines capped at 64 KiB; no panics reachable from protocol input; settings NaN-guarded and clamped; spawned with an argument list, no shell |
| Backend (FastAPI) | user | `127.0.0.1` only | per-run random API key compared in constant time (HTTP and WebSocket); Host and Origin checks; CSP with a per-response nonce, `nosniff`, same-origin resource policy, no referrer; at most 4 unauthenticated WebSockets; preset names restricted; atomic file writes |
| VST3 plug-ins | user, in the backend process | native code chosen by the user | only an absolute path to an existing `.vst3` file (bundle directory on macOS); resolved path logged; failing plug-ins are bypassed |
| Install scripts | administrator / root | run once by the user | Windows: SHA256 manifest from the build checked before `pnputil`, catalog signature checked, test-signed packages need `-AllowTestSigned`; macOS: refuses writable or foreign-owned bundles, copies into a root-owned staging folder, `codesign --verify --deep --strict` before the bundle goes live; uninstallers are idempotent and locale-independent |

Nothing in HushCore sends audio or telemetry anywhere. No credential, key or certificate is stored
in the repository; release signing (see [SIGNING.md](SIGNING.md)) reads them from CI secrets.

## Review results

A static review covered the driver, the plug-in, the scripts, the engine and the backend. No
critical or high issues were found. The medium and low findings below were all fixed:

| Severity | Area | Finding | Fix |
| --- | --- | --- | --- |
| Medium | macOS install | bundle copied into `coreaudiod`'s plug-in folder without verification | ownership/permission checks, root-owned staging, `codesign --verify` |
| Medium | Windows install | kernel package installed without an integrity check | build-time SHA256 manifest, catalog signature check, explicit `-AllowTestSigned` |
| Low | driver | data format size read twice from caller memory | validate a local copy |
| Low | driver | buffer leaked on an unexpected MDL in `FreeAudioBuffer` | always release, assert on mismatch |
| Low | engine | unbounded stdin line | 64 KiB cap |
| Low | engine | reconnect backoff restarted on every loss | resets only after 2 s of stable running |
| Low | backend | VST path not validated | absolute `.vst3` path required |
| Low | backend | no defensive headers on the key-bearing page | CSP with nonce and hardening headers |
| Low | backend | unauthenticated WebSockets unlimited | cap of 4, close code 1013 |
| Low | macOS plug-in | `QueryInterface` accepted any IID; IO thread could spin on a seqlock | strict IID check; double-buffered clock |
| Low | Windows uninstall | English-only parsing; MediaCategories keys left behind | `Get-WindowsDriver`; keys removed |

A correctness review of the engine also found two reliability issues with security-like impact
(availability), both fixed: a monitor failure could hide a simultaneous main-stream failure (the
failure flag is now a bit set), and a macOS processor overload disabled monitoring.

## Not yet verified

The review was static. The kernel driver and the Core Audio plug-in have not run under Driver
Verifier, Static Driver Verifier or a fuzzer. Before a signed release, run Driver Verifier with
standard settings plus special pool on a test machine, Static Driver Verifier on the project, and
the WDK's audio WHQL tests that apply to virtual devices.

## Reporting a vulnerability

Open a private security advisory on the repository rather than a public issue.
