//! UI-independent panel states for the future web interface.
//!
//! Each `*_panel_state` function computes the complete set of widget
//! properties for one panel from server-side sources (engine, options,
//! HAL). The GTK UI applies the result mechanically. No GTK types here.

use std::ops::RangeInclusive;
use std::sync::Arc;

use serde::Serialize;

use crate::core::commands::{ModeCommand, ModeCommandReply};
use crate::core::engine::{Engine, ModeKind};
use crate::core::mode_polar_align::{PolarAlignCommand, PolarAlignState};
use crate::core::utils::{FileNameArg, FileNameUtils};
use crate::hal::{Camera, DeviceType, FrameType, Telescope};
use crate::image::raw::CalibrMethods;
use crate::options::{Binning, GuidingMode, PreviewSource};

/// Serializable spinbutton range + parameters (mirrors `correct_spinbutton_by_range`).
#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct SpinRange {
    pub start:  f64,
    pub end:    f64,
    pub digits: u32,
    pub step:   Option<f64>,
}

impl SpinRange {
    fn from_range(range: RangeInclusive<f64>, digits: u32, step: Option<f64>) -> Self {
        Self { start: *range.start(), end: *range.end(), digits, step }
    }
}

/// (id, label) pair for a combobox item.
#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct IdLabel {
    pub id:    String,
    pub label: String,
}

/// Device list for a device selection combobox
fn device_id_labels(engine: &Engine, type_: DeviceType) -> Vec<IdLabel> {
    engine.hal.devices(type_)
        .unwrap_or_default()
        .into_iter()
        .map(|d| IdLabel { id: d.id, label: d.name })
        .collect()
}

/// Slew speed list for a mount speed combobox
fn slew_speed_labels(telescope: Option<&(dyn Telescope + Send + Sync)>) -> Vec<IdLabel> {
    telescope
        .and_then(|t| t.slew_speed_list().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(id, label)| IdLabel { id, label })
        .collect()
}

/// Camera exposure range for auxiliary panel spinners (plate solve, focuser, guiding).
fn aux_exp_spin_range(camera: Option<&(dyn Camera + Send + Sync)>) -> Option<SpinRange> {
    let range = camera?.exposure_range().ok()?;
    Some(SpinRange::from_range(range, 1, Some(1.0)))
}

// ---------------------------------------------------------------------------
// Camera

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamCommonState {
    pub bx_take_shot_sensitive:  bool,
    pub l_cam_list_sensitive:    bool,
    pub cb_cam_list_sensitive:   bool,
    pub chb_live_view_sensitive: bool,
    // NOTE: cheap local HAL query; keep it here
    pub cam_list:                Vec<IdLabel>,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamCtrlState {
    pub grid_sensitive:          bool,
    pub chb_fan_visible:         bool,
    pub chb_fan_sensitive:       bool,
    pub l_heater_visible:        bool,
    pub cb_heater_visible:       bool,
    pub chb_low_noise_visible:   bool,
    pub chb_low_noise_sensitive: bool,
    pub l_conv_gain_visible:     bool,
    pub cb_conv_gain_visible:    bool,
    pub l_conv_gain_sensitive:   bool,
    pub cb_conv_gain_sensitive:  bool,
    pub chb_high_fw_visible:     bool,
    pub chb_high_fw_sensitive:   bool,
    pub chb_cooler_sensitive:    bool,
    pub spb_temp_sensitive:      bool,
    pub spb_temp_range:          Option<SpinRange>,
    // NOTE: HAL property queries, recomputed on every state build; consider a
    // separate on-demand struct (only the ctrl panel uses them)
    pub heater_list:             Vec<IdLabel>,
    pub conv_gain_list:          Vec<IdLabel>,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamFrameState {
    pub grid_sensitive:         bool,
    pub cb_mode_sensitive:      bool,
    pub chb_auto_exp_sensitive: bool,
    pub spb_exp_sensitive:      bool,
    pub cb_crop_sensitive:      bool,
    pub spb_gain_sensitive:     bool,
    pub spb_offset_sensitive:   bool,
    pub cb_bin_sensitive:       bool,
    pub spb_exp_range:          Option<SpinRange>,
    pub spb_gain_range:         Option<SpinRange>,
    pub spb_offset_range:       Option<SpinRange>,
    // NOTE: cheap camera queries; keep it here
    pub bin_list:               Vec<IdLabel>,
    pub calibr_label:           String,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamCalibrState {
    pub grid_sensitive:     bool,
    pub chb_dark_sensitive: bool,
    pub chb_flat_sensitive: bool,
    pub fch_flat_sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamRawState {
    pub grid_sensitive:            bool,
    pub btn_start_label:           String,
    pub chb_save_master_sensitive: bool,
    pub chb_frames_cnt_sensitive:  bool,
    pub spb_frames_cnt_sensitive:  bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamLiveStState {
    pub grid_sensitive:            bool,
    pub chb_save_period_sensitive: bool,
    pub spb_save_period_sensitive: bool,
    pub chb_save_orig_sensitive:   bool,
    pub chb_frames_cnt_sensitive:  bool,
    pub spb_frames_cnt_sensitive:  bool,
    pub fch_path_sensitive:        bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamQualityState {
    pub bx_sensitive:                bool,
    pub spb_max_fwhm_sensitive:      bool,
    pub spb_max_oval_sensitive:      bool,
    pub spb_max_temp_diff_sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CamActionsState {
    pub take_shot:              bool,
    pub stop_shot:              bool,
    pub start_save_raw_frames:  bool,
    pub stop_save_raw_frames:   bool,
    pub continue_save_raw:      bool,
    pub start_live_stacking:    bool,
    pub stop_live_stacking:     bool,
    pub continue_live_stacking: bool,
    pub load_image:             bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct CameraPanelState {
    pub module_panel_visible: bool,
    pub common:               CamCommonState,
    pub ctrl:                 CamCtrlState,
    pub frame:                CamFrameState,
    pub calibr:               CamCalibrState,
    pub raw:                  CamRawState,
    pub live_st:              CamLiveStState,
    pub quality:              CamQualityState,
    pub actions:              CamActionsState,
}

/// Active calibration methods label ("Dark, Flat, Hot pixels", or "---" if none)
fn compute_calibr_label(engine: &Engine, camera: &Arc<dyn Camera + Send + Sync>) -> String {
    let options = engine.options.read().unwrap();
    let mut result_str = String::new();
    if matches!(options.cam.frame.frame_type, FrameType::Lights | FrameType::Flats) {
        let to_calibrate = FileNameArg::Options(&options.cam);
        let mut fn_utils = FileNameUtils::default();
        fn_utils.init(&(Arc::clone(camera) as Arc<_>));
        let defect_pixel_file = fn_utils.defect_pixels_file_name(
            &to_calibrate,
            &options.calibr.dark_library_path
        );
        let (subtract_fname, subtract_method) = fn_utils.get_subtract_master_fname(
            &to_calibrate,
            &options.calibr.dark_library_path
        );
        if options.calibr.dark_frame_en && subtract_fname.is_file() {
            if subtract_method.contains(CalibrMethods::BY_DARK) {
                result_str += "Dark";
            } else if subtract_method.contains(CalibrMethods::BY_BIAS) {
                result_str += "Bias";
            }
        }
        if options.cam.frame.frame_type == FrameType::Lights
        && options.calibr.flat_frame_en
        && options.calibr.flat_frame_fname.is_file() {
            if !result_str.is_empty() { result_str += ", "; }
            result_str += "Flat";
        }
        if options.calibr.dark_frame_en && defect_pixel_file.is_file() {
            if !result_str.is_empty() { result_str += ", "; }
            result_str += "Hot pixels";
        }
    }
    if result_str.is_empty() {
        result_str += "---";
    }
    result_str
}

pub fn camera_panel_state(engine: &Engine) -> CameraPanelState {
    let Some(camera) = engine.cur_devices.camera() else {
        return CameraPanelState::default();
    };

    // Exposure
    let exp_range = camera.exposure_range().ok();
    let exposure_supported = exp_range.is_some();
    let spb_exp_range = exp_range.map(|r| SpinRange::from_range(r, 4, Some(1.0)));

    // Temperature
    let temp_supported = camera.is_cooler_supported().unwrap_or(false);
    let spb_temp_range = if temp_supported {
        camera.temperature_range().ok().map(|r| SpinRange::from_range(r, 0, Some(1.0)))
    } else {
        None
    };

    // Gain
    let gain_supported = camera.is_gain_supported().unwrap_or(false);
    let spb_gain_range = if gain_supported {
        camera.gain_range().ok().map(|r| SpinRange::from_range(r, 0, None))
    } else {
        None
    };

    // Offset
    let offset_supported = camera.is_offset_supported().unwrap_or(false);
    let spb_offset_range = if offset_supported {
        camera.offset_range().ok().map(|r| SpinRange::from_range(r, 0, None))
    } else {
        None
    };

    let crop_supported = camera.is_frame_supported().unwrap_or(false);
    // TODO: use max bin
    let binning_supported = camera.is_binning_supported().unwrap_or(false);
    let fan_supported = camera.is_fan_ctrl_supported().unwrap_or(false);
    let heater_supported = camera.is_heater_supported().unwrap_or(false);
    let low_noise_supported = camera.is_low_noise_supported().unwrap_or(false);
    let conv_gain_supported = camera.is_conversion_gain_supported().unwrap_or(false);
    let high_fullwell_supported = camera.is_high_fullwell_supported().unwrap_or(false);

    let (cooler_active, frame_mode, quality_use_max_fwhm, quality_use_max_ovality, quality_check_ccd_temp) = {
        let options = engine.options.read().unwrap();
        (
            options.cam.ctrl.enable_cooler,
            options.cam.frame.frame_type,
            options.quality.use_max_fwhm,
            options.quality.use_max_ovality,
            options.quality.check_ccd_temp,
        )
    };

    let cam_list = device_id_labels(engine, DeviceType::CAMERA);

    // Bin list
    let bin_list = {
        let mut list = Vec::new();
        if let Ok((max_width, max_height)) = camera.ccd_size()
            && let Ok((max_hor_bin, max_vert_bin)) = camera.max_binning() {
            let max_bin = usize::min(max_hor_bin, max_vert_bin);
            for bin in [Binning::Orig, Binning::Bin2, Binning::Bin4] {
                let ratio = bin.get_ratio();
                if ratio > max_bin { break; }
                let label = if ratio == 1 {
                    format!("{} x {}", max_width, max_height)
                } else {
                    format!("{} x {} (bin{})", max_width / ratio, max_height / ratio, ratio)
                };
                list.push(IdLabel { id: bin.to_active_id().unwrap_or_default().to_string(), label });
            }
        }
        list
    };

    // Heater + conversion gain lists
    let heater_list = if camera.is_heater_supported().unwrap_or(false) {
        camera.heater_ctrl_list().unwrap_or_default()
            .into_iter()
            .map(|(id, label)| IdLabel { id, label })
            .collect()
    } else {
        Vec::new()
    };
    let conv_gain_list = if camera.is_conversion_gain_supported().unwrap_or(false) {
        camera.conversion_gain_list().unwrap_or_default()
            .into_iter()
            .map(|(id, label)| IdLabel { id, label })
            .collect()
    } else {
        Vec::new()
    };

    let modes = engine.modes();
    let mode_kind = modes.active.kind();
    let waiting = mode_kind == ModeKind::Waiting;
    let single_shot = mode_kind == ModeKind::SingleShot;
    let liveview_active = mode_kind == ModeKind::LiveView;
    let saving_frames = mode_kind == ModeKind::SavingRawFrames;
    let saving_frames_paused = modes.aborted
        .as_ref()
        .map(|mode| mode.kind() == ModeKind::SavingRawFrames)
        .unwrap_or(false);
    let live_active = mode_kind == ModeKind::LiveStacking;
    let livestacking_paused = modes.aborted
        .as_ref()
        .map(|mode| mode.kind() == ModeKind::LiveStacking)
        .unwrap_or(false);
    drop(modes);

    let frame_mode_is_lights = frame_mode == FrameType::Lights;
    let frame_mode_is_flat = frame_mode == FrameType::Flats;
    let frame_mode_is_dark = frame_mode == FrameType::Darks;

    let save_raw_btn_cap = match frame_mode {
        FrameType::Lights => "Start save\nLIGHTS",
        FrameType::Darks  => "Start save\nDARKS",
        FrameType::Biases => "Start save\nBIASES",
        FrameType::Flats  => "Start save\nFLATS",
    };

    let calibr_label = compute_calibr_label(engine, &camera);

    let cam_active = camera.is_active().unwrap_or(false);

    let can_change_cam_opts = !saving_frames && !live_active;
    let can_change_mode = waiting || single_shot || liveview_active;
    let can_change_frame_opts = waiting || liveview_active;
    let can_change_live_stacking_opts = waiting || liveview_active;
    let can_change_cal_ops = !liveview_active;

    CameraPanelState {
        module_panel_visible: cam_active,
        common:               CamCommonState {
            bx_take_shot_sensitive:  true,
            l_cam_list_sensitive:    waiting,
            cb_cam_list_sensitive:   waiting,
            chb_live_view_sensitive: (exposure_supported && liveview_active) || can_change_mode,
            cam_list,
        },
        ctrl: CamCtrlState {
            grid_sensitive:          cam_active,
            chb_fan_visible:         fan_supported,
            chb_fan_sensitive:       !cooler_active,
            l_heater_visible:        heater_supported,
            cb_heater_visible:       heater_supported,
            chb_low_noise_visible:   low_noise_supported,
            chb_low_noise_sensitive: cam_active && can_change_cam_opts,
            l_conv_gain_visible:     conv_gain_supported,
            cb_conv_gain_visible:    conv_gain_supported,
            l_conv_gain_sensitive:   cam_active && can_change_cam_opts,
            cb_conv_gain_sensitive:  cam_active && can_change_cam_opts,
            chb_high_fw_visible:     high_fullwell_supported,
            chb_high_fw_sensitive:   cam_active && can_change_cam_opts,
            chb_cooler_sensitive:    temp_supported && can_change_cam_opts,
            spb_temp_sensitive:      cooler_active && temp_supported && can_change_cam_opts,
            spb_temp_range,
            heater_list,
            conv_gain_list,
        },
        frame: CamFrameState {
            grid_sensitive:         cam_active,
            cb_mode_sensitive:      waiting,
            chb_auto_exp_sensitive: exposure_supported && can_change_frame_opts && frame_mode_is_flat,
            spb_exp_sensitive:      exposure_supported && can_change_frame_opts,
            cb_crop_sensitive:      crop_supported && can_change_frame_opts,
            spb_gain_sensitive:     gain_supported && can_change_frame_opts,
            spb_offset_sensitive:   offset_supported && can_change_frame_opts,
            cb_bin_sensitive:       binning_supported && can_change_frame_opts,
            spb_exp_range,
            spb_gain_range,
            spb_offset_range,
            bin_list,
            calibr_label,
        },
        calibr: CamCalibrState {
            grid_sensitive:     cam_active,
            chb_dark_sensitive: can_change_cal_ops,
            chb_flat_sensitive: can_change_cal_ops,
            fch_flat_sensitive: can_change_cal_ops,
        },
        raw: CamRawState {
            grid_sensitive:            cam_active,
            btn_start_label:           save_raw_btn_cap.to_string(),
            chb_save_master_sensitive: can_change_cal_ops && (frame_mode_is_flat || frame_mode_is_dark) && !saving_frames,
            chb_frames_cnt_sensitive:  !saving_frames && can_change_mode,
            spb_frames_cnt_sensitive:  !saving_frames && can_change_mode,
        },
        live_st: CamLiveStState {
            grid_sensitive:            cam_active,
            chb_save_period_sensitive: can_change_live_stacking_opts,
            spb_save_period_sensitive: can_change_live_stacking_opts,
            chb_save_orig_sensitive:   can_change_live_stacking_opts,
            chb_frames_cnt_sensitive:  can_change_live_stacking_opts,
            spb_frames_cnt_sensitive:  can_change_live_stacking_opts,
            fch_path_sensitive:        can_change_live_stacking_opts,
        },
        quality: CamQualityState {
            bx_sensitive:                cam_active,
            spb_max_fwhm_sensitive:      quality_use_max_fwhm,
            spb_max_oval_sensitive:      quality_use_max_ovality,
            spb_max_temp_diff_sensitive: quality_check_ccd_temp,
        },
        actions: CamActionsState {
            take_shot:              exposure_supported && !single_shot && waiting,
            stop_shot:              single_shot,
            start_save_raw_frames:  exposure_supported && !saving_frames && can_change_mode,
            stop_save_raw_frames:   saving_frames,
            continue_save_raw:      saving_frames_paused && can_change_mode,
            start_live_stacking:    exposure_supported && !live_active && can_change_mode && frame_mode_is_lights,
            stop_live_stacking:     live_active,
            continue_live_stacking: livestacking_paused && can_change_mode,
            load_image:             waiting,
        },
    }
}

// ---------------------------------------------------------------------------
// Main

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct MainActionsState {
    pub stop:      bool,
    pub continue_: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct MainPanelState {
    pub actions: MainActionsState,
}

pub fn main_panel_state(engine: &Engine) -> MainPanelState {
    let modes = engine.modes();
    let can_be_continued = modes.aborted
        .as_ref()
        .map(|m| m.can_be_continued_after_stop())
        .unwrap_or(false);
    MainPanelState {
        actions: MainActionsState {
            stop:      modes.active.can_be_stopped(),
            continue_: can_be_continued,
        },
    }
}

// ---------------------------------------------------------------------------
// Mount

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct MountPanelState {
    pub module_panel_visible:   bool,
    pub bx_widgets_sensitive:   bool,
    pub chb_tracking_sensitive: bool,
    pub cb_speed_sensitive:     bool,
    pub chb_inv_ns_sensitive:   bool,
    pub chb_inv_we_sensitive:   bool,
    pub nav_buttons_sensitive:  bool,
    // NOTE: cheap local HAL query; keep it here
    pub mount_list:             Vec<IdLabel>,
    // NOTE: HAL property query, recomputed on every state build; consider a
    // separate on-demand struct (only the ctrl panel uses it)
    pub slew_speed_list:        Vec<IdLabel>,
}

pub fn mount_panel_state(engine: &Engine) -> MountPanelState {
    let telescope = engine.cur_devices.telescope();

    let mount_list = device_id_labels(engine, DeviceType::TELESCOPE);
    let slew_speed_list = slew_speed_labels(telescope.as_deref());
    let mnt_active = telescope.as_ref()
        .and_then(|t| t.is_active().ok())
        .unwrap_or(false);
    let parked = telescope.as_ref()
        .and_then(|t| t.is_parked().ok())
        .unwrap_or(false);

    let mode_kind = {
        let modes = engine.modes();
        modes.active.kind()
    };
    let waiting = mode_kind == ModeKind::Waiting;
    let live_view = mode_kind == ModeKind::LiveView;
    let single_shot = mode_kind == ModeKind::SingleShot;

    let mount_ctrl_sensitive = mnt_active && (waiting || single_shot || live_view);
    let move_enabled = !parked && mount_ctrl_sensitive;

    MountPanelState {
        module_panel_visible:   mnt_active,
        bx_widgets_sensitive:   mount_ctrl_sensitive,
        chb_tracking_sensitive: move_enabled,
        cb_speed_sensitive:     move_enabled,
        chb_inv_ns_sensitive:   move_enabled,
        chb_inv_we_sensitive:   move_enabled,
        nav_buttons_sensitive:  move_enabled,
        mount_list,
        slew_speed_list,
    }
}

// ---------------------------------------------------------------------------
// Filter wheel

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct FltWheelPanelState {
    pub cb_filter_sensitive: bool,
    // NOTE: cheap local HAL query; keep it here
    pub flt_wheel_list:     Vec<IdLabel>,
    // NOTE: HAL property query, recomputed on every state build; consider a
    // separate on-demand struct (only the filter panel uses it).
    // Empty list + `None` active means "no answer from the wheel"
    pub filters:            Vec<String>,
    pub active_filter:      Option<usize>,
}

pub fn flt_wheel_panel_state(engine: &Engine) -> FltWheelPanelState {
    let filter_wheel = engine.cur_devices.filter_wheel();
    let cb_filter_sensitive = filter_wheel.as_ref()
        .and_then(|fw| fw.is_active().ok())
        .unwrap_or(false);

    let flt_wheel_list = device_id_labels(engine, DeviceType::FLT_WHEEL);

    let (filters, active_filter) = filter_wheel.as_ref()
        .and_then(|fw| fw.list_and_active().ok())
        .map(|(list, active)| (list, Some(active)))
        .unwrap_or_default();

    FltWheelPanelState {
        cb_filter_sensitive,
        flt_wheel_list,
        filters,
        active_filter,
    }
}

// ---------------------------------------------------------------------------
// Plate solving

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct PlateSolvePanelState {
    pub grd_sensitive: bool,
    pub spb_exp_range: Option<SpinRange>,
}

pub fn plate_solve_panel_state(engine: &Engine) -> PlateSolvePanelState {
    let camera = engine.cur_devices.camera();
    let mount = engine.cur_devices.telescope();

    let cam_active = camera.as_ref()
        .and_then(|cam| cam.is_active().ok())
        .unwrap_or(false);
    let mnt_active = mount.and_then(|m| m.is_active().ok())
        .unwrap_or(false);

    let mode_kind = {
        let modes = engine.modes();
        modes.active.kind()
    };
    let waiting = mode_kind == ModeKind::Waiting;
    let live_view = mode_kind == ModeKind::LiveView;
    let single_shot = mode_kind == ModeKind::SingleShot;

    let grd_sensitive = mnt_active && cam_active && (waiting || single_shot || live_view);

    let spb_exp_range = aux_exp_spin_range(camera.as_deref());

    PlateSolvePanelState { grd_sensitive, spb_exp_range }
}

// ---------------------------------------------------------------------------
// Polar alignment

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct PolarAlignActionsState {
    pub start_polar_alignment:   bool,
    pub restart_polar_alignment: bool,
    pub stop_polar_alignment:    bool,
    pub pa_manual_refresh:       bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct PolarAlignPanelState {
    pub chb_auto_refresh_sensitive: bool,
    pub actions:                    PolarAlignActionsState,
    // NOTE: HAL property query, recomputed on every state build; consider a
    // separate on-demand struct (only the speed combobox uses it)
    pub slew_speed_list:            Vec<IdLabel>,
}

pub fn polar_align_panel_state(engine: &Arc<Engine>) -> PolarAlignPanelState {
    let camera = engine.cur_devices.camera();
    let mount = engine.cur_devices.telescope();
    let cam_active = camera.as_ref().and_then(|c| c.is_active().ok()).unwrap_or(false);
    let mnt_active = mount.as_ref().and_then(|c| c.is_active().ok()).unwrap_or(false);

    let mode_kind = {
        let modes = engine.modes();
        modes.active.kind()
    };
    let waiting = mode_kind == ModeKind::Waiting;
    let live_view = mode_kind == ModeKind::LiveView;
    let single_shot = mode_kind == ModeKind::SingleShot;
    let polar_align = mode_kind == ModeKind::PolarAlignment;

    let polar_alignment_can_be_started =
        !polar_align && mnt_active && cam_active && (waiting || single_shot || live_view);

    let auto_refresh = engine.options.read().unwrap().polar_align.auto_refresh;

    let slew_speed_list = slew_speed_labels(mount.as_deref());

    // `exec_mode_command` takes `modes.write()` internally, so the modes guard
    // must be dropped before the call (see the `mode_kind` block above).
    let mut allow_refresh = false;
    if polar_align
        && let Ok(ModeCommandReply::PolarAlignState(state)) =
            engine.exec_mode_command(ModeCommand::PolarAlign(PolarAlignCommand::GetState))
    {
        allow_refresh = matches!(state, PolarAlignState::WaitForManualRefresh);
    }

    PolarAlignPanelState {
        chb_auto_refresh_sensitive: !polar_align,
        actions:                    PolarAlignActionsState {
            start_polar_alignment:   polar_alignment_can_be_started,
            restart_polar_alignment: polar_align,
            stop_polar_alignment:    polar_align,
            pa_manual_refresh:       allow_refresh && !auto_refresh,
        },
        slew_speed_list,
    }
}

// ---------------------------------------------------------------------------
// Focuser

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct FocuserActionsState {
    pub manual_focus:      bool,
    pub stop_manual_focus: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct FocuserPanelState {
    pub module_panel_visible:   bool,
    pub grd_sensitive:          bool,
    pub spb_temp_sensitive:     bool,
    pub cb_fwhm_sensitive:      bool,
    pub cb_period_sensitive:    bool,
    pub spb_val_sensitive:      bool,
    pub cb_list_sensitive:      bool,
    pub bx_ctrl_btns_sensitive: bool,
    pub spb_exp_range:          Option<SpinRange>,
    pub actions:                FocuserActionsState,
    // NOTE: cheap local HAL query; keep it here
    pub focuser_list:           Vec<IdLabel>,
}

pub fn focuser_panel_state(engine: &Engine) -> FocuserPanelState {
    let mode_kind = {
        let modes = engine.modes();
        modes.active.kind()
    };
    let waiting = mode_kind == ModeKind::Waiting;
    let live_view = mode_kind == ModeKind::LiveView;
    let single_shot = mode_kind == ModeKind::SingleShot;
    let focusing = mode_kind == ModeKind::Focusing;
    let can_change_mode = waiting || live_view || single_shot;

    let device_enabled = engine.cur_devices.focuser()
        .and_then(|f| f.is_active().ok())
        .unwrap_or(false);

    let (on_temp_change, on_fwhm_change, periodically) = {
        let options = engine.options.read().unwrap();
        (
            options.focuser.on_temp_change,
            options.focuser.on_fwhm_change,
            options.focuser.periodically,
        )
    };

    let spb_exp_range = aux_exp_spin_range(engine.cur_devices.camera().as_deref());

    let focuser_list = device_id_labels(engine, DeviceType::FOCUSER);

    FocuserPanelState {
        module_panel_visible:   device_enabled,
        grd_sensitive:          device_enabled,
        spb_temp_sensitive:     on_temp_change,
        cb_fwhm_sensitive:      on_fwhm_change,
        cb_period_sensitive:    periodically,
        spb_val_sensitive:      !focusing,
        cb_list_sensitive:      !focusing,
        bx_ctrl_btns_sensitive: !focusing,
        spb_exp_range,
        focuser_list,
        actions: FocuserActionsState {
            manual_focus:      !focusing && can_change_mode,
            stop_manual_focus: focusing,
        },
    }
}

// ---------------------------------------------------------------------------
// Guiding

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct GuidingActionsState {
    pub start_dither_calibr: bool,
    pub stop_dither_calibr:  bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct GuidingPanelState {
    pub grd_sensitive:                 bool,
    pub rbtn_no_guiding_sensitive:     bool,
    pub rbtn_guide_main_cam_sensitive: bool,
    pub rbtn_guide_ext_sensitive:      bool,
    pub cb_dith_period_sensitive:      bool,
    pub spb_dith_dist_sensitive:       bool,
    pub spb_guid_max_err_sensitive:    bool,
    pub spb_mnt_cal_exp_sensitive:     bool,
    pub spb_ext_dith_dist_sensitive:   bool,
    pub module_panel_visible:          bool,
    pub spb_mnt_cal_exp_range:         Option<SpinRange>,
    pub actions:                       GuidingActionsState,
}

pub fn guiding_panel_state(engine: &Engine) -> GuidingPanelState {
    let mode_kind = {
        let modes = engine.modes();
        modes.active.kind()
    };
    let can_change_mode =
        mode_kind == ModeKind::Waiting ||
        mode_kind == ModeKind::SingleShot ||
        mode_kind == ModeKind::LiveView;
    let dither_calibr = mode_kind == ModeKind::DitherCalibr;

    let guiding_mode = engine.options.read().unwrap().guiding.mode.clone();
    let disabled = guiding_mode == GuidingMode::Disabled;
    let by_main_cam = guiding_mode == GuidingMode::MainCamera;
    let by_ext = guiding_mode == GuidingMode::External;

    let spb_mnt_cal_exp_range = aux_exp_spin_range(engine.cur_devices.camera().as_deref());

    GuidingPanelState {
        grd_sensitive:                 true,
        rbtn_no_guiding_sensitive:     can_change_mode,
        rbtn_guide_main_cam_sensitive: can_change_mode,
        rbtn_guide_ext_sensitive:      can_change_mode,
        cb_dith_period_sensitive:      !disabled && can_change_mode,
        spb_dith_dist_sensitive:       by_main_cam && can_change_mode,
        spb_guid_max_err_sensitive:    by_main_cam && can_change_mode,
        spb_mnt_cal_exp_sensitive:     by_main_cam && can_change_mode,
        spb_ext_dith_dist_sensitive:   by_ext && can_change_mode,
        module_panel_visible:          by_ext,
        spb_mnt_cal_exp_range,
        actions: GuidingActionsState {
            start_dither_calibr: !dither_calibr && by_main_cam && can_change_mode,
            stop_dither_calibr:  dither_calibr,
        },
    }
}

// ---------------------------------------------------------------------------
// Preview

#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct PreviewPanelState {
    pub cb_color_sensitive:      bool,
    pub chb_wb_auto_sensitive:   bool,
    pub l_dark_sensitive:        bool,
    pub scl_dark_sensitive:      bool,
    pub l_highlight_sensitive:   bool,
    pub scl_highlight_sensitive: bool,
    pub l_wb_red_sensitive:      bool,
    pub scl_wb_red_sensitive:    bool,
    pub l_wb_green_sensitive:    bool,
    pub scl_wb_green_sensitive:  bool,
    pub l_wb_blue_sensitive:     bool,
    pub scl_wb_blue_sensitive:   bool,
}

pub fn preview_panel_state(engine: &Engine) -> PreviewPanelState {
    let (source, wb_auto) = {
        let options = engine.options.read().unwrap();
        (options.preview.source, options.preview.wb_auto)
    };

    let (is_color, levels_locked) = {
        let image = match source {
            PreviewSource::OrigFrame     => engine.preview.image.read().unwrap(),
            PreviewSource::LiveStacking  => engine.live_stacking.image.read().unwrap(),
        };
        (image.is_color(), image.levels_locked())
    };

    // Display-referred images have fixed display levels and WB
    let levels_unlocked = !levels_locked;
    let rgb_enabled = !wb_auto && is_color && levels_unlocked;

    PreviewPanelState {
        cb_color_sensitive:      is_color,
        chb_wb_auto_sensitive:   is_color && levels_unlocked,
        l_dark_sensitive:        levels_unlocked,
        scl_dark_sensitive:      levels_unlocked,
        l_highlight_sensitive:   levels_unlocked,
        scl_highlight_sensitive: levels_unlocked,
        l_wb_red_sensitive:      rgb_enabled,
        scl_wb_red_sensitive:    rgb_enabled,
        l_wb_green_sensitive:    rgb_enabled,
        scl_wb_green_sensitive:  rgb_enabled,
        l_wb_blue_sensitive:     rgb_enabled,
        scl_wb_blue_sensitive:   rgb_enabled,
    }
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    // `Engine::new()` needs no hardware (HAL falls back to empty drivers).

    #[test]
    fn panel_states_without_devices_are_inert() {
        let engine = Engine::new();

        let cam = camera_panel_state(&engine);
        assert_eq!(cam, CameraPanelState::default());
        assert!(cam.common.cam_list.is_empty());
        assert!(cam.ctrl.heater_list.is_empty());
        assert!(cam.ctrl.conv_gain_list.is_empty());
        assert!(cam.frame.bin_list.is_empty());

        let flt = flt_wheel_panel_state(&engine);
        assert_eq!(flt, FltWheelPanelState::default());
        assert!(flt.flt_wheel_list.is_empty());
        assert!(flt.filters.is_empty());
        assert_eq!(flt.active_filter, None);

        let mount = mount_panel_state(&engine);
        assert_eq!(mount, MountPanelState::default());
        assert!(mount.mount_list.is_empty());
        assert!(mount.slew_speed_list.is_empty());

        assert_eq!(plate_solve_panel_state(&engine), PlateSolvePanelState::default());

        let focuser = focuser_panel_state(&engine);
        assert!(!focuser.module_panel_visible);
        assert!(focuser.spb_exp_range.is_none());
        assert!(focuser.focuser_list.is_empty());

        let guiding = guiding_panel_state(&engine);
        assert!(!guiding.module_panel_visible);
        assert!(!guiding.actions.start_dither_calibr);

        let polar = polar_align_panel_state(&engine);
        assert!(polar.slew_speed_list.is_empty());
        assert!(!polar.actions.start_polar_alignment);
        assert!(!polar.actions.restart_polar_alignment);
        assert!(!polar.actions.stop_polar_alignment);
        assert!(!polar.actions.pa_manual_refresh);

        let preview = preview_panel_state(&engine);
        assert!(!preview.cb_color_sensitive);
        assert!(!preview.scl_wb_red_sensitive);
    }

    #[test]
    fn main_panel_state_waiting_mode() {
        let engine = Engine::new();
        assert_eq!(main_panel_state(&engine), MainPanelState::default());
    }

    #[test]
    fn guiding_panel_state_follows_options_mode() {
        let engine = Engine::new();

        engine.options.write().unwrap().guiding.mode = GuidingMode::MainCamera;
        let state = guiding_panel_state(&engine);
        assert!(state.spb_dith_dist_sensitive);
        assert!(state.actions.start_dither_calibr);
        assert!(!state.module_panel_visible);

        engine.options.write().unwrap().guiding.mode = GuidingMode::External;
        let state = guiding_panel_state(&engine);
        assert!(!state.spb_dith_dist_sensitive);
        assert!(state.spb_ext_dith_dist_sensitive);
        assert!(state.module_panel_visible);
    }

    #[test]
    fn focuser_panel_state_follows_options() {
        let engine = Engine::new();
        {
            let mut options = engine.options.write().unwrap();
            options.focuser.on_temp_change = true;
            options.focuser.periodically = true;
        }
        let state = focuser_panel_state(&engine);
        assert!(state.spb_temp_sensitive);
        assert!(!state.cb_fwhm_sensitive);
        assert!(state.cb_period_sensitive);
    }

    #[test]
    fn preview_panel_state_follows_wb_auto_option() {
        let engine = Engine::new();
        engine.options.write().unwrap().preview.wb_auto = true;
        // No image loaded: not color, so WB controls stay insensitive
        let state = preview_panel_state(&engine);
        assert!(!state.scl_wb_red_sensitive);
    }
}
