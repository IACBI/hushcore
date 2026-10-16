import json
import logging
import math
import os
import tempfile

logger = logging.getLogger(__name__)

SETTINGS_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "settings.json")

DEFAULT_SETTINGS = {
    "input_device": "",
    "output_device": "",
    "monitor_device": "",
    "monitor_enabled": False,
    "input_gain": 1.0,
    "output_gain": 1.0,

    # Noise Suppression
    "ns_mode": "eco",  # "off", "eco", "high"
    "ns_eco_strength": 0.8,
    "ns_high_strength": 1.0,

    # Noise Gate
    "gate_enabled": True,
    "gate_threshold_db": -45.0,
    "gate_release_ms": 150.0,

    # Equalizer (3-band)
    "eq_enabled": True,
    "eq_low_gain_db": 0.0,
    "eq_mid_gain_db": 0.0,
    "eq_high_gain_db": 0.0,

    # Compressor
    "compressor_enabled": True,
    "compressor_threshold_db": -18.0,
    "compressor_ratio": 3.0,
    "compressor_attack_ms": 10.0,
    "compressor_release_ms": 100.0,

    # Limiter
    "limiter_threshold_db": -1.0,

    # De-Esser
    "deesser_enabled": False,
    "deesser_threshold_db": -25.0,
    "deesser_amount": 0.5,

    # Vocal Exciter
    "exciter_enabled": False,
    "exciter_frequency": 3000.0,
    "exciter_amount": 0.2,
    "exciter_mix": 0.15,

    # VST3 Hosting
    "vst_enabled": False,
    "vst_path": "",

    # Latency Tuning
    "buffer_size": "auto",

    # Native engine stream mode; the Python engine ignores it
    "audio_mode": "shared",
}

# Numeric keys carry min/max; enum keys carry choices. Booleans and strings
# are checked by type alone.
SCHEMA = {
    "input_gain": {"min": 0.0, "max": 4.0},
    "output_gain": {"min": 0.0, "max": 4.0},
    "ns_mode": {"choices": ("off", "eco", "high")},
    "ns_eco_strength": {"min": 0.0, "max": 1.0},
    "ns_high_strength": {"min": 0.0, "max": 1.0},
    "gate_threshold_db": {"min": -80.0, "max": 0.0},
    "gate_release_ms": {"min": 10.0, "max": 1000.0},
    "eq_low_gain_db": {"min": -12.0, "max": 12.0},
    "eq_mid_gain_db": {"min": -12.0, "max": 12.0},
    "eq_high_gain_db": {"min": -12.0, "max": 12.0},
    "compressor_threshold_db": {"min": -60.0, "max": 0.0},
    "compressor_ratio": {"min": 1.0, "max": 20.0},
    "compressor_attack_ms": {"min": 1.0, "max": 200.0},
    "compressor_release_ms": {"min": 10.0, "max": 1000.0},
    "limiter_threshold_db": {"min": -20.0, "max": 0.0},
    "deesser_threshold_db": {"min": -60.0, "max": 0.0},
    "deesser_amount": {"min": 0.0, "max": 1.0},
    "exciter_frequency": {"min": 1000.0, "max": 8000.0},
    "exciter_amount": {"min": 0.0, "max": 1.0},
    "exciter_mix": {"min": 0.0, "max": 1.0},
    "buffer_size": {"choices": ("auto", "64", "128", "256", "512")},
    "audio_mode": {"choices": ("shared", "exclusive")},
}

MAX_STRING_LENGTH = 512
_TRUE_STRINGS = {"true", "1", "yes"}
_FALSE_STRINGS = {"false", "0", "no"}


def _coerce_bool(value):
    if isinstance(value, bool):
        return value, None
    if isinstance(value, int) and value in (0, 1):
        return bool(value), None
    if isinstance(value, str):
        text = value.strip().lower()
        if text in _TRUE_STRINGS:
            return True, None
        if text in _FALSE_STRINGS:
            return False, None
    return None, "expected a boolean"


def _coerce_number(value, bounds):
    if isinstance(value, bool):
        return None, "expected a number"
    try:
        if isinstance(value, str):
            number = float(value)
        elif isinstance(value, (int, float)):
            number = float(value)
        else:
            return None, "expected a number"
    except (ValueError, OverflowError):
        return None, "expected a number"
    if not math.isfinite(number):
        return None, "must be a finite number"
    if bounds:
        # Clamp instead of rejecting so slider overshoot still applies
        number = min(max(number, bounds["min"]), bounds["max"])
    return number, None


def _coerce_choice(value, choices):
    if isinstance(value, bool) or not isinstance(value, (str, int)):
        return None, f"expected one of {', '.join(choices)}"
    text = str(value).strip()
    if text in choices:
        return text, None
    return None, f"expected one of {', '.join(choices)}"


def _coerce_string(value):
    if not isinstance(value, str):
        return None, "expected a string"
    if len(value) > MAX_STRING_LENGTH:
        return None, f"longer than {MAX_STRING_LENGTH} characters"
    return value, None


def validate_and_coerce(key, value):
    """Return (coerced_value, None) if valid, otherwise (None, reason).

    Numeric values outside their range are clamped, not rejected.
    """
    if not isinstance(key, str) or key not in DEFAULT_SETTINGS:
        return None, f"Invalid setting key: {key!r}"

    default = DEFAULT_SETTINGS[key]
    spec = SCHEMA.get(key, {})

    if "choices" in spec:
        result, error = _coerce_choice(value, spec["choices"])
    elif isinstance(default, bool):
        result, error = _coerce_bool(value)
    elif isinstance(default, float):
        result, error = _coerce_number(value, spec)
    elif isinstance(default, str):
        result, error = _coerce_string(value)
    else:
        return None, f"Unsupported type for setting: {key}"

    if error is not None:
        return None, f"Invalid value for {key}: {error}"
    return result, None


def atomic_write_json(path, data):
    # Write to a sibling temp file and rename, so a crash mid-write never
    # leaves a truncated file behind.
    directory = os.path.dirname(os.path.abspath(path))
    fd, tmp_path = tempfile.mkstemp(dir=directory, prefix=".tmp-", suffix=".json")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=4)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp_path, path)
    except BaseException:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise


def load_settings():
    settings = DEFAULT_SETTINGS.copy()
    if not os.path.exists(SETTINGS_FILE):
        return settings
    try:
        with open(SETTINGS_FILE, "r", encoding="utf-8") as f:
            stored = json.load(f)
    except Exception as e:
        logger.error("Could not read settings file, using defaults: %s", e)
        return settings

    if not isinstance(stored, dict):
        logger.error("Settings file does not contain a JSON object, using defaults")
        return settings

    for key, value in stored.items():
        coerced, error = validate_and_coerce(key, value)
        if error is None:
            settings[key] = coerced
        else:
            logger.warning("Ignoring stored setting: %s", error)
    return settings


def save_settings(settings):
    data = {key: settings[key] for key in DEFAULT_SETTINGS if key in settings}
    try:
        atomic_write_json(SETTINGS_FILE, data)
        return True
    except OSError as e:
        logger.error("Could not save settings: %s", e)
        return False
