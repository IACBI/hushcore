import os
import json

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
    "buffer_size": "auto"
}

def load_settings():
    if not os.path.exists(SETTINGS_FILE):
        return DEFAULT_SETTINGS.copy()
    try:
        with open(SETTINGS_FILE, "r") as f:
            settings = json.load(f)
            # Merge with defaults to ensure all keys exist
            merged = DEFAULT_SETTINGS.copy()
            merged.update(settings)
            return merged
    except Exception as e:
        print(f"Error loading settings: {e}")
        return DEFAULT_SETTINGS.copy()

def save_settings(settings):
    try:
        with open(SETTINGS_FILE, "w") as f:
            json.dump(settings, f, indent=4)
    except Exception as e:
        print(f"Error saving settings: {e}")
