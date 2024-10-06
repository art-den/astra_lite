use core::f64;
use std::{
    any::Any, path::PathBuf, sync::{atomic::AtomicBool, Arc, Mutex, RwLock}
};

use chrono::Utc;

use crate::{
    core::consts::INDI_SET_PROP_TIMEOUT,
    guiding::external_guider::*,
    image::{histogram::*, info::LightFrameInfo, raw::{FrameType, RawAdder, RawImage, RawImageInfo}, stars_offset::*},
    indi,
    options::*,
    utils::{io_utils::*, timer::Timer},
    TimeLogger
};
use super::{core::*, frame_processing::*, mode_darks_library::DarkCreationProgramItem, mode_mount_calibration::*, utils::FileNameUtils};

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
    SavingMasterDark,
    SavingDefectPixels,
    LiveStacking,
}

#[derive(PartialEq)]
enum State {
    FrameToSkip,
    Common,
    CameraOffsetCalculation,
    WaitingForMountCalibration,
    InternalMountCorrection,
    ExternalDithering,
}

// Guider data for guiding by external program
struct ExtGuiderData {
    dither_exp_sum: f64,
    ext_guider:     Arc<Mutex<Option<Box<dyn ExternalGuider + Send>>>>,
}

struct RefocusData {
    exp_sum:  f64,
    min_temp: Option<f64>,
    max_temp: Option<f64>,
    fwhm:     Vec<f32>,
}

#[derive(Default)]
struct Flags {
    skip_frame_done:    bool,
    save_raw_files:     bool,
    use_raw_adder:      bool,
    save_master_file:   bool,
    save_defect_pixels: bool,
}

#[derive(Default, Debug)]
struct OutFileNames {
    raw_files_dir:       PathBuf,
    master_fname:        PathBuf,
    defect_pixels_fname: PathBuf,
}

struct CamOffsetCalc {
    step: usize,
    low_values: Vec<(u16, f64)>,
    high_values: Vec<(u16, f64)>,
}

pub struct TackingPicturesMode {
    cam_mode:        CameraMode,
    state:           State,
    device:          DeviceAndProp,
    mount_device:    String,
    fn_gen:          Arc<Mutex<SeqFileNameGen>>,
    indi:            Arc<indi::Connection>,
    subscribers:     Arc<RwLock<Subscribers>>,
    timer:           Option<Arc<Timer>>,
    raw_adder:       Arc<Mutex<RawAdder>>,
    options:         Arc<RwLock<Options>>,
    cam_options:     CamOptions,
    focus_options:   Option<FocuserOptions>,
    guider_options:  Option<GuidingOptions>,
    ref_stars:       Option<Arc<Mutex<Option<Vec<Point>>>>>,
    progress:        Option<Progress>,
    cur_exposure:    f64,
    simple_guider:   Option<SimpleGuider>,
    guider:          Option<ExtGuiderData>,
    live_stacking:   Option<Arc<LiveStackingData>>,
    refocus:         RefocusData,
    flags:           Flags,
    fname_utils:     FileNameUtils,
    out_file_names:  OutFileNames,
    camera_offset:   Option<u16>,
    cam_offset_calc: Option<CamOffsetCalc>,
    next_mode:       Option<ModeBox>,
}

impl TackingPicturesMode {
    pub fn new(
        indi:        &Arc<indi::Connection>,
        subscribers: &Arc<RwLock<Subscribers>>,
        timer:       Option<&Arc<Timer>>,
        cam_mode:    CameraMode,
        options:     &Arc<RwLock<Options>>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };
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

        let refocus = RefocusData {
            exp_sum:  0.0,
            min_temp: None,
            max_temp: None,
            fwhm:     Vec::new(),
        };

        let mut cam_options = opts.cam.clone();
        if cam_mode == CameraMode::LiveStacking {
            cam_options.frame.frame_type = crate::image::raw::FrameType::Lights;
        }

        Ok(Self {
            cam_mode,
            state:           State::Common,
            device:          cam_device.clone(),
            mount_device:    opts.mount.device.to_string(),
            fn_gen:          Arc::new(Mutex::new(SeqFileNameGen::new())),
            indi:            Arc::clone(indi),
            subscribers:     Arc::clone(subscribers),
            timer:           timer.cloned(),
            raw_adder:       Arc::new(Mutex::new(RawAdder::new())),
            options:         Arc::clone(options),
            cam_options,
            focus_options:   None,
            guider_options:  None,
            ref_stars:       None,
            cur_exposure:    0.0,
            simple_guider:   None,
            guider:          None,
            live_stacking:   None,
            out_file_names:  OutFileNames::default(),
            camera_offset:   None,
            cam_offset_calc: None,
            next_mode:       None,
            flags:           Flags::default(),
            fname_utils:     FileNameUtils::default(),
            refocus,
            progress,
        })
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

    pub fn set_dark_creation_program_item(&mut self, item: &DarkCreationProgramItem) {
        self.progress = Some(Progress {cur: 0, total: item.count});
        if let Some(temperature) = item.temperature {
            self.cam_options.ctrl.temperature = temperature;
            self.cam_options.ctrl.enable_cooler = true;
        }
        self.cam_options.frame.exp_main = item.exposure;
        self.cam_options.frame.gain = item.gain;
        self.cam_options.frame.offset = item.offset;
        self.cam_options.frame.binning = item.binning;
        self.cam_options.frame.crop = item.crop;
    }

    pub fn set_next_mode(&mut self, next_mode: Option<ModeBox>) {
        self.next_mode = next_mode;
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

    fn correct_options_before_start(&mut self) {
        match self.cam_mode {
            CameraMode::LiveStacking => {
                let mut options = self.options.write().unwrap();
                options.cam.frame.frame_type = FrameType::Lights;
                self.cam_options.frame.frame_type = FrameType::Lights;
            }
            CameraMode::SavingMasterDark|
            CameraMode::SavingDefectPixels => {
                self.cam_options.frame.frame_type = FrameType::Darks;
            }
            _ => {}
        }
    }

    fn start_or_continue(&mut self) -> anyhow::Result<()> {
        // First frame must be skiped
        // for saving frames and live stacking mode
        let need_skip_first_frame =
            self.cam_mode == CameraMode::SavingRawFrames ||
            self.cam_mode == CameraMode::LiveStacking ||
            self.cam_mode == CameraMode::SavingMasterDark;
        if !self.flags.skip_frame_done && need_skip_first_frame {
            self.start_shot_that_will_be_skipped()?;
            self.state = State::FrameToSkip;
            return Ok(());
        }

        if self.cam_mode == CameraMode::SavingRawFrames
        && self.cam_options.frame.frame_type == FrameType::Flats
        && self.cam_options.frame.offset != 0
        && self.camera_offset.is_none()
        && self.flags.save_master_file {
            // we need to calculate real camera offset before creating master flat file
            self.cam_offset_calc = Some(CamOffsetCalc {
                step: 0,
                low_values: Vec::new(),
                high_values: Vec::new(),
            });
            self.start_offset_calculation_shot()?;
            self.state = State::CameraOffsetCalculation;
            return Ok(());
        }

        let continuously = self.is_continuous_mode();
        init_cam_continuous_mode(&self.indi, &self.device, &self.cam_options.frame, continuously)?;
        apply_camera_options_and_take_shot(&self.indi, &self.device, &self.cam_options.frame)?;

        self.state = State::Common;
        self.cur_exposure = self.cam_options.frame.exposure();
        Ok(())
    }

    fn start_shot_that_will_be_skipped(&mut self) -> anyhow::Result<()> {
        let mut frame_opts = self.cam_options.frame.clone();
        const MAX_EXP: f64 = 1.0;
        if frame_opts.exposure() > MAX_EXP {
            frame_opts.set_exposure(MAX_EXP);
        }
        init_cam_continuous_mode(&self.indi, &self.device, &frame_opts, false)?;
        apply_camera_options_and_take_shot(&self.indi, &self.device, &frame_opts)?;
        self.cur_exposure = frame_opts.exposure();
        Ok(())
    }

    fn start_offset_calculation_shot(&mut self) -> anyhow::Result<()> {
        if let Some(offset_calc) = &self.cam_offset_calc {
            let mut frame_opts = self.cam_options.frame.clone();
            if offset_calc.step % 2 == 0 { frame_opts.offset = 0; }
            //frame_opts.exp_flat /= offset_calc.step as f64;
            init_cam_continuous_mode(&self.indi, &self.device, &frame_opts, false)?;
            apply_camera_options_and_take_shot(&self.indi, &self.device, &frame_opts)?;
            self.cur_exposure = frame_opts.exposure();
        }
        Ok(())
    }

    fn is_continuous_mode(&self) -> bool {
        match (&self.cam_mode, &self.cam_options.frame.frame_type) {
            (CameraMode::SingleShot,         _                ) => false,
            (CameraMode::LiveView,           _                ) => false,
            (CameraMode::SavingRawFrames,    FrameType::Flats ) => false,
            (CameraMode::SavingRawFrames,    FrameType::Biases) => false,
            (CameraMode::SavingRawFrames,    _                ) => true,
            (CameraMode::LiveStacking,       _                ) => true,
            (CameraMode::SavingMasterDark,   _                ) => true,
            (CameraMode::SavingDefectPixels, _                ) => true,
        }
    }

    fn generate_output_file_names(&mut self) -> anyhow::Result<()> {
        let options = self.options.read().unwrap();

        let time = Utc::now();

        // Calibration master file for saving

        if self.flags.save_master_file {
            let mut path = PathBuf::new();
            if self.cam_mode == CameraMode::SavingMasterDark {
                path.push(&options.calibr.dark_library_path);
                path.push(&self.device.to_file_name_part());
            } else {
                path.push(&options.raw_frames.out_path);
            }
            let file_name = self.fname_utils.master_only_file_name(
                Some(time),
                &self.cam_options,
            );
            path.push(&file_name);
            self.out_file_names.master_fname = path;
        }

        // Defect pixels file for saving

        if self.flags.save_defect_pixels {
            self.out_file_names.defect_pixels_fname = self.fname_utils.defect_pixels_file_name(
                &self.cam_options,
                &options
            );
        }

        // Full path for raw images

        if self.flags.save_raw_files {
            let save_dir = self.fname_utils.raw_file_dest_dir(time, &self.cam_options);
            let mut path = PathBuf::new();
            path.push(&options.raw_frames.out_path);
            path.push(&save_dir);
            self.out_file_names.raw_files_dir = get_free_folder_name(&path);
        }

        log::debug!("output_file_names: {:?}", self.out_file_names);

        Ok(())
    }

    fn process_light_frame_info_and_refocus(
        &mut self,
        info: &LightFrameInfo
    ) -> anyhow::Result<NotifyResult> {
        let use_focus =
            self.cam_mode == CameraMode::LiveStacking ||
            self.cam_mode == CameraMode::SavingRawFrames;
        if !use_focus {
            return Ok(NotifyResult::Empty);
        }

        // push fwhm
        if let Some(fwhm) = info.stars.fwhm {
            self.refocus.fwhm.push(fwhm);
        }

        // Update exposure sum
        self.refocus.exp_sum += self.cam_options.frame.exposure();

        let Some(focuser_options) = &self.focus_options else {
            return Ok(NotifyResult::Empty);
        };

        // Update min and max temperature
        let temperature = self.indi.focuser_get_temperature(&focuser_options.device)?;
        if !temperature.is_nan() && !temperature.is_infinite() {
            self.refocus.min_temp = self.refocus.min_temp
                .map(|v| f64::min(v, temperature))
                .or_else(|| Some(temperature));
            self.refocus.max_temp = self.refocus.max_temp
                .map(|v| f64::max(v, temperature))
                .or_else(|| Some(temperature));
        }

        if !self.indi.is_device_enabled(&focuser_options.device).unwrap_or(false) {
            return Ok(NotifyResult::Empty);
        }

        let mut have_to_refocus = false;

        // Periodically
        if focuser_options.periodically
        && focuser_options.period_minutes != 0 {
            let max_exp_sum = (focuser_options.period_minutes * 60) as f64;
            if self.refocus.exp_sum >= max_exp_sum {
                have_to_refocus = true;
            }
        }

        // When temperature changed
        if focuser_options.on_temp_change
        && focuser_options.max_temp_change > 0.0 {
            if let (Some(min), Some(max)) = (self.refocus.min_temp, self.refocus.max_temp) {
                if max - min > focuser_options.max_temp_change {
                    have_to_refocus = true;
                }
            }
        }

        // On FWHM increase
        if focuser_options.on_fwhm_change
        && focuser_options.max_fwhm_change != 0
        && self.refocus.fwhm.len() >= 6 {
            let pos = self.refocus.fwhm.len() - 3;
            let before = &self.refocus.fwhm[..pos];
            let after = &self.refocus.fwhm[pos..];
            let before_best = before.iter()
                .copied()
                .min_by(f32::total_cmp)
                .unwrap_or_default() as f64;
            let after_aver = after.iter().sum::<f32>() as f64 / after.len() as f64;
            if before_best != 0.0 {
                let percent = 100.0 * (after_aver - before_best) / before_best;
                if percent > focuser_options.max_fwhm_change as f64 {
                    have_to_refocus = true;
                }
            }
        }

        if have_to_refocus {
            self.refocus.exp_sum = 0.0;
            self.refocus.min_temp = None;
            self.refocus.max_temp = None;
            self.refocus.fwhm.clear();
            return Ok(NotifyResult::StartFocusing);
        }

        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info_and_dither_by_main_camera(
        &mut self,
        info: &LightFrameInfo
    ) -> anyhow::Result<NotifyResult> {
        let mount_device_active = self.indi.is_device_enabled(&self.mount_device).unwrap_or(false);
        if !mount_device_active {
            return Ok(NotifyResult::Empty);
        }

        let guider_options = self.guider_options.as_ref().unwrap();

        let guider_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
        if guider_options.is_used() && mount_device_active {
            if guider_data.mnt_calibr.is_none() { // mount moving calibration
                self.abort()?;
                self.state = State::WaitingForMountCalibration;
                return Ok(NotifyResult::Empty);
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
                guider_data.dither_x = guider_options.main_cam.dith_dist as f64 * (rng.gen::<f64>() - 0.5);
                guider_data.dither_y = guider_options.main_cam.dith_dist as f64 * (rng.gen::<f64>() - 0.5);
                log::debug!("dithering position = {}px,{}px", guider_data.dither_x, guider_data.dither_y);
                dithering_flag = true;
            }
        }

        // guiding
        if let Some(offset) = &info.stars_offset {
            let mut offset_x = offset.x;
            let mut offset_y = offset.y;
            offset_x -= guider_data.dither_x;
            offset_y -= guider_data.dither_y;
            let diff_dist = f64::sqrt(offset_x * offset_x + offset_y * offset_y);
            log::debug!("diff_dist = {}px", diff_dist);
            if diff_dist > guider_options.main_cam.max_error || dithering_flag {
                move_offset = Some((-offset_x, -offset_y));
                log::debug!(
                    "diff_dist > guid_options.max_error ({} > {}), start mount correction",
                    diff_dist,
                    guider_options.main_cam.max_error
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
                    self.state = State::InternalMountCorrection;
                    return Ok(NotifyResult::ModeStrChanged);
                }
            }
        }

        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info_and_dither_by_ext_guider(
        &mut self,
        info: &LightFrameInfo
    ) -> anyhow::Result<NotifyResult> {
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
                    let dist = guider_options.ext_guider.dith_dist;
                    log::info!("Starting dithering by external guider with {} pixels...", dist);
                    guider.start_dithering(dist)?;
                    self.abort()?;
                    self.state = State::ExternalDithering;
                    return Ok(NotifyResult::ModeStrChanged);
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
        frame_is_ok:    bool,
        blob:           &indi::BlobPropValue,
        raw_image_info: &RawImageInfo,
        cmd_stop_flag:  &Arc<AtomicBool>,
    ) -> anyhow::Result<NotifyResult> {
        if self.cam_mode == CameraMode::SingleShot {
            return Ok(NotifyResult::Finished {
                next_mode: self.next_mode.take()
            });
        }

        if let (State::CameraOffsetCalculation, Some(offset_calc))
        = (&self.state, &mut self.cam_offset_calc) {
            if offset_calc.step == Self::MAX_OFFSET_CALC_STEPS {
                self.start_or_continue()?;
                return Ok(NotifyResult::ModeStrChanged);
            }
        }

        if self.state != State::Common
        && self.state != State::WaitingForMountCalibration
        && self.state != State::InternalMountCorrection {
            return Ok(NotifyResult::Empty);
        }

        let frame_type = raw_image_info.frame_type;
        let mut result = NotifyResult::Empty;
        let mut is_last_frame = false;
        if let Some(progress) = &mut self.progress {
            if frame_is_ok && progress.cur != progress.total {
                progress.cur += 1;
                result = NotifyResult::ProgressChanges;
            }
            if progress.cur == progress.total {
                abort_camera_exposure(&self.indi, &self.device)?;
                result = NotifyResult::Finished {
                    next_mode: self.next_mode.take()
                };
                is_last_frame = true;
            } else {
                let have_shart_new_shot = match (&self.cam_mode, frame_type) {
                    (CameraMode::SavingRawFrames, FrameType::Biases) => true,
                    (CameraMode::SavingRawFrames, FrameType::Flats) => true,
                    _ => false
                };
                if have_shart_new_shot {
                    apply_camera_options_and_take_shot(
                        &self.indi,
                        &self.device,
                        &self.cam_options.frame
                    )?;
                }
            }
        }

        // Save raw image
        if frame_is_ok && self.flags.save_raw_files {
            self.save_raw_image(blob, raw_image_info)?;
        }

        // Save master file
        if is_last_frame && self.flags.save_master_file {
            self.save_master_file()?;

            let result = FrameProcessResultData::MasterSaved {
                frame_type: raw_image_info.frame_type,
                file_name: self.out_file_names.master_fname.clone()
            };

            let event_data = FrameProcessResult {
                camera:        self.device.clone(),
                cmd_stop_flag: Arc::clone(cmd_stop_flag),
                mode_type:     self.get_type(),
                data:          result,
            };

            let subscribers = self.subscribers.read().unwrap();
            subscribers.inform_event(CoreEvent::FrameProcessing(event_data));
        }

        if is_last_frame && self.flags.save_defect_pixels {
            self.save_defect_pixels_file()?;
        }

        if self.state == State::WaitingForMountCalibration {
            self.state = State::Common;
            return Ok(NotifyResult::StartMountCalibr);
        }

        Ok(result)
    }

    const MAX_OFFSET_CALC_STEPS: usize = 8;

    fn process_raw_histogram(
        &mut self,
        hist: &Arc<RwLock<Histogram>>
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;

        if let (State::CameraOffsetCalculation, Some(offset_calc))
        = (&self.state, &mut self.cam_offset_calc) {
            let hist = hist.read().unwrap();
            let chan = if hist.g.is_some() { &hist.g } else { &hist.l };
            if let Some(chan) = chan {
                if offset_calc.step % 2 == 0 {
                    offset_calc.low_values.push((chan.median(), chan.std_dev));
                } else {
                    offset_calc.high_values.push((chan.median(), chan.std_dev));
                }
            }

            offset_calc.step += 1;
            if offset_calc.step != Self::MAX_OFFSET_CALC_STEPS {
                self.start_offset_calculation_shot()?;
            } else {
                log::debug!(
                    "Calculating camera offset from low = {:?} and high = {:?} values ...",
                    offset_calc.low_values, offset_calc.high_values
                );
                let mut min_deviation_diff = f64::MAX;
                let mut result_value = 0i32;
                for (m1, d1) in &offset_calc.low_values {
                    for (m2, d2) in &offset_calc.high_values {
                        let dev_diff = f64::abs(d1 - d2);
                        if dev_diff < min_deviation_diff {
                            min_deviation_diff = dev_diff;
                            result_value = *m2 as i32 - *m1 as i32;
                        }
                    }
                }
                log::debug!("Camera offset result = {}", result_value);
                result_value = result_value.min(u16::MAX as i32);
                result_value = result_value.max(u16::MIN as i32);
                self.camera_offset = Some(result_value as u16);
                result = NotifyResult::ModeStrChanged;
            }
        }

        Ok(result)
    }

    fn add_raw_image(&mut self, raw_image: &RawImage) -> anyhow::Result<()> {
        let mut adder = self.raw_adder.lock().unwrap();
        if raw_image.info().frame_type == FrameType::Flats {
            let mut normalized_flat = raw_image.clone();
            let tmr = TimeLogger::start();
            let flat_offset = self.camera_offset.unwrap_or_default();
            normalized_flat.set_offset(flat_offset as i32);
            normalized_flat.normalize_flat();
            tmr.log("Normalizing flat");
            let tmr = TimeLogger::start();
            adder.add(&normalized_flat, false)?;
            tmr.log("Adding raw calibration frame");
        } else {
            let tmr = TimeLogger::start();
            adder.add(raw_image, true)?;
            tmr.log("Adding raw calibration frame");
        }
        Ok(())
    }

    fn save_raw_image(
        &mut self,
        blob:           &indi::BlobPropValue,
        raw_image_info: &RawImageInfo,
    ) -> anyhow::Result<()> {
        let prefix = match raw_image_info.frame_type {
            FrameType::Lights => "light",
            FrameType::Flats => "flat",
            FrameType::Darks => "dark",
            FrameType::Biases => "bias",
            FrameType::Undef => unreachable!(),
        };
        if !self.out_file_names.raw_files_dir.is_dir() {
            std::fs::create_dir_all(&self.out_file_names.raw_files_dir)
                .map_err(|e|anyhow::anyhow!(
                    "Error '{}'\nwhen trying to create directory '{}' for saving RAW frame",
                    e.to_string(),
                    self.out_file_names.raw_files_dir.to_str().unwrap_or_default()
                ))?;
        }
        let mut file_ext = blob.format.as_str().trim();
        while file_ext.starts_with('.') { file_ext = &file_ext[1..]; }
        let fn_mask = format!("{}_${{num}}.{}", prefix, file_ext);
        let mut fn_gen = self.fn_gen.lock().unwrap();
        let file_name = fn_gen.generate(&self.out_file_names.raw_files_dir, &fn_mask);
        drop(fn_gen);

        let tmr = TimeLogger::start();
        std::fs::write(&file_name, blob.data.as_slice())
            .map_err(|e| anyhow::anyhow!(
                "Error '{}'\nwhen saving file '{}'",
                e.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;
        tmr.log("Saving raw image");

        Ok(())
    }

    fn save_master_file(&mut self) -> anyhow::Result<()> {
        log::debug!("Saving master frame...");
        let mut adder = self.raw_adder.lock().unwrap();

        let raw_image = adder.get()?;
        adder.clear();
        drop(adder);

        if let Some(parent) = self.out_file_names.master_fname.parent() {
            if !parent.is_dir() {
                log::debug!("Creating directory {} ...", parent.to_str().unwrap_or_default());
                std::fs::create_dir_all(&parent)?;
            }
        }

        raw_image.save_to_fits_file(&self.out_file_names.master_fname)?;

        log::debug!("Master frame saved!");
        Ok(())
    }

    fn save_defect_pixels_file(&mut self) -> anyhow::Result<()> {
        log::debug!("Saving defect pixels file...");
        let mut adder = self.raw_adder.lock().unwrap();

        let raw_image = adder.get()?;
        adder.clear();
        drop(adder);

        if let Some(parent) = self.out_file_names.defect_pixels_fname.parent() {
            if !parent.is_dir() {
                log::debug!("Creating directory {} ...", parent.to_str().unwrap_or_default());
                std::fs::create_dir_all(&parent)?;
            }
        }

        let defect_pixels = raw_image.find_hot_pixels_in_master_dark();
        log::debug!("Defect pixels count = {}", defect_pixels.items.len());

        defect_pixels.save_to_file(&self.out_file_names.defect_pixels_fname)?;
        log::debug!("Defect pixels file saved!");

        Ok(())
    }

    fn is_frame_type_for_raw_adder(frame_type: FrameType) -> bool {
        matches!(
            frame_type,
            FrameType::Flats| FrameType::Darks | FrameType::Biases
        )
    }

    fn process_raw_image(
        &mut self,
        raw_image: &RawImage,
    ) -> anyhow::Result<NotifyResult> {
        if self.state != State::Common {
            return Ok(NotifyResult::Empty);
        }

        let frame_for_raw_adder =
            Self::is_frame_type_for_raw_adder(raw_image.info().frame_type);

        if frame_for_raw_adder && self.flags.use_raw_adder {
            self.add_raw_image(raw_image)?;
        }
        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info(
        &mut self,
        info: &LightFrameInfo,
    ) -> anyhow::Result<NotifyResult> {
        if !info.stars.is_ok() {
            return Ok(NotifyResult::Empty);
        }

        if self.state != State::Common {
            return Ok(NotifyResult::Empty);
        }

        let res = self.process_light_frame_info_and_refocus(info)?;
        if matches!(&res, NotifyResult::Empty) == false {
            return Ok(res);
        }

        // Guiding and dithering
        if let Some(guid_options) = &self.guider_options {
            let res = match guid_options.mode {
                GuidingMode::Disabled =>
                    NotifyResult::Empty,
                GuidingMode::MainCamera =>
                    self.process_light_frame_info_and_dither_by_main_camera(info)?,
                GuidingMode::External =>
                    self.process_light_frame_info_and_dither_by_ext_guider(info)?,
            };
            if matches!(&res, NotifyResult::Empty) == false { return Ok(res); }
        }

        Ok(NotifyResult::Empty)
    }

    fn get_dark_creation_short_info(&self) -> String {
        let mut result = String::new();
        if self.cam_options.ctrl.enable_cooler {
            result += &format!("{:.1}°С ", self.cam_options.ctrl.temperature);
        }
        result += &format!(
            "{:.1}s g:{:.0} offs:{}",
            self.cam_options.frame.exposure(),
            self.cam_options.frame.gain,
            self.cam_options.frame.offset,
        );
        if self.cam_options.frame.binning != Binning::Orig {
            result += &format!(" bin:{}", self.cam_options.frame.binning.to_str());
        }
        if self.cam_options.frame.crop != Crop::None {
            result += &format!(" crop:{}", self.cam_options.frame.crop.to_str());
        }
        result
    }

    fn get_defect_pixels_creation_short_info(&self) -> String {
        format!(
            "bin:{} crop:{}",
            self.cam_options.frame.binning.to_str(),
            self.cam_options.frame.crop.to_str(),
        )
    }

}

impl Mode for TackingPicturesMode {
    fn get_type(&self) -> ModeType {
        match self.cam_mode {
            CameraMode::SingleShot         => ModeType::SingleShot,
            CameraMode::LiveView           => ModeType::LiveView,
            CameraMode::SavingRawFrames    => ModeType::SavingRawFrames,
            CameraMode::LiveStacking       => ModeType::LiveStacking,
            CameraMode::SavingMasterDark   => ModeType::SavingMasterDark,
            CameraMode::SavingDefectPixels => ModeType::SavingDefectPixels,
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.device)
    }

    fn cam_opts(&self) -> Option<&CamOptions> {
        Some(&self.cam_options)
    }

    fn progress_string(&self) -> String {
        let mut mode_str = match (&self.state, &self.cam_mode) {
            (State::FrameToSkip, _) =>
                "First frame (will be skipped)".to_string(),
            (State::InternalMountCorrection, _) =>
                "Mount position correction".to_string(),
            (State::ExternalDithering, _) =>
                "Dithering".to_string(),
            (State::CameraOffsetCalculation, _) =>
                "Camera calibration...".to_string(),
            (_, CameraMode::SingleShot) =>
                "Taking shot".to_string(),
            (_, CameraMode::LiveView) =>
                "Live view from camera".to_string(),
            (_, CameraMode::SavingRawFrames) =>
                self.cam_options.frame.frame_type.to_readable_str().to_string(),
            (_, CameraMode::SavingMasterDark) =>
                format!("Creating master dark ({})", self.get_dark_creation_short_info()),
            (_, CameraMode::SavingDefectPixels) =>
                format!("Creating defective pixels files ({})", self.get_defect_pixels_creation_short_info()),
            (_, CameraMode::LiveStacking) =>
                "Live stacking".to_string(),
        };
        let mut extra_modes = Vec::new();
        if matches!(self.cam_mode, CameraMode::SavingRawFrames|CameraMode::LiveStacking)
        && self.cam_options.frame.frame_type == FrameType::Lights
        && self.state == State::Common {
            if let Some(focus_options) = &self.focus_options {
                if focus_options.on_fwhm_change
                || focus_options.on_temp_change
                || focus_options.periodically {
                    extra_modes.push("F");
                }
            }
            if let Some(guid_options) = &self.guider_options {
                extra_modes.push("G");
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

    fn progress(&self) -> Option<Progress> {
        self.progress.clone()
    }

    fn take_next_mode(&mut self) -> Option<ModeBox> {
        self.next_mode.take()
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        if self.state != State::FrameToSkip {
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
            CameraMode::SavingMasterDark|
            CameraMode::SavingDefectPixels|
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

        let options = self.options.read().unwrap();
        self.flags.save_raw_files = match self.cam_mode {
            CameraMode::SavingRawFrames => true,
            CameraMode::LiveStacking => options.live.save_orig,
            _ => false,
        };
        self.flags.save_master_file = match self.cam_mode {
            CameraMode::SavingRawFrames =>
                self.cam_options.frame.frame_type != FrameType::Lights &&
                options.raw_frames.create_master,
            CameraMode::SavingMasterDark => true,
            _ => false,
        };
        self.flags.save_defect_pixels = match self.cam_mode {
            CameraMode::SavingDefectPixels => true,
            _ => false,
        };
        self.flags.use_raw_adder =
            self.flags.save_master_file ||
            self.flags.save_defect_pixels;

        drop(options);

        if let Some(ref_stars) = &mut self.ref_stars {
            let mut ref_stars = ref_stars.lock().unwrap();
            *ref_stars = None;
        }
        if let Some(live_stacking) = &mut self.live_stacking {
            let mut adder = live_stacking.adder.write().unwrap();
            adder.clear();
        }

        self.fname_utils.init(&self.indi, &self.device);
        self.generate_output_file_names()?;

        self.start_or_continue()?;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.device)?;
        self.flags.skip_frame_done = false; // will skip first frame when continue
        Ok(())
    }

    fn continue_work(&mut self) -> anyhow::Result<()> {
        self.correct_options_before_start();
        self.update_options_copies();
        self.state = State::Common;

        // Restore original frame options
        // in saving raw or live stacking mode
        if self.cam_mode == CameraMode::SavingRawFrames
        || self.cam_mode == CameraMode::LiveStacking {
            let mut options = self.options.write().unwrap();
            options.cam.frame = self.cam_options.frame.clone();
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
        match (&self.cam_mode, &self.cam_options.frame.frame_type) {
            (CameraMode::SingleShot,      _                ) => return Ok(NotifyResult::Empty),
            (CameraMode::SavingRawFrames, FrameType::Flats ) => return Ok(NotifyResult::Empty),
            (CameraMode::SavingRawFrames, FrameType::Biases) => return Ok(NotifyResult::Empty),
            _ => {},
        }
        if self.cam_mode == CameraMode::LiveView {
            // We need fresh frame options in live view mode
            let options = self.options.read().unwrap();
            self.cam_options = options.cam.clone();
        }
        let fast_mode_enabled =
            self.indi.camera_is_fast_toggle_supported(&self.device.name).unwrap_or(false) &&
            self.indi.camera_is_fast_toggle_enabled(&self.device.name).unwrap_or(false);
        if !fast_mode_enabled {
            self.cur_exposure = self.cam_options.frame.exposure();
            if !self.cam_options.frame.have_to_use_delay() {
                apply_camera_options_and_take_shot(&self.indi, &self.device, &self.cam_options.frame)?;
            } else {
                let indi = Arc::clone(&self.indi);
                let camera = self.device.clone();
                let frame = self.cam_options.frame.clone();

                if let Some(thread_timer) = &self.timer {
                    thread_timer.exec((frame.delay * 1000.0) as u32, false, move || {
                        let res = apply_camera_options_and_take_shot(&indi, &camera, &frame);
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
        if self.state == State::FrameToSkip {
            *should_be_processed = false;
            self.state = State::Common;
            self.flags.skip_frame_done = true;
            self.start_or_continue()?;
            return Ok(NotifyResult::ModeStrChanged)
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult>  {
        match &fp_result.data {
            FrameProcessResultData::RawFrame(raw_image) =>
                self.process_raw_image(raw_image),

            FrameProcessResultData::LightFrameInfo(info) =>
                self.process_light_frame_info(info),

            FrameProcessResultData::ShotProcessingFinished {
                frame_is_ok, blob, raw_image_info, ..
            } =>
                self.process_frame_processing_finished_event(
                    *frame_is_ok,
                    blob,
                    raw_image_info,
                    &fp_result.cmd_stop_flag,
                ),

            FrameProcessResultData::RawHistogram(hist) =>
                self.process_raw_histogram(hist),

            _ =>
                Ok(NotifyResult::Empty)
        }
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        let options = self.options.read().unwrap();

        match self.cam_mode {
            CameraMode::SavingRawFrames => {
                if options.cam.frame.frame_type == FrameType::Lights
                && !options.mount.device.is_empty()
                && options.guiding.mode == GuidingMode::MainCamera {
                    cmd.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
                }
            },
            CameraMode::LiveStacking => {
                cmd.live_stacking = Some(LiveStackingParams {
                    data:    Arc::clone(self.live_stacking.as_ref().unwrap()),
                    options: options.live.clone(),
                });
                cmd.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
             },
            _ => {},
        }
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if self.state == State::InternalMountCorrection {
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
                    let continuously = self.is_continuous_mode();
                    init_cam_continuous_mode(
                        &self.indi,
                        &self.device,
                        &self.cam_options.frame,
                        continuously
                    )?;
                    apply_camera_options_and_take_shot(
                        &self.indi,
                        &self.device,
                        &self.cam_options.frame
                    )?;
                    self.state = State::Common;
                    result = NotifyResult::ModeStrChanged;
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
            if guid_options.mode == GuidingMode::External
            && self.state == State::ExternalDithering {
                match event {
                    ExtGuiderEvent::DitheringFinished => {
                        self.flags.skip_frame_done = false;
                        self.start_or_continue()?;
                        return Ok(NotifyResult::ModeStrChanged);
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
