use serde::{Serialize, Deserialize};

use crate::image::raw::FrameType;

use super::CalibrOptions;

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Binning {#[default]Orig, Bin2, Bin3, Bin4}

impl Binning {
    pub fn to_str(self) -> &'static str {
        match self {
            Self::Orig => "1x1",
            Self::Bin2 => "2x2",
            Self::Bin3 => "3x3",
            Self::Bin4 => "4x4",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Crop {#[default]None, P75, P50, P33, P25}

impl Crop {
    pub fn translate(&self, value: usize) -> usize {
        match self {
            Crop::None => value,
            Crop::P75  => 3 * value / 4,
            Crop::P50  => value / 2,
            Crop::P33  => value / 3,
            Crop::P25  => value / 4,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::P75 => "75%",
            Self::P50 => "50%",
            Self::P33 => "33.3%",
            Self::P25 => "25%",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
#[serde(default)]
pub struct DeviceAndProp {
    pub name: String,
    pub prop: String, // CCD1, CCD2... or emprty for any
}

impl DeviceAndProp {
    pub fn new(text: &str) -> Self {
        let mut result = Self::default();
        let mut splitted = text.split(" | ");
        if let Some(name) = splitted.next() {
            result.name = name.trim().to_string();
            result.prop = if let Some(prop) = splitted.next() {
                prop.trim().to_string()
            } else {
                "CCD1".to_string()
            };
        }
        result
    }

    pub fn to_string(&self) -> String {
        let mut result = self.name.clone();
        if !result.is_empty() && !self.prop.is_empty() && self.prop != "CCD1" {
            result += " | ";
            result += &self.prop;
        }
        result
    }

    pub fn to_file_name_part(&self) -> String {
        let mut result = self.name.clone();
        if !result.is_empty() && !self.prop.is_empty() && self.prop != "CCD1" {
            result += "_";
            result += &self.prop;
        }
        result
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct CamCtrlOptions {
    pub enable_cooler: bool,
    pub enable_fan:    bool,
    pub heater_str:    Option<String>,
    pub temperature:   f64,
    pub low_noise:     bool,
    pub high_fullwell: bool,
    pub conv_gain_str: Option<String>,

}

impl Default for CamCtrlOptions {
    fn default() -> Self {
        Self {
            enable_cooler: false,
            enable_fan:    false,
            heater_str:    None,
            temperature:   0.0,
            low_noise:     false,
            high_fullwell: false,
            conv_gain_str: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct FrameOptions {
    pub exp_main:   f64,
    pub exp_bias:   f64,
    pub exp_flat:   f64,
    pub gain:       f64,
    pub offset:     i32,
    pub frame_type: FrameType,
    pub binning:    Binning,
    pub crop:       Crop,
}

impl Default for FrameOptions {
    fn default() -> Self {
        Self {
            exp_main:   2.0,
            exp_bias:   0.0001,
            exp_flat:   0.5,
            gain:       1.0,
            offset:     0,
            frame_type: FrameType::default(),
            binning:    Binning::default(),
            crop:       Crop::default(),
        }
    }
}

impl FrameOptions {
    pub fn exposure(&self) -> f64 {
        match self.frame_type {
            FrameType::Flats  => self.exp_flat,
            FrameType::Biases => self.exp_bias,
            _                 => self.exp_main,
        }
    }

    pub fn set_exposure(&mut self, value: f64) {
        match self.frame_type {
            FrameType::Flats  => self.exp_flat = value,
            FrameType::Biases => self.exp_bias = value,
            _                 => self.exp_main = value,
        }
    }

    pub fn active_sensor_size(&self, sensor_width: usize, sensor_height: usize) -> (usize, usize) {
        let cropped_width = self.crop.translate(sensor_width);
        let cropped_height = self.crop.translate(sensor_height);
        let bin = self.binning.get_ratio();
        (cropped_width/bin, cropped_height/bin)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CamOptions {
    pub device:    Option<DeviceAndProp>,
    pub live_view: bool,
    pub ctrl:      CamCtrlOptions,
    pub frame:     FrameOptions,
}

impl CamOptions {
    pub fn calc_active_zone_mm(
        &self,
        sensor_width:    usize,
        sensor_height:   usize,
        pixel_width_um:  f64,
        pixel_height_um: f64
    ) -> (f64, f64) {
        let cropped_width = self.frame.crop.translate(sensor_width) as f64;
        let cropped_height = self.frame.crop.translate(sensor_height) as f64;
        let pixel_width_mm = pixel_width_um / 1000.0;
        let pixel_height_mm = pixel_height_um / 1000.0;
        let width_mm = cropped_width * pixel_width_mm;
        let height_mm = cropped_height * pixel_height_mm;
        (width_mm, height_mm)
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct SeparatedCamOptions {
    pub frame:  FrameOptions,
    pub ctrl:   CamCtrlOptions,
    pub calibr: CalibrOptions,
}
