use std::path::PathBuf;

use serde::{Serialize, Deserialize};

use crate::{image_raw::FrameType, image_processing::CalibrParams};

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct IndiOptions {
    pub mount:   Option<String>,
    pub camera:  Option<String>,
    pub focuser: Option<String>,
    pub remote:  bool,
    pub address: String,
}

impl Default for IndiOptions {
    fn default() -> Self {
        Self {
            mount:   None,
            camera:  None,
            focuser: None,
            remote:  false,
            address: "127.0.0.1".to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Binning {#[default]Orig, Bin2, Bin3, Bin4}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone)]
pub enum Crop {#[default]None, P75, P50, P33, P25}

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
    pub exposure:   f64,
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
            exposure:   5.0,
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
    pub fn create_master_dark_file_name_suff(&self) -> String {
        format!("{:.1}s_g{:.0}_ofs{}", self.exposure, self.gain, self.offset)
    }

    pub fn create_master_flat_file_name_suff(&self) -> String {
        format!("g{:.0}_ofs{}", self.gain, self.offset)
    }

    pub fn create_master_bias_file_name_suff(&self) -> String {
        format!("{:.1}s_g{:.0}_ofs{}", self.exposure, self.gain, self.offset)
    }

    pub fn have_to_use_delay(&self) -> bool {
        self.exposure < 2.0 &&
        self.delay > 0.0
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct CalibrOptions {
    pub dark_frame_en: bool,
    pub dark_frame:    Option<PathBuf>,
    pub flat_frame_en: bool,
    pub flat_frame:    Option<PathBuf>,
    pub hot_pixels:    bool,
}

impl Default for CalibrOptions {
    fn default() -> Self {
        Self {
            dark_frame_en: true,
            dark_frame:    None,
            flat_frame_en: true,
            flat_frame:    None,
            hot_pixels:    true,
        }
    }
}

impl CalibrOptions {
    pub fn into_params(&self) -> CalibrParams {
        let dark = if self.dark_frame_en {
            self.dark_frame.clone()
        } else {
            None
        };
        let flat = if self.flat_frame_en {
            self.flat_frame.clone()
        } else {
            None
        };
        CalibrParams {
            dark,
            flat,
            hot_pixels: self.hot_pixels
        }
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
    pub fn check_and_correct(&mut self) -> anyhow::Result<()> {
        if self.out_path.as_os_str().is_empty() {
            let mut out_path = dirs::home_dir().unwrap();
            out_path.push("Astro");
            out_path.push("RawFrames");
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
    pub fn check_and_correct(&mut self) -> anyhow::Result<()> {
        if self.out_dir.as_os_str().is_empty() {
            let mut save_path = dirs::home_dir().unwrap();
            save_path.push("Astro");
            save_path.push("LiveStaking");
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
            max_ovality:     0.5,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub enum PreviewSource {#[default]OrigFrame, LiveStacking}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub enum ImgPreviewScale {#[default]FitWindow, Original, P75, P50, P33, P25}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PreviewOptions {
    pub scale:       ImgPreviewScale,
    pub dark_lvl:    f64,
    pub light_lvl:   f64,
    pub gamma:       f64,
    pub source:      PreviewSource,
    pub remove_grad: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            scale:       ImgPreviewScale::default(),
            dark_lvl:    0.2,
            light_lvl:   0.8,
            gamma:       2.2,
            source:      PreviewSource::default(),
            remove_grad: false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct HistOptions {
    pub log_x:    bool,
    pub log_y:    bool,
    pub percents: bool,
}

impl Default for HistOptions {
    fn default() -> Self {
        Self {
            log_x:    false,
            log_y:    false,
            percents: true,
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
pub struct GuidingOptions {
    pub enabled:         bool,
    pub max_error:       f64,
    pub dith_period:     u32, // in minutes, 0 - do not dither
    pub dith_percent:    f64, // percent of image
    pub calibr_exposure: f64,
}

impl Default for GuidingOptions {
    fn default() -> Self {
        Self {
            enabled:         false,
            max_error:       5.0,
            dith_period:     0,
            dith_percent:    5.0,
            calibr_exposure: 2.0,
        }
    }
}

impl GuidingOptions {
    pub fn is_used(&self) -> bool {
        self.enabled ||
        self.dith_period != 0
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

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct CamOptions {
    pub device:     String,
    pub live_view:  bool,
    pub ctrl:       CamCtrlOptions,
    pub frame:      FrameOptions,
    pub calibr:     CalibrOptions,
    pub raw_frames: RawFrameOptions,
    pub live:       LiveStackingOptions,
    pub quality:    QualityOptions,
    pub preview:    PreviewOptions,
}

impl Default for CamOptions {
    fn default() -> Self {
        Self {
            device:     String::new(),
            live_view:  false,
            preview:    PreviewOptions::default(),
            ctrl:       CamCtrlOptions::default(),
            frame:      FrameOptions::default(),
            calibr:     CalibrOptions::default(),
            raw_frames: RawFrameOptions::default(),
            live:       LiveStackingOptions::default(),
            quality:    QualityOptions::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Options {
    pub indi:      IndiOptions,
    pub cam:       CamOptions,
    pub hist:      HistOptions,
    pub focuser:   FocuserOptions,
    pub guiding:   GuidingOptions,
    pub mount:     MountOptions,
    pub telescope: TelescopeOptions,
}
