use std::{
    sync::{Arc, Mutex, RwLock, atomic::AtomicBool},
    path::PathBuf,
    any::Any
};

use chrono::Utc;

use crate::{
    core::consts::INDI_SET_PROP_TIMEOUT, guiding::external_guider::*, image::{info::LightFrameInfo, raw::{FrameType, RawAdder}, stars_offset::*}, indi, options::*, utils::{io_utils::*, timer::Timer}
};
use super::{core::*, frame_processing::*, mode_mount_calibration::*};

const MAX_TIMED_GUIDE: f64 = 20.0; // in seconds

// Guider data for guiding by main camera
struct SimpleGuider {
    mnt_calibr:        Option<MountMoveCalibrRes>,
    dither_x:          f64,
    dither_y:          f64,
    cur_timed_guide_n: f64,
    cur_timed_guide_s: f64,
    cur_timed_guide_w: f64,
    cur_timed_guide_e: f64,
    dither_exp_sum:    f64,
}

impl SimpleGuider {
    fn new() -> Self {
        Self {
            mnt_calibr: None,
            dither_x: 0.0,
            dither_y: 0.0,
            cur_timed_guide_n: 0.0,
            cur_timed_guide_s: 0.0,
            cur_timed_guide_w: 0.0,
            cur_timed_guide_e: 0.0,
            dither_exp_sum:    0.0,
        }
    }
}

#[derive(PartialEq)]
pub enum CameraMode {
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
}

#[derive(PartialEq)]
enum FramesModeState {
    FrameToSkip,
    Common,
    InternalMountCorrection,
    ExternalDithering,
}

// Guider data for guiding by external program
struct ExtGuiderData {
    dither_exp_sum: f64,
    ext_guider:     Arc<Mutex<Option<Box<dyn ExternalGuider + Send>>>>,
}

pub struct TackingPicturesMode {
    cam_mode:           CameraMode,
    state:              FramesModeState,
    device:             DeviceAndProp,
    mount_device:       String,
    fn_gen:             Arc<Mutex<SeqFileNameGen>>,
    indi:               Arc<indi::Connection>,
    timer:              Option<Arc<Timer>>,
    raw_adder:          Arc<Mutex<RawAdder>>,
    options:            Arc<RwLock<Options>>,
    frame_options:      FrameOptions,
    focus_options:      Option<FocuserOptions>,
    guider_options:     Option<GuidingOptions>,
    ref_stars:          Option<Arc<Mutex<Option<Vec<Point>>>>>,
    progress:           Option<Progress>,
    cur_exposure:       f64,
    exp_sum:            f64,
    simple_guider:      Option<SimpleGuider>,
    guider:             Option<ExtGuiderData>,
    live_stacking:      Option<Arc<LiveStackingData>>,
    save_dir:           PathBuf,
    master_file:        PathBuf,
    skip_frame_done:    bool, // first frame was taken and skipped
    img_proc_stop_flag: Arc<Mutex<Arc<AtomicBool>>>,
}

impl TackingPicturesMode {
    pub fn new(
        indi:     &Arc<indi::Connection>,
        timer:    Option<&Arc<Timer>>,
        cam_mode: CameraMode,
        options:  &Arc<RwLock<Options>>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
    ) -> Self {
        let opts = options.read().unwrap();
        let progress = match cam_mode {
            CameraMode::SavingRawFrames => {
                if opts.raw_frames.use_cnt && opts.raw_frames.frame_cnt != 0 {
                    Some(Progress { cur: 0, total: opts.raw_frames.frame_cnt })
                } else {
                    None
                }
            },
            _ => None,
        };
        Self {
            cam_mode,
            state:           FramesModeState::Common,
            device:          opts.cam.device.clone(),
            mount_device:    opts.mount.device.to_string(),
            fn_gen:          Arc::new(Mutex::new(SeqFileNameGen::new())),
            indi:            Arc::clone(indi),
            timer:           timer.cloned(),
            raw_adder:       Arc::new(Mutex::new(RawAdder::new())),
            options:         Arc::clone(options),
            frame_options:   opts.cam.frame.clone(),
            focus_options:   None,
            guider_options:  None,
            ref_stars:       None,
            progress,
            cur_exposure:    0.0,
            exp_sum:         0.0,
            simple_guider:   None,
            guider:          None,
            live_stacking:   None,
            save_dir:        PathBuf::new(),
            master_file:     PathBuf::new(),
            skip_frame_done: false,
            img_proc_stop_flag: Arc::clone(img_proc_stop_flag),
        }
    }

    pub fn set_guider(
        &mut self,
        ext_guider: &Arc<Mutex<Option<Box<dyn ExternalGuider + Send>>>>
    ) {
        self.guider = Some(ExtGuiderData {
            ext_guider:     Arc::clone(ext_guider),
            dither_exp_sum: 0.0,
        });
    }

    pub fn set_ref_stars(&mut self, ref_stars: &Arc<Mutex<Option<Vec<Point>>>>) {
        self.ref_stars = Some(Arc::clone(ref_stars));
    }

    pub fn set_live_stacking(&mut self, live_stacking: &Arc<LiveStackingData>) {
        self.live_stacking = Some(Arc::clone(live_stacking));
    }

    fn update_options_copies(&mut self) {
        let opts = self.options.read().unwrap();
        let work_mode =
            self.cam_mode == CameraMode::SavingRawFrames ||
            self.cam_mode == CameraMode::LiveStacking;
        self.focus_options = if opts.focuser.is_used() && work_mode {
            Some(opts.focuser.clone())
        } else {
            None
        };
        self.guider_options = if opts.guiding.is_used() && work_mode {
            Some(opts.guiding.clone())
        } else {
            None
        };
    }

    fn correct_options_before_start(&self) {
        if self.cam_mode == CameraMode::LiveStacking {
            let mut options = self.options.write().unwrap();
            options.cam.frame.frame_type = FrameType::Lights;
        }
    }

    fn start_or_continue(&mut self) -> anyhow::Result<()> {
        // First frame must be skiped
        // for saving frames and live stacking mode
        if !self.skip_frame_done
        && matches!(self.cam_mode, CameraMode::SavingRawFrames|CameraMode::LiveStacking) {
            let mut frame_opts = self.frame_options.clone();
            const MAX_EXP: f64 = 1.0;
            if frame_opts.exposure() > MAX_EXP {
                frame_opts.set_exposure(MAX_EXP);
            }
            start_taking_shots(
                &self.indi,
                &frame_opts,
                &self.device,
                &self.img_proc_stop_flag,
                false
            )?;
            self.state = FramesModeState::FrameToSkip;
            self.cur_exposure = frame_opts.exposure();
            return Ok(());
        }

        let continuously = match (&self.cam_mode, &self.frame_options.frame_type) {
            (CameraMode::SingleShot,      _                ) => false,
            (CameraMode::LiveView,        _                ) => false,
            (CameraMode::SavingRawFrames, FrameType::Flats ) => false,
            (CameraMode::SavingRawFrames, FrameType::Biases) => false,
            (CameraMode::SavingRawFrames, _                ) => true,
            (CameraMode::LiveStacking,    _                ) => true,
        };
        start_taking_shots(
            &self.indi,
            &self.frame_options,
            &self.device,
            &self.img_proc_stop_flag,
            continuously
        )?;
        self.state = FramesModeState::Common;
        self.cur_exposure = self.frame_options.exposure();
        Ok(())
    }

    fn create_file_names_for_raw_saving(&mut self) {
        let now_date_str = Utc::now().format("%Y-%m-%d").to_string();
        let options = self.options.read().unwrap();
        let bin = options.cam.frame.binning.get_ratio();
        let cam_ccd = indi::CamCcd::from_ccd_prop_name(&self.device.prop);
        let (width, height) =
            self.indi
                .camera_get_max_frame_size(&self.device.name, cam_ccd)
                .unwrap_or((0, 0));
        let cropped_width = options.cam.frame.crop.translate(width/bin);
        let cropped_height = options.cam.frame.crop.translate(height/bin);
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
            exp_to_str(options.cam.frame.exposure()),
            options.cam.frame.gain,
            options.cam.frame.offset,
            cropped_width,
            cropped_height,
        );
        if bin != 1 {
            common_part.push_str(&format!("_bin{}x{}", bin, bin));
        }
        let type_part = match options.cam.frame.frame_type {
            FrameType::Undef => unreachable!(),
            FrameType::Lights => "light",
            FrameType::Flats => "flat",
            FrameType::Darks => "dark",
            FrameType::Biases => "bias",
        };
        let cam_cooler_supported = self.indi
            .camera_is_cooler_supported(&self.device.name)
            .unwrap_or(false);
        let temp_path = if cam_cooler_supported && options.cam.ctrl.enable_cooler {
            Some(format!("{:+.0}C", options.cam.ctrl.temperature))
        } else {
            None
        };
        if options.cam.frame.frame_type != FrameType::Lights {
            let mut master_file = String::new();
            master_file.push_str(type_part);
            master_file.push_str("_");
            master_file.push_str(&common_part);
            if options.cam.frame.frame_type != FrameType::Flats {
                if let Some(temp) = &temp_path {
                    master_file.push_str("_");
                    master_file.push_str(&temp);
                }
            }
            if options.cam.frame.frame_type == FrameType::Flats {
                master_file.push_str("_");
                master_file.push_str(&now_date_str);
            }
            master_file.push_str(".fit");

            let mut path = options.raw_frames.out_path.clone();
            path.push(&master_file);
            self.master_file = path;
        }
        let mut save_dir = String::new();
        save_dir.push_str(type_part);
        save_dir.push_str("_");
        save_dir.push_str(&now_date_str);
        save_dir.push_str("__");
        save_dir.push_str(&common_part);
        if options.cam.frame.frame_type != FrameType::Flats {
            if let Some(temp) = &temp_path {
                save_dir.push_str("_");
                save_dir.push_str(&temp);
            }
        }
        let mut path = options.raw_frames.out_path.clone();
        path.push(&save_dir);
        self.save_dir = get_free_folder_name(&path);
    }

    fn process_light_frame_info_and_refocus(
        &mut self,
        _info: &LightFrameInfo
    ) -> anyhow::Result<NotifyResult> {
        // Refocus
        let use_focus =
            self.cam_mode == CameraMode::LiveStacking ||
            self.cam_mode == CameraMode::SavingRawFrames;
        if let (Some(focuser_options), true) = (&self.focus_options, use_focus) {
            let mut have_to_refocus = false;
            if self.indi.is_device_enabled(&focuser_options.device).unwrap_or(false) {
                if focuser_options.periodically && focuser_options.period_minutes != 0 {
                    self.exp_sum += self.frame_options.exposure();
                    let max_exp_sum = (focuser_options.period_minutes * 60) as f64;
                    if self.exp_sum >= max_exp_sum {
                        have_to_refocus = true;
                        self.exp_sum = 0.0;
                    }
                }
            }
            if have_to_refocus {
                return Ok(NotifyResult::StartFocusing);
            }
        }

        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info_and_dither_by_main_camera(
        &mut          self,
        info:         &LightFrameInfo
    ) -> anyhow::Result<NotifyResult> {
        if self.state != FramesModeState::Common {
            return Ok(NotifyResult::Empty);
        }

        let mount_device_active = self.indi.is_device_enabled(&self.mount_device).unwrap_or(false);
        if !mount_device_active {
            return Ok(NotifyResult::Empty);
        }

        let guider_options = self.guider_options.as_ref().unwrap();

        let guider_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
        if guider_options.is_used() && mount_device_active {
            if guider_data.mnt_calibr.is_none() { // mount moving calibration
                return Ok(NotifyResult::StartMountCalibr);
            }
        }

        let mut move_offset = None;
        let mut prev_dither_x = 0_f64;
        let mut prev_dither_y = 0_f64;
        let mut dithering_flag = false;

        // dithering
        if guider_options.dith_period != 0 {
            guider_data.dither_exp_sum += info.exposure;
            if guider_data.dither_exp_sum > (guider_options.dith_period * 60) as f64 {
                guider_data.dither_exp_sum = 0.0;
                use rand::prelude::*;
                let mut rng = rand::thread_rng();
                prev_dither_x = guider_data.dither_x;
                prev_dither_y = guider_data.dither_y;
                guider_data.dither_x = guider_options.dith_dist as f64 * (rng.gen::<f64>() - 0.5);
                guider_data.dither_y = guider_options.dith_dist as f64 * (rng.gen::<f64>() - 0.5);
                log::debug!("dithering position = {}px,{}px", guider_data.dither_x, guider_data.dither_y);
                dithering_flag = true;
            }
        }

        // guiding
        if let (Some(offset), true) = (&info.stars_offset, guider_options.simp_guid_enabled) {
            let mut offset_x = offset.x;
            let mut offset_y = offset.y;
            offset_x -= guider_data.dither_x;
            offset_y -= guider_data.dither_y;
            let diff_dist = f64::sqrt(offset_x * offset_x + offset_y * offset_y);
            log::debug!("diff_dist = {}px", diff_dist);
            if diff_dist > guider_options.simp_guid_max_error || dithering_flag {
                move_offset = Some((-offset_x, -offset_y));
                log::debug!(
                    "diff_dist > guid_options.max_error ({} > {}), start mount correction",
                    diff_dist,
                    guider_options.simp_guid_max_error
                );
            }
        } else if dithering_flag {
            move_offset = Some((
                guider_data.dither_x - prev_dither_x,
                guider_data.dither_y - prev_dither_y
            ));
        }

        // Move mount position
        if let (Some((offset_x, offset_y)), Some(mnt_calibr)) = (move_offset, &guider_data.mnt_calibr) {
            if mnt_calibr.is_ok() {
                if let Some((mut ra, mut dec)) = mnt_calibr.calc(offset_x, offset_y) {
                    guider_data.cur_timed_guide_n = 0.0;
                    guider_data.cur_timed_guide_s = 0.0;
                    guider_data.cur_timed_guide_w = 0.0;
                    guider_data.cur_timed_guide_e = 0.0;
                    self.abort()?;
                    let can_set_guide_rate =
                        self.indi.mount_is_guide_rate_supported(&self.mount_device)? &&
                        self.indi.mount_get_guide_rate_prop_data(&self.mount_device)?.permition == indi::PropPermition::RW;
                    if can_set_guide_rate {
                        self.indi.mount_set_guide_rate(
                            &self.mount_device,
                            DITHER_CALIBR_SPEED,
                            DITHER_CALIBR_SPEED,
                            true,
                            INDI_SET_PROP_TIMEOUT
                        )?;
                    }
                    let (max_dec, max_ra) = self.indi.mount_get_timed_guide_max(&self.mount_device)?;
                    let max_dec = f64::min(MAX_TIMED_GUIDE * 1000.0, max_dec);
                    let max_ra = f64::min(MAX_TIMED_GUIDE * 1000.0, max_ra);
                    ra *= 1000.0;
                    dec *= 1000.0;
                    if ra > max_ra { ra = max_ra; }
                    if ra < -max_ra { ra = -max_ra; }
                    if dec > max_dec { dec = max_dec; }
                    if dec < -max_dec { dec = -max_dec; }
                    log::debug!("Timed guide, NS = {:.2}s, WE = {:.2}s", dec, ra);
                    self.indi.mount_timed_guide(&self.mount_device, dec, ra)?;
                    self.state = FramesModeState::InternalMountCorrection;
                    return Ok(NotifyResult::ModeChanged);
                }
            }
        }

        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info_and_dither_by_ext_guider(
        &mut self,
        info: &LightFrameInfo
    ) -> anyhow::Result<NotifyResult> {
        if self.state != FramesModeState::Common {
            return Ok(NotifyResult::Empty);
        }

        // take self.guider
        let Some(mut guider_data) = self.guider.take() else {
            return Ok(NotifyResult::Empty);
        };

        let mut fun = || -> anyhow::Result<NotifyResult> {
            let guider = guider_data.ext_guider.lock().unwrap();
            let Some(guider) = &*guider else {
                return Ok(NotifyResult::Empty);
            };

            if !guider.is_active() {
                return Ok(NotifyResult::Empty);
            }

            let guider_options = self.guider_options.as_ref().unwrap();

            if guider_options.dith_period != 0 {
                guider_data.dither_exp_sum += info.exposure;
                if guider_data.dither_exp_sum > (guider_options.dith_period * 60) as f64 {
                    guider_data.dither_exp_sum = 0.0;
                    log::info!("Starting dithering by external guider with {} pixels...", guider_options.dith_dist);
                    guider.start_dithering(guider_options.dith_dist)?;
                    self.abort()?;
                    self.state = FramesModeState::ExternalDithering;
                    return Ok(NotifyResult::ModeChanged);
                }
            }
            Ok(NotifyResult::Empty)
        };

        let res = fun();

        // return self.guider back
        self.guider = Some(guider_data);
        res
    }

    fn process_frame_processing_finished_event(
        &mut self,
        frame_is_ok: bool
    ) -> anyhow::Result<NotifyResult> {
        if self.cam_mode == CameraMode::SingleShot {
            return Ok(NotifyResult::Finished { next_mode: None });
        }
        let mut result = NotifyResult::Empty;
        if let Some(progress) = &mut self.progress {
            if frame_is_ok && progress.cur != progress.total {
                progress.cur += 1;
                result = NotifyResult::ProgressChanges;
            }
            if progress.cur == progress.total {
                abort_camera_exposure(
                    &self.indi,
                    &self.device,
                    &self.img_proc_stop_flag
                )?;
                result = NotifyResult::Finished { next_mode: None };
            } else {
                let have_shart_new_shot = match (&self.cam_mode, &self.frame_options.frame_type) {
                    (CameraMode::SavingRawFrames, FrameType::Biases) => true,
                    (CameraMode::SavingRawFrames, FrameType::Flats) => true,
                    _ => false
                };
                if have_shart_new_shot {
                    apply_camera_options_and_take_shot(
                        &self.indi,
                        &self.device,
                        &self.frame_options,
                        &self.img_proc_stop_flag,
                    )?;
                }
            }
        }
        Ok(result)
    }

    fn process_light_frame_info(
        &mut self,
        info:         &LightFrameInfo,
        _subscribers: &Arc<RwLock<Subscribers>>
    ) -> anyhow::Result<NotifyResult> {
        if !info.stars.is_ok() {
            return Ok(NotifyResult::Empty);
        }

        let res = self.process_light_frame_info_and_refocus(info)?;
        if matches!(&res, NotifyResult::Empty) == false {
            return Ok(res);
        }

        // Guiding and dithering
        if let Some(guid_options) = &self.guider_options {
            let res = match guid_options.mode {
                GuidingMode::MainCamera =>
                    self.process_light_frame_info_and_dither_by_main_camera(info)?,
                GuidingMode::Phd2 =>
                    self.process_light_frame_info_and_dither_by_ext_guider(info)?,
            };
            if matches!(&res, NotifyResult::Empty) == false { return Ok(res); }
        }

        Ok(NotifyResult::Empty)
    }

    fn process_frame_processing_started_event(&mut self) -> anyhow::Result<NotifyResult> {
        if self.state == FramesModeState::FrameToSkip {
            return Ok(NotifyResult::Empty);
        }
/*        if let Some(progress) = &mut self.progress {
            if progress.cur+1 == progress.total &&
            self.indi.camera_is_fast_toggle_enabled(&self.device.name)? {
                self.abort()?;
            }
        } */
        Ok(NotifyResult::Empty)
    }
}

impl Mode for TackingPicturesMode {
    fn get_type(&self) -> ModeType {
        match self.cam_mode {
            CameraMode::SingleShot => ModeType::SingleShot,
            CameraMode::LiveView => ModeType::LiveView,
            CameraMode::SavingRawFrames => ModeType::SavingRawFrames,
            CameraMode::LiveStacking => ModeType::LiveStacking,
        }
    }

    fn progress_string(&self) -> String {
        let mut mode_str = match (&self.state, &self.cam_mode) {
            (FramesModeState::FrameToSkip, _) =>
                "First frame (will be skipped)".to_string(),
            (FramesModeState::InternalMountCorrection, _) =>
                "Mount position correction".to_string(),
            (FramesModeState::ExternalDithering, _) =>
                "Dithering".to_string(),
            (_, CameraMode::SingleShot) =>
                "Taking shot".to_string(),
            (_, CameraMode::LiveView) =>
                "Live view from camera".to_string(),
            (_, CameraMode::SavingRawFrames) =>
                self.frame_options.frame_type.to_readable_str().to_string(),
            (_, CameraMode::LiveStacking) =>
                "Live stacking".to_string(),
        };
        let mut extra_modes = Vec::new();
        if matches!(self.cam_mode, CameraMode::SavingRawFrames|CameraMode::LiveStacking)
        && self.frame_options.frame_type == FrameType::Lights
        && self.state == FramesModeState::Common {
            if let Some(focus_options) = &self.focus_options {
                if focus_options.on_fwhm_change
                || focus_options.on_temp_change
                || focus_options.periodically {
                    extra_modes.push("F");
                }
            }
            if let Some(guid_options) = &self.guider_options {
                if guid_options.simp_guid_enabled {
                    extra_modes.push("G");
                }
                if guid_options.dith_period != 0 {
                    extra_modes.push("D");
                }
            }
        }
        if !extra_modes.is_empty() {
            mode_str += " ";
            mode_str += &extra_modes.join(" + ");
        }
        mode_str
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.device)
    }

    fn progress(&self) -> Option<Progress> {
        self.progress.clone()
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        if self.state != FramesModeState::FrameToSkip {
            Some(self.cur_exposure)
        } else {
            Some(self.cur_exposure)
        }
    }

    fn can_be_stopped(&self) -> bool {
        matches!(
            &self.cam_mode,
            CameraMode::SingleShot |
            CameraMode::SavingRawFrames|
            CameraMode::LiveStacking
        )
    }

    fn can_be_continued_after_stop(&self) -> bool {
        matches!(
            &self.cam_mode,
            CameraMode::SavingRawFrames|
            CameraMode::LiveStacking
        )
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.correct_options_before_start();
        self.update_options_copies();
        if let Some(ref_stars) = &mut self.ref_stars {
            let mut ref_stars = ref_stars.lock().unwrap();
            *ref_stars = None;
        }
        if let Some(live_stacking) = &mut self.live_stacking {
            let mut adder = live_stacking.adder.write().unwrap();
            adder.clear();
        }

        if let CameraMode::SavingRawFrames|CameraMode::LiveStacking = self.cam_mode {
            self.create_file_names_for_raw_saving();
        }

        self.start_or_continue()?;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.device, &self.img_proc_stop_flag)?;
        self.skip_frame_done = false; // will skip first frame when continue
        Ok(())
    }

    fn continue_work(&mut self) -> anyhow::Result<()> {
        self.correct_options_before_start();
        self.update_options_copies();
        self.state = FramesModeState::Common;

        // Restore original frame options
        // in saving raw or live stacking mode
        if self.cam_mode == CameraMode::SavingRawFrames
        || self.cam_mode == CameraMode::LiveStacking {
            let mut options = self.options.write().unwrap();
            options.cam.frame = self.frame_options.clone();
        }
        self.start_or_continue()?;
        Ok(())
    }

    fn set_or_correct_value(&mut self, value: &mut dyn Any) {
        if let Some(value) = value.downcast_mut::<MountMoveCalibrRes>() {
            let dith_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
            dith_data.mnt_calibr = Some(value.clone());
            log::debug!("New mount calibration set: {:?}", dith_data.mnt_calibr);
        }
    }

    fn notify_blob_start_event(
        &mut self,
        event: &indi::BlobStartEvent
    ) -> anyhow::Result<NotifyResult> {
        if *event.device_name != self.device.name
        || *event.prop_name != self.device.prop {
            return Ok(NotifyResult::Empty);
        }
        match (&self.cam_mode, &self.frame_options.frame_type) {
            (CameraMode::SingleShot,      _                ) => return Ok(NotifyResult::Empty),
            (CameraMode::SavingRawFrames, FrameType::Flats ) => return Ok(NotifyResult::Empty),
            (CameraMode::SavingRawFrames, FrameType::Biases) => return Ok(NotifyResult::Empty),
            _ => {},
        }
        if self.cam_mode == CameraMode::LiveView {
            // We need fresh frame options in live view mode
            let options = self.options.read().unwrap();
            self.frame_options = options.cam.frame.clone();
        }
        let fast_mode_enabled =
            self.indi.camera_is_fast_toggle_supported(&self.device.name).unwrap_or(false) &&
            self.indi.camera_is_fast_toggle_enabled(&self.device.name).unwrap_or(false);
        if !fast_mode_enabled {
            self.cur_exposure = self.frame_options.exposure();
            if !self.frame_options.have_to_use_delay() {
                apply_camera_options_and_take_shot(
                    &self.indi,
                    &self.device,
                    &self.frame_options,
                    &self.img_proc_stop_flag,
                )?;
            } else {
                let indi = Arc::clone(&self.indi);
                let camera = self.device.clone();
                let frame = self.frame_options.clone();
                let img_proc_stop_flag = Arc::clone(&self.img_proc_stop_flag);

                if let Some(thread_timer) = &self.timer {
                    thread_timer.exec((frame.delay * 1000.0) as u32, false, move || {
                        let res = apply_camera_options_and_take_shot(
                            &indi,
                            &camera,
                            &frame,
                            &img_proc_stop_flag,
                        );
                        if let Err(err) = res {
                            log::error!("{} during trying start next shot", err.to_string());
                            // TODO: show error!!!
                        }
                    });
                }
            }
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_before_frame_processing_start(
        &mut self,
        should_be_processed: &mut bool
    ) -> anyhow::Result<NotifyResult> {
        if self.state == FramesModeState::FrameToSkip {
            *should_be_processed = false;
            self.state = FramesModeState::Common;
            self.skip_frame_done = true;
            self.start_or_continue()?;
            return Ok(NotifyResult::ModeChanged)
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result:   &FrameProcessResult,
        subscribers: &Arc<RwLock<Subscribers>>
    ) -> anyhow::Result<NotifyResult>  {
        match &fp_result.data {
            FrameProcessResultData::ShotProcessingFinished {
                frame_is_ok, ..
            } =>
                self.process_frame_processing_finished_event(*frame_is_ok),

            FrameProcessResultData::LightFrameInfo(info) =>
                self.process_light_frame_info(info, subscribers),

            FrameProcessResultData::ShotProcessingStarted =>
                self.process_frame_processing_started_event(),

            _ =>
                Ok(NotifyResult::Empty)
        }
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        let options = self.options.read().unwrap();
        cmd.fn_gen = Some(Arc::clone(&self.fn_gen));
        let last_in_seq = if let Some(progress) = &self.progress {
            progress.cur + 1 == progress.total
        } else {
            false
        };
        match self.cam_mode {
            CameraMode::SavingRawFrames => {
                cmd.save_path = Some(self.save_dir.clone());
                if options.raw_frames.create_master {
                    cmd.raw_adder = Some(RawAdderParams {
                        adder: Arc::clone(&self.raw_adder),
                        save_fn: if last_in_seq { Some(get_free_file_name(&self.master_file)) } else { None },
                    });
                }
                if options.cam.frame.frame_type == FrameType::Lights
                && !options.mount.device.is_empty() && options.guiding.simp_guid_enabled {
                    cmd.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
                }
                cmd.flags |= ProcessImageFlags::SAVE_RAW;
            },
            CameraMode::LiveStacking => {
                cmd.save_path = Some(self.save_dir.clone());
                cmd.live_stacking = Some(LiveStackingParams {
                    data:    Arc::clone(self.live_stacking.as_ref().unwrap()),
                    options: options.live.clone(),
                });
                cmd.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
                if options.live.save_orig {
                    cmd.flags |= ProcessImageFlags::SAVE_RAW;
                }
            },
            _ => {},
        }
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if self.state == FramesModeState::InternalMountCorrection {
            if let ("TELESCOPE_TIMED_GUIDE_NS"|"TELESCOPE_TIMED_GUIDE_WE", indi::PropChange::Change { value, .. }, Some(guid_data))
            = (prop_change.prop_name.as_str(), &prop_change.change, &mut self.simple_guider) {
                match value.elem_name.as_str() {
                    "TIMED_GUIDE_N" => guid_data.cur_timed_guide_n = value.prop_value.to_f64()?,
                    "TIMED_GUIDE_S" => guid_data.cur_timed_guide_s = value.prop_value.to_f64()?,
                    "TIMED_GUIDE_W" => guid_data.cur_timed_guide_w = value.prop_value.to_f64()?,
                    "TIMED_GUIDE_E" => guid_data.cur_timed_guide_e = value.prop_value.to_f64()?,
                    _ => {},
                }
                if guid_data.cur_timed_guide_n == 0.0
                && guid_data.cur_timed_guide_s == 0.0
                && guid_data.cur_timed_guide_w == 0.0
                && guid_data.cur_timed_guide_e == 0.0 {
                    start_taking_shots(
                        &self.indi,
                        &self.frame_options,
                        &self.device,
                        &self.img_proc_stop_flag,
                        true
                    )?;
                    self.state = FramesModeState::Common;
                    result = NotifyResult::ModeChanged;
                }
            }
        }
        Ok(result)
    }

    fn notify_guider_event(
        &mut self,
        event: ExtGuiderEvent
    ) -> anyhow::Result<NotifyResult> {
        if let Some(guid_options) = &self.guider_options {
            if guid_options.mode == GuidingMode::Phd2
            && self.state == FramesModeState::ExternalDithering {
                match event {
                    ExtGuiderEvent::DitheringFinished => {
                        self.skip_frame_done = false;
                        self.start_or_continue()?;
                        return Ok(NotifyResult::ModeChanged);
                    }
                    ExtGuiderEvent::Error(error) =>
                        return Err(anyhow::anyhow!("External guider error: {}", error)),
                    _ => {}
                }
            }
        }
        Ok(NotifyResult::Empty)
    }
}
