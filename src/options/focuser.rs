use serde::{Serialize, Deserialize};

use super::Gain;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct FocuserOptions {
    pub device:              String,
    pub on_temp_change:      bool,
    pub max_temp_change:     f64,
    pub on_fwhm_change:      bool,
    pub max_fwhm_change:     u32,
    pub periodically:        bool,
    pub period_minutes:      u32,
    pub measures:            u32,
    pub step:                f64,
    pub exposure:            f64,
    pub gain:                Gain,
    pub anti_backlash_steps: usize,
}

impl Default for FocuserOptions {
    fn default() -> Self {
        Self {
            device:              String::new(),
            on_temp_change:      false,
            max_temp_change:     1.0,
            on_fwhm_change:      false,
            max_fwhm_change:     20,
            periodically:        false,
            period_minutes:      60,
            measures:            11,
            step:                50.0,
            exposure:            2.0,
            gain:                Gain::default(),
            anti_backlash_steps: 500,
        }
    }
}

impl FocuserOptions {
    pub fn is_used(&self) -> bool {
        !self.device.is_empty() && (
            self.on_temp_change ||
            self.on_fwhm_change ||
            self.periodically
        )
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct SeparatedFocuserOptions {
    pub exposure: f64,
    pub gain:     Gain,
}
