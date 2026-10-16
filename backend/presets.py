import json
import logging
import os
import re
import threading

from config import DEFAULT_SETTINGS, atomic_write_json, validate_and_coerce

logger = logging.getLogger(__name__)

USER_PRESETS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "user_presets.json")

# Device, monitoring, gain and buffer keys belong to the machine setup, not to
# a voice profile, so presets never carry them.
NON_DSP_KEYS = frozenset({
    "input_device",
    "output_device",
    "monitor_device",
    "monitor_enabled",
    "input_gain",
    "output_gain",
    "buffer_size",
    "vst_path",
})
DSP_KEYS = tuple(key for key in DEFAULT_SETTINGS if key not in NON_DSP_KEYS)

_NAME_PATTERN = re.compile(r"[\w \-]{1,32}")
_lock = threading.Lock()

BUILTIN_PRESETS = {
    "off": {
        "ns_mode": "off",
        "gate_enabled": False,
        "eq_enabled": False,
        "compressor_enabled": False,
        "deesser_enabled": False,
        "exciter_enabled": False,
        "vst_enabled": False,
    },
    "voice": {
        "ns_mode": "eco",
        "ns_eco_strength": 0.8,
        "gate_enabled": True,
        "gate_threshold_db": -48.0,
        "gate_release_ms": 150.0,
        "eq_enabled": True,
        "eq_low_gain_db": 0.0,
        "eq_mid_gain_db": 0.0,
        "eq_high_gain_db": 0.0,
        "compressor_enabled": True,
        "compressor_threshold_db": -18.0,
        "compressor_ratio": 3.0,
        "compressor_attack_ms": 10.0,
        "compressor_release_ms": 100.0,
        "deesser_enabled": False,
        "exciter_enabled": False,
        "vst_enabled": False,
    },
    "gaming": {
        "ns_mode": "eco",
        "ns_eco_strength": 0.9,
        "gate_enabled": True,
        # High threshold so mechanical key clicks are gated
        "gate_threshold_db": -38.0,
        "gate_release_ms": 100.0,
        "eq_enabled": True,
        "eq_low_gain_db": -2.0,
        "eq_mid_gain_db": 2.0,
        "eq_high_gain_db": 1.0,
        "compressor_enabled": True,
        "compressor_threshold_db": -14.0,
        "compressor_ratio": 3.5,
        "compressor_attack_ms": 5.0,
        "compressor_release_ms": 80.0,
        "deesser_enabled": False,
        "exciter_enabled": False,
        "vst_enabled": False,
    },
    "broadcaster": {
        "ns_mode": "eco",
        "ns_eco_strength": 0.75,
        "gate_enabled": True,
        "gate_threshold_db": -50.0,
        "gate_release_ms": 200.0,
        "eq_enabled": True,
        "eq_low_gain_db": 4.5,
        "eq_mid_gain_db": 1.0,
        "eq_high_gain_db": 3.0,
        "compressor_enabled": True,
        "compressor_threshold_db": -22.0,
        "compressor_ratio": 4.0,
        "compressor_attack_ms": 12.0,
        "compressor_release_ms": 120.0,
        "deesser_enabled": True,
        "deesser_threshold_db": -25.0,
        "deesser_amount": 0.5,
        "exciter_enabled": True,
        "exciter_frequency": 3000.0,
        "exciter_amount": 0.2,
        "exciter_mix": 0.15,
        "vst_enabled": False,
    },
}

_BUILTIN_NAMES_FOLDED = {name.casefold() for name in BUILTIN_PRESETS}


def sanitize_preset_name(name):
    if not isinstance(name, str):
        raise ValueError("Preset name must be a string")
    cleaned = name.strip()
    if not _NAME_PATTERN.fullmatch(cleaned):
        raise ValueError("Preset name must be 1-32 letters, digits, spaces, '-' or '_'")
    if cleaned.casefold() in _BUILTIN_NAMES_FOLDED:
        raise ValueError("Preset name is reserved for a built-in preset")
    return cleaned


def snapshot_dsp(settings):
    return {key: settings[key] for key in DSP_KEYS if key in settings}


def _clean_values(values):
    cleaned = {}
    for key, value in values.items():
        if key not in DSP_KEYS:
            continue
        coerced, error = validate_and_coerce(key, value)
        if error is None:
            cleaned[key] = coerced
        else:
            logger.warning("Dropping invalid preset value: %s", error)
    return cleaned


def _read_user_presets():
    if not os.path.exists(USER_PRESETS_FILE):
        return {}
    try:
        with open(USER_PRESETS_FILE, "r", encoding="utf-8") as f:
            raw = json.load(f)
    except Exception as e:
        logger.error("Could not read user presets, ignoring them: %s", e)
        return {}
    if not isinstance(raw, dict):
        return {}

    presets = {}
    for name, values in raw.items():
        try:
            if sanitize_preset_name(name) != name:
                raise ValueError("name is not normalized")
        except ValueError as e:
            logger.warning("Ignoring stored preset: %s", e)
            continue
        if isinstance(values, dict):
            presets[name] = _clean_values(values)
    return presets


def get_user_presets():
    with _lock:
        return _read_user_presets()


def find_preset(name):
    """Return the DSP values for a built-in or user preset, or None."""
    if name in BUILTIN_PRESETS:
        return dict(BUILTIN_PRESETS[name])
    with _lock:
        return _read_user_presets().get(name)


def save_user_preset(name, values):
    with _lock:
        presets = _read_user_presets()
        presets[name] = _clean_values(values)
        atomic_write_json(USER_PRESETS_FILE, presets)
        return presets[name]


def delete_user_preset(name):
    with _lock:
        presets = _read_user_presets()
        if name not in presets:
            return False
        del presets[name]
        atomic_write_json(USER_PRESETS_FILE, presets)
        return True
