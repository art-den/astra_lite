use std::path::PathBuf;

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
    pub save_orig:     bool,
    pub save_minutes:  usize,
    pub save_enabled:  bool,
    pub out_dir:       PathBuf,
    pub remove_tracks: bool,
}

impl Default for LiveStackingOptions {
    fn default() -> Self {
        Self {
            save_orig:     false,
            save_minutes:  5,
            save_enabled:  true,
            out_dir:       PathBuf::new(),
            remove_tracks: false,
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
    pub wb_auto:     bool,
    pub wb_red:      f64,
    pub wb_green:    f64,
    pub wb_blue:     f64,

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
            wb_auto:       true,
            wb_red:        1.0,
            wb_green:      1.0,
            wb_blue:       1.0,
            color:         PreviewColor::Rgb,
            widget_width:  0,
            widget_height: 0,
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

#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy)]
pub enum PlateSolverType {
    #[default]
    Astrometry,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PlateSolverOptions {
    pub solver: PlateSolverType,
    pub exposure: f64,
    pub gain: Gain,
    pub bin: Binning,
    pub timeout: u32,
    pub blind_timeout: u32,
}

impl Default for PlateSolverOptions {
    fn default() -> Self {
        Self {
            solver: PlateSolverType::default(),
            exposure: 3.0,
            gain: Gain::Same,
            bin: Binning::Bin2,
            timeout: 10,
            blind_timeout: 30,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct SiteOptions {
    pub latitude:  f64,
    pub longitude: f64,
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
        let cropped_width = self.frame.crop.translate(sensor_width) as f64;
        let cropped_height = self.frame.crop.translate(sensor_height) as f64;
        let pixel_width_mm = pixel_width_um / 1000.0;
        let pixel_height_mm = pixel_height_um / 1000.0;
        let width_mm = cropped_width * pixel_width_mm;
        let height_mm = cropped_height * pixel_height_mm;
        (width_mm, height_mm)
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
            dith_dist:       50,
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PloarAlignDir {
    East,
    West,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PloarAlignOptions {
    pub angle:     f64,
    pub direction: PloarAlignDir,
    pub speed:     Option<String>,
    pub sim_alt_err:   f64,
    pub sim_az_err:    f64,
}

impl Default for PloarAlignOptions {
    fn default() -> Self {
        Self {
            angle:       30.0,
            direction:   PloarAlignDir::West,
            speed:       None,
            sim_alt_err: 1.1,
            sim_az_err:  1.4,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Options {
    pub indi:         IndiOptions,
    pub cam:          CamOptions,
    pub calibr:       CalibrOptions,
    pub raw_frames:   RawFrameOptions,
    pub live:         LiveStackingOptions,
    pub quality:      QualityOptions,
    pub preview:      PreviewOptions,
    pub focuser:      FocuserOptions,
    pub plate_solver: PlateSolverOptions,
    pub mount:        MountOptions,
    pub telescope:    TelescopeOptions,
    pub site:         SiteOptions,
    pub guiding:      GuidingOptions,
    pub polar_align:  PloarAlignOptions,
}
