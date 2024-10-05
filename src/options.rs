use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};

use crate::{
    core::{consts::*, frame_processing::*}, image::raw::FrameType
};

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct IndiOptions {
    pub mount:    Option<String>,
    pub camera:   Option<String>,
    pub guid_cam: Option<String>,
    pub focuser:  Option<String>,
    pub remote:   bool,
    pub address:  String,
}

impl Default for IndiOptions {
    fn default() -> Self {
        Self {
            mount:    None,
            camera:   None,
            guid_cam: None,
            focuser:  None,
            remote:   false,
            address:  "localhost".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Gain {
    #[default]Same,
    Min,
    P25,
    P50,
    P75,
    Max
}

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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct CamCtrlOptions {
    pub enable_cooler: bool,
    pub enable_fan:    bool,
    pub heater_str:    Option<String>,
    pub temperature:   f64,
}

impl Default for CamCtrlOptions {
    fn default() -> Self {
        Self {
            enable_cooler: false,
            enable_fan:    false,
            heater_str:    None,
            temperature:   0.0,
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
    pub low_noise:  bool,
    pub delay:      f64,
}

impl Default for FrameOptions {
    fn default() -> Self {
        Self {
            exp_main:   2.0,
            exp_bias:   0.01,
            exp_flat:   0.5,
            gain:       1.0,
            offset:     0,
            frame_type: FrameType::default(),
            binning:    Binning::default(),
            crop:       Crop::default(),
            low_noise:  false,
            delay:      1.0,
        }
    }
}

impl FrameOptions {
    pub fn have_to_use_delay(&self) -> bool {
        self.exposure() < 2.0 &&
        self.delay > 0.0
    }

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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct CalibrOptions {
    pub dark_library_path: PathBuf,
    pub dark_frame_en:     bool,
    pub flat_frame_en:     bool,
    pub flat_frame_fname:  Option<PathBuf>,
    pub hot_pixels:        bool,
}

impl Default for CalibrOptions {
    fn default() -> Self {
        Self {
            dark_library_path: PathBuf::new(),
            dark_frame_en:     true,
            flat_frame_en:     false,
            flat_frame_fname:  None,
            hot_pixels:        true,
        }
    }
}

impl CalibrOptions {
    pub fn check(&mut self) -> anyhow::Result<()> {
        if self.dark_library_path.as_os_str().is_empty() {
            let mut dark_lib_path = dirs::home_dir().unwrap();
            dark_lib_path.push(DIRECTORY);
            dark_lib_path.push("DarksLibrary");
            if !dark_lib_path.is_dir() {
                std::fs::create_dir_all(&dark_lib_path)?;
            }
            self.dark_library_path = dark_lib_path;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RawFrameOptions {
    pub out_path:      PathBuf,
    pub frame_cnt:     usize,
    pub use_cnt:       bool,
    pub create_master: bool,
}

impl Default for RawFrameOptions {
    fn default() -> Self {
        Self {
            out_path:      PathBuf::new(),
            frame_cnt:     100,
            use_cnt:       true,
            create_master: true,
        }
    }
}

impl RawFrameOptions {
    pub fn check(&mut self) -> anyhow::Result<()> {
        if self.out_path.as_os_str().is_empty() {
            let mut out_path = dirs::home_dir().unwrap();
            out_path.push(DIRECTORY);
            out_path.push(RAW_FRAMES_DIR);
            if !out_path.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            }
            self.out_path = out_path;
        }
        Ok(())
    }
}


#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct LiveStackingOptions {
    pub save_orig:    bool,
    pub save_minutes: usize,
    pub save_enabled: bool,
    pub out_dir:      PathBuf,
}

impl Default for LiveStackingOptions {
    fn default() -> Self {
        Self {
            save_orig:    false,
            save_minutes: 5,
            save_enabled: true,
            out_dir:      PathBuf::new(),
        }
    }
}

impl LiveStackingOptions {
    pub fn check(&mut self) -> anyhow::Result<()> {
        if self.out_dir.as_os_str().is_empty() {
            let mut save_path = dirs::home_dir().unwrap();
            save_path.push(DIRECTORY);
            save_path.push(LIVE_STACKING_DIR);
            if !save_path.is_dir() {
                std::fs::create_dir_all(&save_path)?;
            }
            self.out_dir = save_path;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct QualityOptions {
    pub use_max_fwhm:    bool,
    pub max_fwhm:        f32,
    pub use_max_ovality: bool,
    pub max_ovality:     f32,
}

impl Default for QualityOptions {
    fn default() -> Self {
        Self {
            use_max_fwhm:    false,
            max_fwhm:        20.0,
            use_max_ovality: true,
            max_ovality:     1.0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone, Copy)]
pub enum PreviewColor { #[default]Rgb, Red, Green, Blue }

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub enum PreviewSource {#[default]OrigFrame, LiveStacking}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub enum PreviewScale {#[default]FitWindow, Original, P75, P50, P33, P25}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PreviewOptions {
    pub scale:       PreviewScale,
    pub dark_lvl:    f64,
    pub light_lvl:   f64,
    pub gamma:       f64,
    pub source:      PreviewSource,
    pub remove_grad: bool,

    #[serde(skip_serializing)]
    pub color:       PreviewColor,

    // fields for PreviewOptions::preview_params
    #[serde(skip_serializing)] pub widget_width: usize,
    #[serde(skip_serializing)] pub widget_height: usize,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            scale:         PreviewScale::default(),
            dark_lvl:      0.2,
            light_lvl:     0.8,
            gamma:         2.2,
            source:        PreviewSource::default(),
            remove_grad:   false,
            widget_width:  0,
            widget_height: 0,
            color:         PreviewColor::Rgb,
        }
    }
}

impl PreviewOptions {
    pub fn preview_params(&self) -> PreviewParams {
        let img_size = if self.scale == PreviewScale::FitWindow {
            PreviewImgSize::Fit {
                width: self.widget_width,
                height: self.widget_height
            }
        } else {
            PreviewImgSize::Scale(self.scale.clone())
        };
        PreviewParams {
            dark_lvl:         self.dark_lvl,
            light_lvl:        self.light_lvl,
            gamma:            self.gamma,
            orig_frame_in_ls: self.source == PreviewSource::OrigFrame,
            remove_gradient:  self.remove_grad,
            img_size,
            color:            self.color
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct FocuserOptions {
    pub device:          String,
    pub on_temp_change:  bool,
    pub max_temp_change: f64,
    pub on_fwhm_change:  bool,
    pub max_fwhm_change: u32,
    pub periodically:    bool,
    pub period_minutes:  u32,
    pub measures:        u32,
    pub step:            f64,
    pub exposure:        f64,
    pub gain:            Gain,
}

impl Default for FocuserOptions {
    fn default() -> Self {
        Self {
            device:          String::new(),
            on_temp_change:  false,
            max_temp_change: 5.0,
            on_fwhm_change:  false,
            max_fwhm_change: 20,
            periodically:    false,
            period_minutes:  120,
            measures:        11,
            step:            2000.0,
            exposure:        2.0,
            gain:            Gain::default(),
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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PlateSolveOptions {
    pub exposure: f64,
    pub gain: Gain,
    pub bin: Binning,
}

impl Default for PlateSolveOptions {
    fn default() -> Self {
        Self {
            exposure: 3.0,
            gain: Gain::Same,
            bin: Binning::Bin2,
        }
    }
}


#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct MountOptions {
    pub device: String,
    pub inv_ns: bool,
    pub inv_we: bool,
    pub speed:  Option<String>,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            device: String::new(),
            inv_ns: false,
            inv_we: false,
            speed:  None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct TelescopeOptions {
    pub focal_len: f64,
    pub barlow:    f64,
}

impl Default for TelescopeOptions {
    fn default() -> Self {
        Self {
            focal_len: 750.0,
            barlow:    1.0,
        }
    }
}

impl TelescopeOptions {
    pub fn real_focal_length(&self) -> f64 {
        self.focal_len * self.barlow
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
pub struct CamOptions {
    pub device:    Option<DeviceAndProp>,
    pub live_view: bool,
    pub ctrl:      CamCtrlOptions,
    pub frame:     FrameOptions,
}

impl Default for CamOptions {
    fn default() -> Self {
        Self {
            device:    None,
            live_view: false,
            ctrl:      CamCtrlOptions::default(),
            frame:     FrameOptions::default(),
        }
    }
}

impl CamOptions {
    pub fn calc_active_zone_mm(
        &self,
        sensor_width:    usize,
        sensor_height:   usize,
        pixel_width_um:  f64,
        pixel_height_um: f64
    ) -> (f64, f64) {
        let bin = self.frame.binning.get_ratio();
        let cropped_width = self.frame.crop.translate(sensor_width / bin) as f64;
        let cropped_height = self.frame.crop.translate(sensor_height / bin) as f64;
        let pixel_width_mm = pixel_width_um / 1000.0;
        let pixel_height_mm = pixel_height_um / 1000.0;
        let width_mm = cropped_width * pixel_width_mm;
        let height_mm = cropped_height * pixel_height_mm;
        (width_mm, height_mm)
    }

    fn type_part_of_file_name(&self) -> &'static str {
        match self.frame.frame_type {
            FrameType::Undef => unreachable!(),
            FrameType::Lights => "light",
            FrameType::Flats => "flat",
            FrameType::Darks => "dark",
            FrameType::Biases => "bias",
        }
    }

    fn commont_part_of_file_name(&self, sensor_width: usize, sensor_height: usize) -> String {
        let bin = self.frame.binning.get_ratio();
        let cropped_width = self.frame.crop.translate(sensor_width/bin);
        let cropped_height = self.frame.crop.translate(sensor_height/bin);
        let exp_to_str = |exp: f64| {
            if exp > 1.0 {
                format!("{:.0}", exp)
            } else if exp >= 0.1 {
                format!("{:.1}", exp)
            } else {
                format!("{:.3}", exp)
            }
        };
        let mut common_part = format!(
            "{}s_g{}_offs{}_{}x{}",
            exp_to_str(self.frame.exposure()),
            self.frame.gain,
            self.frame.offset,
            cropped_width,
            cropped_height,
        );
        if bin != 1 {
            common_part.push_str(&format!("_bin{}x{}", bin, bin));
        }
        common_part
    }

    fn temperature_part_of_file_name(&self, cooler_supported: bool) -> Option<String> {
        if cooler_supported && self.ctrl.enable_cooler {
            Some(format!("{:+.0}C", self.ctrl.temperature))
        } else {
            None
        }
    }

    pub fn raw_master_file_name(
        &self,
        time:             Option<DateTime<Utc>>,
        sensor_width:     usize,
        sensor_height:    usize,
        cooler_supported: bool
    ) -> String {
        let mut result = String::new();
        let type_part = self.type_part_of_file_name();
        let common_part = self.commont_part_of_file_name(sensor_width, sensor_height);
        result.push_str(type_part);
        result.push_str("_");
        result.push_str(&common_part);
        if self.frame.frame_type != FrameType::Flats {
            let temp_path = self.temperature_part_of_file_name(cooler_supported);
            if let Some(temp) = &temp_path {
                result.push_str("_");
                result.push_str(&temp);
            }
        }
        if self.frame.frame_type == FrameType::Flats {
            let time = time.expect("You must define time for master flat file!");
            let now_date_str = time.format("%Y-%m-%d").to_string();
            result.push_str("_");
            result.push_str(&now_date_str);
        }
        result.push_str(".fit");
        result
    }

    pub fn defect_pixels_file_name(
        &self,
        sensor_width:  usize,
        sensor_height: usize,
    ) -> String {
        let bin = self.frame.binning.get_ratio();
        let cropped_width = self.frame.crop.translate(sensor_width/bin);
        let cropped_height = self.frame.crop.translate(sensor_height/bin);

        let mut result = String::new();
        result.push_str("defect_pixels");
        result.push_str(&format!("_{}x{}", cropped_width, cropped_height));
        if bin != 1 {
            result.push_str(&format!("_bin{}x{}", bin, bin));
        }
        result.push_str(".txt");
        result
    }

    pub fn raw_file_dest_dir(
        &self,
        time:             DateTime<Utc>,
        sensor_width:     usize,
        sensor_height:    usize,
        cooler_supported: bool
    ) -> String {
        let mut save_dir = String::new();
        let type_part = self.type_part_of_file_name();
        let common_part = self.commont_part_of_file_name(sensor_width, sensor_height);
        save_dir.push_str(type_part);
        save_dir.push_str("_");
        let now_date_str = time.format("%Y-%m-%d").to_string();
        save_dir.push_str(&now_date_str);
        save_dir.push_str("__");
        save_dir.push_str(&common_part);
        if self.frame.frame_type != FrameType::Flats {
            let temp_path = self.temperature_part_of_file_name(cooler_supported);
            if let Some(temp) = &temp_path {
                save_dir.push_str("_");
                save_dir.push_str(&temp);
            }
        }
        save_dir
    }
}

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
            dith_dist:       100,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ExtGuiderOptions {
    pub foc_len:   f64,
    pub dith_dist: i32,   // in pixels
}

impl Default for ExtGuiderOptions {
    fn default() -> Self {
        Self {
            foc_len:   250.0,
            dith_dist: 10,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct GuidingOptions {
    pub mode:        GuidingMode,
    pub dith_period: u32,  // in minutes, 0 - do not dither
    pub main_cam:    MainCamGuidingOptions,
    pub ext_guider:  ExtGuiderOptions,
}

impl Default for GuidingOptions {
    fn default() -> Self {
        Self {
            mode:        GuidingMode::Disabled,
            dith_period: 1,
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

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SkyMapOptions {
    pub latitude:  f64,
    pub longitude: f64,
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Options {
    pub indi:        IndiOptions,
    pub cam:         CamOptions,
    pub calibr:      CalibrOptions,
    pub raw_frames:  RawFrameOptions,
    pub live:        LiveStackingOptions,
    pub quality:     QualityOptions,
    pub preview:     PreviewOptions,
    pub focuser:     FocuserOptions,
    pub plate_solve: PlateSolveOptions,
    pub mount:       MountOptions,
    pub telescope:   TelescopeOptions,
    pub guiding:     GuidingOptions,
    pub sky_map:     SkyMapOptions,
}
