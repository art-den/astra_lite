use serde::{Serialize, Deserialize};

/// Stars Recognition Sensitivity
#[derive(Default, Serialize, Deserialize, Debug, Clone, Copy)]
pub enum StarRecognSensitivity {
    Low,
    #[default]
    Normal,
    High
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct QualityOptions {
    pub use_max_fwhm:       bool,
    pub max_fwhm:           f32,
    pub use_max_ovality:    bool,
    pub max_ovality:        f32,
    pub ignore_3px_stars:   bool,
    pub star_recogn_sens:   StarRecognSensitivity,
    pub check_ccd_temp:     bool,
    pub max_ccd_temp_diff:  f64,
}

impl Default for QualityOptions {
    fn default() -> Self {
        Self {
            use_max_fwhm:       false,
            max_fwhm:           5.0,
            use_max_ovality:    false,
            max_ovality:        2.0,
            ignore_3px_stars:   true,
            star_recogn_sens:   StarRecognSensitivity::default(),
            check_ccd_temp:     true,
            max_ccd_temp_diff:  1.0,
        }
    }
}

impl QualityOptions {
    pub fn is_used_for_light_frames(&self) -> bool {
        self.use_max_fwhm || self.use_max_ovality
    }

    pub fn is_used_for_raw(&self) -> bool {
        self.check_ccd_temp
    }
}
