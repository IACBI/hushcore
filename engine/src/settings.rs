use serde::{Deserialize, Serialize};

/// Flat settings, key-compatible with backend/config.py DEFAULT_SETTINGS.
/// Unknown keys are ignored; missing keys take the defaults below.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub input_gain: f32,
    pub output_gain: f32,
    pub ns_mode: String, // "off" | "eco" | "high" (high == eco until the native DeepFilterNet path lands)
    pub ns_eco_strength: f32,
    pub ns_high_strength: f32,
    pub gate_enabled: bool,
    pub gate_threshold_db: f32,
    pub gate_release_ms: f32,
    pub eq_enabled: bool,
    pub eq_low_gain_db: f32,
    pub eq_mid_gain_db: f32,
    pub eq_high_gain_db: f32,
    pub compressor_enabled: bool,
    pub compressor_threshold_db: f32,
    pub compressor_ratio: f32,
    pub compressor_attack_ms: f32,
    pub compressor_release_ms: f32,
    pub limiter_threshold_db: f32,
    pub deesser_enabled: bool,
    pub deesser_threshold_db: f32,
    pub deesser_amount: f32,
    pub exciter_enabled: bool,
    pub exciter_frequency: f32,
    pub exciter_amount: f32,
    pub exciter_mix: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            input_gain: 1.0,
            output_gain: 1.0,
            ns_mode: "eco".into(),
            ns_eco_strength: 0.8,
            ns_high_strength: 1.0,
            gate_enabled: true,
            gate_threshold_db: -45.0,
            gate_release_ms: 150.0,
            eq_enabled: true,
            eq_low_gain_db: 0.0,
            eq_mid_gain_db: 0.0,
            eq_high_gain_db: 0.0,
            compressor_enabled: true,
            compressor_threshold_db: -18.0,
            compressor_ratio: 3.0,
            compressor_attack_ms: 10.0,
            compressor_release_ms: 100.0,
            limiter_threshold_db: -1.0,
            deesser_enabled: false,
            deesser_threshold_db: -25.0,
            deesser_amount: 0.5,
            exciter_enabled: false,
            exciter_frequency: 3000.0,
            exciter_amount: 0.2,
            exciter_mix: 0.15,
        }
    }
}
