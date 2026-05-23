use serde::{Serialize, Deserialize};
use crate::hal::FrameType;

use super::CalibrOptions;

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Binning {#[default]Orig, Bin2, Bin4}

impl FrameType {
    pub fn from_str(text: &str, def: FrameType) -> Self {
        match text {
            "Light" => FrameType::Lights,
            "Flat"  => FrameType::Flats,
            "Dark"  => FrameType::Darks,
            "Bias"  => FrameType::Biases,
            _       => def,
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            FrameType::Lights => "Light",
            FrameType::Flats  => "Flat",
            FrameType::Darks  => "Dark",
            FrameType::Biases => "Bias",
        }
    }

    pub fn to_readable_str(&self) -> &'static str {
        match self {
            FrameType::Lights => "Saving LIGHT frames",
            FrameType::Flats  => "Saving FLAT frames",
            FrameType::Darks  => "Saving DARK frames",
            FrameType::Biases => "Saving BIAS frames",
        }
    }
}

impl Binning {
    pub fn to_str(self) -> &'static str {
        match self {
            Self::Orig => "1x1",
            Self::Bin2 => "2x2",
            Self::Bin4 => "4x4",
        }
    }

    pub fn get_ratio(&self) -> usize {
        match self {
            Self::Orig => 1,
            Self::Bin2 => 2,
            Self::Bin4 => 4,
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
    pub const CCD2_POSTFIX: &str = "_CCD2";

    pub fn new(mut text: &str) -> Self {
        let prop = if text.ends_with(Self::CCD2_POSTFIX) {
            let new_len = text.len() - Self::CCD2_POSTFIX.len();
            text = &text[..new_len];
            "CCD2"
        } else {
            "CCD1"
        };

        Self {
            name: text.to_string(),
            prop: prop.to_string(),
        }
    }

    pub fn to_string(&self) -> String {
        let mut result = self.name.clone();
        if !result.is_empty() && !self.prop.is_empty() && self.prop != "CCD1" {
            result += "_";
            result += &self.prop;
        }
        result
    }

    // TODO: remove!
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
    pub auto_exp:   bool, // only for flats
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
            auto_exp:   true,
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
    pub device_id: String,
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
