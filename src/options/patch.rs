// NOTE: `#[serde_with::skip_serializing_none]` must be placed BEFORE `#[derive(...)]`,
// otherwise it silently has no effect (the derive expands first).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    Binning, Crop, Gain, GuidingMode, PreviewColorMode, PreviewScale, PreviewSource,
    StarRecognSensitivity,
};
use crate::hal::FrameType;
use crate::options::{
    AscomAlpacaOptions, CalibrOptions, CamCtrlOptions, CamOptions, ExtGuiderOptions,
    FocuserOptions, FrameOptions, GuidingOptions, IndiOptions, LiveStackingOptions,
    MainCamGuidingOptions, MountOptions, PreviewOptions, PlateSolverOptions,
    PlateSolverType, PolarAlignDir, PolarAlignOptions, QualityOptions, RawFrameOptions,
    SiteOptions, TelescopeOptions,
};

/// JSON `null` -> `Some(None)` (clear the option); a value -> `Some(Some(v))` (set it).
/// Missing field stays `None` (don't touch), handled by serde `default`.
fn de_opt_opt<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

macro_rules! opt_field {
    ($self:expr, $target:expr, $changed:expr, $field:ident) => {
        if let Some(v) = &$self.$field {
            if $target.$field != *v {
                $target.$field = v.clone();
                $changed = true;
            }
        }
    };
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AscomAlpacaOptionPatch {
    pub address: Option<String>,
}

impl AscomAlpacaOptionPatch {
    pub fn apply(&self, target: &mut AscomAlpacaOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, address);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndiOptionPatch {
    pub remote:  Option<bool>,
    pub address: Option<String>,
}

impl IndiOptionPatch {
    pub fn apply(&self, target: &mut IndiOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, remote);
        opt_field!(self, target, changed, address);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CamCtrlOptionPatch {
    pub enable_cooler: Option<bool>,
    pub enable_fan:    Option<bool>,
    #[serde(default, deserialize_with = "de_opt_opt")]
    pub heater_str:    Option<Option<String>>,
    pub temperature:   Option<f64>,
    pub low_noise:     Option<bool>,
    pub high_fullwell: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_opt")]
    pub conv_gain_str: Option<Option<String>>,
}

impl CamCtrlOptionPatch {
    pub fn apply(&self, target: &mut CamCtrlOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, enable_cooler);
        opt_field!(self, target, changed, enable_fan);
        opt_field!(self, target, changed, heater_str);
        opt_field!(self, target, changed, temperature);
        opt_field!(self, target, changed, low_noise);
        opt_field!(self, target, changed, high_fullwell);
        opt_field!(self, target, changed, conv_gain_str);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameOptionPatch {
    pub auto_exp:   Option<bool>,
    pub exp_main:   Option<f64>,
    pub exp_bias:   Option<f64>,
    pub exp_flat:   Option<f64>,
    pub gain:       Option<f64>,
    pub offset:     Option<i32>,
    /// Routes to the exp field matching the target's current `frame_type`
    /// (like `FrameOptions::set_exposure`); applied before a `frame_type`
    /// change in the same patch.
    pub exposure:   Option<f64>,
    pub frame_type: Option<FrameType>,
    pub binning:    Option<Binning>,
    pub crop:       Option<Crop>,
}

impl FrameOptionPatch {
    pub fn apply(&self, target: &mut FrameOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, auto_exp);
        opt_field!(self, target, changed, exp_main);
        opt_field!(self, target, changed, exp_bias);
        opt_field!(self, target, changed, exp_flat);
        opt_field!(self, target, changed, gain);
        opt_field!(self, target, changed, offset);
        if let Some(v) = self.exposure
        && target.exposure() != v {
            target.set_exposure(v);
            changed = true;
        }

        opt_field!(self, target, changed, frame_type);
        opt_field!(self, target, changed, binning);
        opt_field!(self, target, changed, crop);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CamOptionPatch {
    pub live_view: Option<bool>,
    pub ctrl:      Option<CamCtrlOptionPatch>,
    pub frame:     Option<FrameOptionPatch>,
}

impl CamOptionPatch {
    pub fn apply(&self, target: &mut CamOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, live_view);
        if let Some(ctrl) = &self.ctrl {
            changed |= ctrl.apply(&mut target.ctrl);
        }
        if let Some(frame) = &self.frame {
            changed |= frame.apply(&mut target.frame);
        }
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CalibrOptionPatch {
    pub dark_library_path: Option<PathBuf>,
    pub dark_frame_en:     Option<bool>,
    pub flat_frame_en:     Option<bool>,
    pub flat_frame_fname:  Option<PathBuf>,
    pub hot_pixels:        Option<bool>,
}

impl CalibrOptionPatch {
    pub fn apply(&self, target: &mut CalibrOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, dark_library_path);
        opt_field!(self, target, changed, dark_frame_en);
        opt_field!(self, target, changed, flat_frame_en);
        opt_field!(self, target, changed, flat_frame_fname);
        opt_field!(self, target, changed, hot_pixels);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RawFrameOptionPatch {
    pub out_path:      Option<PathBuf>,
    pub frame_cnt:     Option<usize>,
    pub use_cnt:       Option<bool>,
    pub create_master: Option<bool>,
}

impl RawFrameOptionPatch {
    pub fn apply(&self, target: &mut RawFrameOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, out_path);
        opt_field!(self, target, changed, frame_cnt);
        opt_field!(self, target, changed, use_cnt);
        opt_field!(self, target, changed, create_master);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveStackingOptionPatch {
    pub save_orig:     Option<bool>,
    pub save_minutes:  Option<usize>,
    pub save_enabled:  Option<bool>,
    pub out_dir:       Option<PathBuf>,
    pub remove_tracks: Option<bool>,
    pub use_cnt:       Option<bool>,
    pub frame_cnt:     Option<usize>,
}

impl LiveStackingOptionPatch {
    pub fn apply(&self, target: &mut LiveStackingOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, save_orig);
        opt_field!(self, target, changed, save_minutes);
        opt_field!(self, target, changed, save_enabled);
        opt_field!(self, target, changed, out_dir);
        opt_field!(self, target, changed, remove_tracks);
        opt_field!(self, target, changed, use_cnt);
        opt_field!(self, target, changed, frame_cnt);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityOptionPatch {
    pub use_max_fwhm:      Option<bool>,
    pub max_fwhm:          Option<f32>,
    pub use_max_ovality:   Option<bool>,
    pub max_ovality:       Option<f32>,
    pub ignore_3px_stars:  Option<bool>,
    pub star_recogn_sens:  Option<StarRecognSensitivity>,
    pub check_ccd_temp:    Option<bool>,
    pub max_ccd_temp_diff: Option<f64>,
}

impl QualityOptionPatch {
    pub fn apply(&self, target: &mut QualityOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, use_max_fwhm);
        opt_field!(self, target, changed, max_fwhm);
        opt_field!(self, target, changed, use_max_ovality);
        opt_field!(self, target, changed, max_ovality);
        opt_field!(self, target, changed, ignore_3px_stars);
        opt_field!(self, target, changed, star_recogn_sens);
        opt_field!(self, target, changed, check_ccd_temp);
        opt_field!(self, target, changed, max_ccd_temp_diff);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewOptionPatch {
    pub scale:       Option<PreviewScale>,
    pub dark_lvl:    Option<f64>,
    pub light_lvl:   Option<f64>,
    pub gamma:       Option<f64>,
    pub source:      Option<PreviewSource>,
    pub remove_grad: Option<bool>,
    pub wb_auto:     Option<bool>,
    pub wb_red:      Option<f64>,
    pub wb_green:    Option<f64>,
    pub wb_blue:     Option<f64>,
    pub stars:       Option<bool>,
    pub color:       Option<PreviewColorMode>,
}

impl PreviewOptionPatch {
    pub fn apply(&self, target: &mut PreviewOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, scale);
        opt_field!(self, target, changed, dark_lvl);
        opt_field!(self, target, changed, light_lvl);
        opt_field!(self, target, changed, gamma);
        opt_field!(self, target, changed, source);
        opt_field!(self, target, changed, remove_grad);
        opt_field!(self, target, changed, wb_auto);
        opt_field!(self, target, changed, wb_red);
        opt_field!(self, target, changed, wb_green);
        opt_field!(self, target, changed, wb_blue);
        opt_field!(self, target, changed, stars);
        opt_field!(self, target, changed, color);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FocuserOptionPatch {
    pub on_temp_change:      Option<bool>,
    pub max_temp_change:     Option<f64>,
    pub on_fwhm_change:      Option<bool>,
    pub max_fwhm_change:     Option<u32>,
    pub periodically:        Option<bool>,
    pub period_minutes:      Option<u32>,
    pub measures:            Option<u32>,
    pub step:                Option<f64>,
    pub exposure:            Option<f64>,
    pub gain:                Option<Gain>,
    pub anti_backlash_steps: Option<usize>,
}

impl FocuserOptionPatch {
    pub fn apply(&self, target: &mut FocuserOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, on_temp_change);
        opt_field!(self, target, changed, max_temp_change);
        opt_field!(self, target, changed, on_fwhm_change);
        opt_field!(self, target, changed, max_fwhm_change);
        opt_field!(self, target, changed, periodically);
        opt_field!(self, target, changed, period_minutes);
        opt_field!(self, target, changed, measures);
        opt_field!(self, target, changed, step);
        opt_field!(self, target, changed, exposure);
        opt_field!(self, target, changed, gain);
        opt_field!(self, target, changed, anti_backlash_steps);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlateSolverOptionPatch {
    pub solver:        Option<PlateSolverType>,
    pub exposure:      Option<f64>,
    pub gain:          Option<Gain>,
    pub bin:           Option<Binning>,
    pub timeout:       Option<u32>,
    pub blind_timeout: Option<u32>,
}

impl PlateSolverOptionPatch {
    pub fn apply(&self, target: &mut PlateSolverOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, solver);
        opt_field!(self, target, changed, exposure);
        opt_field!(self, target, changed, gain);
        opt_field!(self, target, changed, bin);
        opt_field!(self, target, changed, timeout);
        opt_field!(self, target, changed, blind_timeout);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MountOptionPatch {
    pub inv_ns: Option<bool>,
    pub inv_we: Option<bool>,
    #[serde(default, deserialize_with = "de_opt_opt")]
    pub speed:  Option<Option<String>>,
}

impl MountOptionPatch {
    pub fn apply(&self, target: &mut MountOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, inv_ns);
        opt_field!(self, target, changed, inv_we);
        opt_field!(self, target, changed, speed);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelescopeOptionPatch {
    pub focal_len:       Option<f64>,
    pub barlow:          Option<f64>,
    pub from_platesolve: Option<bool>,
}

impl TelescopeOptionPatch {
    pub fn apply(&self, target: &mut TelescopeOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, focal_len);
        opt_field!(self, target, changed, barlow);
        opt_field!(self, target, changed, from_platesolve);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteOptionPatch {
    pub latitude:  Option<f64>,
    pub longitude: Option<f64>,
}

impl SiteOptionPatch {
    pub fn apply(&self, target: &mut SiteOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, latitude);
        opt_field!(self, target, changed, longitude);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MainCamGuidingOptionPatch {
    pub max_error:       Option<f64>,
    pub calibr_exposure: Option<f64>,
    pub calibr_gain:     Option<Gain>,
    pub dith_dist:       Option<i32>,
}

impl MainCamGuidingOptionPatch {
    pub fn apply(&self, target: &mut MainCamGuidingOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, max_error);
        opt_field!(self, target, changed, calibr_exposure);
        opt_field!(self, target, changed, calibr_gain);
        opt_field!(self, target, changed, dith_dist);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtGuiderOptionPatch {
    pub dith_dist: Option<i32>,
}

impl ExtGuiderOptionPatch {
    pub fn apply(&self, target: &mut ExtGuiderOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, dith_dist);
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuidingOptionPatch {
    pub mode:      Option<GuidingMode>,
    pub foc_len:   Option<f64>,
    pub dith_period: Option<u32>,
    pub main_cam:  Option<MainCamGuidingOptionPatch>,
    pub ext_guider: Option<ExtGuiderOptionPatch>,
}

impl GuidingOptionPatch {
    pub fn apply(&self, target: &mut GuidingOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, mode);
        opt_field!(self, target, changed, foc_len);
        opt_field!(self, target, changed, dith_period);
        if let Some(main_cam) = &self.main_cam {
            changed |= main_cam.apply(&mut target.main_cam);
        }
        if let Some(ext_guider) = &self.ext_guider {
            changed |= ext_guider.apply(&mut target.ext_guider);
        }
        changed
    }
}

#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PolarAlignOptionPatch {
    pub angle:        Option<f64>,
    pub direction:    Option<PolarAlignDir>,
    #[serde(default, deserialize_with = "de_opt_opt")]
    pub speed:        Option<Option<String>>,
    pub sim_alt_err:  Option<f64>,
    pub sim_az_err:   Option<f64>,
    pub auto_refresh: Option<bool>,
}

impl PolarAlignOptionPatch {
    pub fn apply(&self, target: &mut PolarAlignOptions) -> bool {
        let mut changed = false;
        opt_field!(self, target, changed, angle);
        opt_field!(self, target, changed, direction);
        opt_field!(self, target, changed, speed);
        opt_field!(self, target, changed, sim_alt_err);
        opt_field!(self, target, changed, sim_az_err);
        opt_field!(self, target, changed, auto_refresh);
        changed
    }
}

/// Partial update of `Options`: `None` field = don't touch,
/// `Some(v)` = set, `Some(None)` (for `Option` fields) = clear.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionPatch {
    AscomAlpaca(AscomAlpacaOptionPatch),
    Indi(IndiOptionPatch),
    Cam(CamOptionPatch),
    Calibr(CalibrOptionPatch),
    RawFrames(RawFrameOptionPatch),
    #[serde(rename = "live")]
    LiveStacking(LiveStackingOptionPatch),
    Quality(QualityOptionPatch),
    Preview(PreviewOptionPatch),
    Focuser(FocuserOptionPatch),
    PlateSolver(PlateSolverOptionPatch),
    Mount(MountOptionPatch),
    Telescope(TelescopeOptionPatch),
    Site(SiteOptionPatch),
    Guiding(GuidingOptionPatch),
    PolarAlign(PolarAlignOptionPatch),
}

impl OptionPatch {
    pub fn apply(&self, options: &mut crate::options::Options) -> bool {
        match self {
            Self::AscomAlpaca(p) => p.apply(&mut options.ascom_alpaca),
            Self::Indi(p)        => p.apply(&mut options.indi),
            Self::Cam(p)         => p.apply(&mut options.cam),
            Self::Calibr(p)      => p.apply(&mut options.calibr),
            Self::RawFrames(p)   => p.apply(&mut options.raw_frames),
            Self::LiveStacking(p) => p.apply(&mut options.live),
            Self::Quality(p)     => p.apply(&mut options.quality),
            Self::Preview(p)     => p.apply(&mut options.preview),
            Self::Focuser(p)     => p.apply(&mut options.focuser),
            Self::PlateSolver(p) => p.apply(&mut options.plate_solver),
            Self::Mount(p)       => p.apply(&mut options.mount),
            Self::Telescope(p)   => p.apply(&mut options.telescope),
            Self::Site(p)        => p.apply(&mut options.site),
            Self::Guiding(p)     => p.apply(&mut options.guiding),
            Self::PolarAlign(p)  => p.apply(&mut options.polar_align),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Options;

    #[test]
    fn apply_single_field() {
        let mut options = Options::default();
        let patch = OptionPatch::Mount(MountOptionPatch { inv_ns: Some(true), ..Default::default() });
        assert!(patch.apply(&mut options));
        assert!(options.mount.inv_ns);
        assert!(!options.mount.inv_we);
    }

    #[test]
    fn unspecified_fields_untouched() {
        let mut options = Options::default();
        let orig = options.mount.clone();
        let patch = OptionPatch::Mount(MountOptionPatch { inv_we: Some(true), ..Default::default() });
        patch.apply(&mut options);
        assert_eq!(options.mount.inv_ns, orig.inv_ns);
        assert!(options.mount.inv_we);
    }

    #[test]
    fn serde_roundtrip_enum() {
        let patch = OptionPatch::Mount(MountOptionPatch { inv_ns: Some(true), ..Default::default() });
        let json = serde_json::to_string(&patch).unwrap();
        assert_eq!(json, r#"{"mount":{"inv_ns":true}}"#);
        let back: OptionPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, patch);
    }

    #[test]
    fn null_speed_clears_via_de_opt_opt() {
        let json = r#"{"polar_align":{"speed":null}}"#;
        let patch: OptionPatch = serde_json::from_str(json).unwrap();
        let OptionPatch::PolarAlign(ref p) = patch else { panic!("expected polar_align") };
        assert_eq!(p.speed, Some(None));
        let mut options = Options::default();
        options.polar_align.speed = Some("1x".to_string());
        assert!(patch.apply(&mut options));
        assert_eq!(options.polar_align.speed, None);
    }

    #[test]
    fn empty_patch_does_not_touch() {
        let json = r#"{"polar_align":{}}"#;
        let patch: OptionPatch = serde_json::from_str(json).unwrap();
        let OptionPatch::PolarAlign(p) = patch else { panic!("expected polar_align") };
        assert_eq!(p.speed, None);
    }

    #[test]
    fn apply_false_on_equal_values() {
        let mut options = Options::default();
        let patch = OptionPatch::Mount(MountOptionPatch { inv_ns: Some(options.mount.inv_ns), ..Default::default() });
        assert!(!patch.apply(&mut options));
    }

    #[test]
    fn nested_cam_frame() {
        let mut options = Options::default();
        let patch = OptionPatch::Cam(CamOptionPatch {
            frame: Some(FrameOptionPatch { gain: Some(2.5), ..Default::default() }),
            ..Default::default()
        });
        assert!(patch.apply(&mut options));
        assert_eq!(options.cam.frame.gain, 2.5);
    }

    #[test]
    fn exposure_routes_by_frame_type() {
        let mut options = Options::default();
        options.cam.frame.frame_type = FrameType::Flats;
        let patch = OptionPatch::Cam(CamOptionPatch {
            frame: Some(FrameOptionPatch { exposure: Some(0.7), ..Default::default() }),
            ..Default::default()
        });
        assert!(patch.apply(&mut options));
        assert_eq!(options.cam.frame.exp_flat, 0.7);
        assert_eq!(options.cam.frame.exp_main, FrameOptions::default().exp_main);
        assert!(!patch.apply(&mut options));
    }

    #[test]
    fn some_none_serializes_to_null() {
        let patch = OptionPatch::Mount(MountOptionPatch { speed: Some(None), ..Default::default() });
        let json = serde_json::to_string(&patch).unwrap();
        assert_eq!(json, r#"{"mount":{"speed":null}}"#);
    }

    #[test]
    fn option_option_string_clears_mount_speed() {
        let mut options = Options::default();
        options.mount.speed = Some("1x".to_string());
        let patch = OptionPatch::Mount(MountOptionPatch { speed: Some(None), ..Default::default() });
        assert!(patch.apply(&mut options));
        assert_eq!(options.mount.speed, None);
    }
}
