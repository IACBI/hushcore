"""Regenerate the golden reference vectors in engine/tests/golden/.

The references come from the Python DSP classes in backend/audio_processor.py, driven
with the same 480-sample blocks the Python engine uses (10 ms at 48 kHz). The Rust
integration tests in engine/tests/golden.rs read the manifest and compare against them.

Run from the repository root:  py -3.10 engine/tests/gen_golden.py
"""

import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
BACKEND = os.path.normpath(os.path.join(HERE, "..", "..", "backend"))
GOLDEN = os.path.join(HERE, "golden")
sys.path.insert(0, BACKEND)

import audio_processor as ap  # noqa: E402

SR = 48000
BLOCK = 480
N = 12000  # 0.25 s at 48 kHz
SEED = 20260101

# Tolerances are max absolute sample error against the Python reference (full scale = 1.0).
# Filters are exact linear maps, so only float32/float64 rounding differs. The envelope
# modules are block-level in both implementations; their gain steps are identical at
# block 480, so the bound is loose only to absorb float32 rounding of the detector input.
TOL_FILTER = 1e-5
TOL_ENVELOPE = 1e-4


def _blocks(x, fn):
    out = np.empty_like(x)
    for start in range(0, x.shape[0], BLOCK):
        out[start:start + BLOCK] = fn(x[start:start + BLOCK])
    return out


def _t():
    return np.arange(N, dtype=np.float64) / SR


def make_cases():
    rng = np.random.default_rng(SEED)
    t = _t()
    cases = []

    # EQ: non-trivial low shelf boost, mid cut, high shelf boost.
    x = 0.1 * rng.standard_normal(N) + 0.25 * np.sin(2 * np.pi * 180.0 * t) \
        + 0.15 * np.sin(2 * np.pi * 1000.0 * t) + 0.1 * np.sin(2 * np.pi * 7000.0 * t)
    eq = ap.Equalizer3Band(sr=SR)
    eq.update_gains(6.0, -5.0, 4.0)
    cases.append(dict(
        name="eq", module="eq", input=x, tolerance=TOL_FILTER,
        params={"low_gain_db": 6.0, "mid_gain_db": -5.0, "high_gain_db": 4.0},
        run=lambda y: _blocks(y, eq.process),
    ))

    # Compressor: quiet lead-in then a loud section, so attack and release both act.
    env = np.where(t < 0.1, 0.02, 0.6)
    x = env * np.sin(2 * np.pi * 220.0 * t) + 0.02 * rng.standard_normal(N)
    comp = ap.DynamicsCompressor(sr=SR, block_size=BLOCK)
    comp.threshold_db = -18.0
    comp.ratio = 4.0
    comp.attack_ms = 10.0
    comp.release_ms = 100.0
    cases.append(dict(
        name="compressor", module="compressor", input=x, tolerance=TOL_ENVELOPE,
        params={"threshold_db": -18.0, "ratio": 4.0, "attack_ms": 10.0, "release_ms": 100.0},
        run=lambda y: _blocks(y, comp.process),
    ))

    # Gate: tone bursts over a quiet floor, so the gate opens, holds and closes.
    gate_env = (np.floor(t * 4.0).astype(int) % 2 == 0).astype(np.float64)
    x = gate_env * 0.3 * np.sin(2 * np.pi * 300.0 * t) + 0.003 * rng.standard_normal(N)
    gate = ap.NoiseGate(sr=SR, block_size=BLOCK)
    cases.append(dict(
        name="gate", module="gate", input=x, tolerance=TOL_ENVELOPE,
        params={"threshold_db": -45.0, "release_ms": 150.0},
        run=lambda y: _blocks(y, lambda b: gate.process(b, -45.0, 150.0)),
    ))

    # De-esser: sibilant noise bursts above a low tone.
    burst = (np.floor(t * 5.0).astype(int) % 2 == 1).astype(np.float64)
    x = 0.2 * np.sin(2 * np.pi * 300.0 * t) + burst * 0.3 * rng.standard_normal(N)
    dess = ap.VocalDeEsser(sr=SR, block_size=BLOCK)
    dess.update_params(-25.0, 0.5)
    cases.append(dict(
        name="deesser", module="deesser", input=x, tolerance=TOL_ENVELOPE,
        params={"threshold_db": -25.0, "amount": 0.5},
        run=lambda y: _blocks(y, dess.process),
    ))

    # Exciter: harmonics generated above 3 kHz, with the drive and mix raised so they show.
    x = 0.3 * np.sin(2 * np.pi * 300.0 * t) + 0.15 * np.sin(2 * np.pi * 2000.0 * t) \
        + 0.05 * rng.standard_normal(N)
    exc = ap.VocalExciter(sr=SR)
    exc.update_params(3000.0, 0.6, 0.4)
    cases.append(dict(
        name="exciter", module="exciter", input=x, tolerance=TOL_FILTER,
        params={"frequency": 3000.0, "amount": 0.6, "mix": 0.4},
        run=lambda y: _blocks(y, exc.process),
    ))

    # Limiter: loud 1 kHz sine well above the -1 dB ceiling, then a lower section so the
    # 100 ms release curve is exercised.
    env = np.where(t < 0.12, 1.5, 0.6)
    x = env * np.sin(2 * np.pi * 1000.0 * t)
    lim = ap.Limiter(sr=SR, block_size=BLOCK, release_ms=100.0)
    thr = 10.0 ** (-1.0 / 20.0)
    cases.append(dict(
        name="limiter", module="limiter", input=x, tolerance=TOL_ENVELOPE,
        params={"threshold_db": -1.0, "release_ms": 100.0},
        run=lambda y: _blocks(y, lambda b: lim.process(b, thr)),
    ))
    return cases


def main():
    os.makedirs(GOLDEN, exist_ok=True)
    manifest = {
        "sample_rate": SR,
        "block_size": BLOCK,
        "samples": N,
        "seed": SEED,
        "source": "backend/audio_processor.py",
        "cases": [],
    }
    for case in make_cases():
        x = case["input"].astype(np.float32)
        y = case["run"](x).astype(np.float32)
        in_name = f"{case['name']}.input.f32"
        out_name = f"{case['name']}.expected.f32"
        x.astype("<f4").tofile(os.path.join(GOLDEN, in_name))
        y.astype("<f4").tofile(os.path.join(GOLDEN, out_name))
        manifest["cases"].append({
            "name": case["name"],
            "module": case["module"],
            "params": case["params"],
            "input": in_name,
            "expected": out_name,
            "max_abs_tolerance": case["tolerance"],
            "peak_expected": float(np.max(np.abs(y))),
            "peak_input": float(np.max(np.abs(x))),
        })
        print(f"{case['name']:>10}: in peak {np.max(np.abs(x)):.3f}  ref peak {np.max(np.abs(y)):.3f}")
    with open(os.path.join(GOLDEN, "manifest.json"), "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")


if __name__ == "__main__":
    main()
