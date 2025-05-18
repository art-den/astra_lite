use serde::{Serialize, Deserialize};

use super::Gain;

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub enum GuidingMode {
    #[default]
    Disabled,
    MainCamera,
    External,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct MainCamGuidingOptions {
    pub max_error:       f64,
    pub calibr_exposure: f64,
    pub calibr_gain:     Gain,
    pub dith_dist:       i32,
}

impl Default for MainCamGuidingOptions {
    fn default() -> Self {
        Self {
            max_error:       3.0,
            calibr_exposure: 2.0,
            calibr_gain:     Gain::default(),
            dith_dist:       50,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ExtGuiderOptions {
    pub dith_dist: i32,   // in pixels
}

impl Default for ExtGuiderOptions {
    fn default() -> Self {
        Self {
            dith_dist: 10,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct GuidingOptions {
    pub mode:        GuidingMode,
    pub foc_len:     f64,  // mm
    pub dith_period: u32,  // in minutes, 0 - do not dither
    pub main_cam:    MainCamGuidingOptions,
    pub ext_guider:  ExtGuiderOptions,
}

impl Default for GuidingOptions {
    fn default() -> Self {
        Self {
            mode:        GuidingMode::Disabled,
            foc_len:     250.0,
            dith_period: 2,
            main_cam:    MainCamGuidingOptions::default(),
            ext_guider:  ExtGuiderOptions::default(),
        }
    }
}

impl GuidingOptions {
    pub fn is_used(&self) -> bool {
        self.mode != GuidingMode::Disabled
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct SeparatedGuidingOptions {
    pub exposure: f64,
    pub gain:     Gain,
}
