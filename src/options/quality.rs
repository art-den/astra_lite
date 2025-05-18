use serde::{Serialize, Deserialize};

#[derive(Default, Serialize, Deserialize, Debug, Clone, Copy)]
pub enum StarRecognSensivity {
    Low,
    #[default]
    Normal,
    High
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct QualityOptions {
    pub use_max_fwhm:     bool,
    pub max_fwhm:         f32,
    pub use_max_ovality:  bool,
    pub max_ovality:      f32,
    pub ignore_3px_stars: bool,
    pub star_recgn_sens:  StarRecognSensivity,
}

impl Default for QualityOptions {
    fn default() -> Self {
        Self {
            use_max_fwhm:     false,
            max_fwhm:         5.0,
            use_max_ovality:  false,
            max_ovality:      2.0,
            ignore_3px_stars: true,
            star_recgn_sens:  StarRecognSensivity::default(),
        }
    }
}
