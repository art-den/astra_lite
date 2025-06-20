use core::f64;
use std::{
    any::Any, path::PathBuf, sync::{atomic::AtomicBool, Arc, Mutex, RwLock}
};

use chrono::Utc;

use crate::{
    guiding::external_guider::*,
    image::{
        histogram::*,
        raw::{FrameType, RawImage, RawImageInfo},
        raw_stacker::RawStacker,
        stars_offset::*,
    },
    indi,
    options::*,
    utils::io_utils::*,
    TimeLogger
};

use super::{
    consts::*,
    core::*,
    events::*,
    frame_processing::*,
    mode_darks_lib::MasterFileCreationProgramItem,
    mode_focusing::FocuserEvent,
    mode_mnt_calib::*,
    utils::{FileNameArg, FileNameUtils},
};

#[derive(PartialEq)]
pub enum CameraMode {
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
    DefectPixels,
    MasterDark,
    MasterBias,
}

#[derive(PartialEq)]
enum State {
    FrameToSkip,
    Common,
    CameraOffsetCalculation,
    InternalMountCorrection(usize /* ok_time */),
    ExternalDithering,
}

enum NextJob {
    MountCalibration,
    ExternalDithering,
    InternalDithering { ra_pulse: f64, dec_pulse: f64 },
    Autofocus,
}

struct AutoFocuser {
    options:    FocuserOptions,
    exp_sum:    f64,
    start_temp: Option<f64>,
    fwhm:       Vec<f32>,
}

#[derive(Default)]
struct Flags {
    skip_frame_done:    bool,
    save_raw_files:     bool,
    use_raw_stacker:    bool,
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
    low_values: Vec<(u16, f32)>,
    high_values: Vec<(u16, f32)>,
}

// Guider data for guiding by main camera
struct SimpleGuider {
    mnt_calibr:     Option<MountMoveCalibrRes>,
    dither_x:       f64,
    dither_y:       f64,
}

impl SimpleGuider {
    fn new() -> Self {
        Self {
            mnt_calibr: None,
            dither_x: 0.0,
            dither_y: 0.0,
        }
    }
}

struct Guider {
    options:        GuidingOptions,
    dither_exp_sum: f64,
    simple:         Option<SimpleGuider>,
    external:       Option<Arc<ExternalGuiderCtrl>>,
}

pub struct TackingPicturesMode {
    cam_mode:         CameraMode,
    state:            State,
    device:           DeviceAndProp,
    mount_device:     String,
    fn_gen:           Arc<Mutex<SeqFileNameGen>>,
    indi:             Arc<indi::Connection>,
    subscribers:      Arc<EventSubscriptions>,
    raw_stacker:      RawStacker,
    options:          Arc<RwLock<Options>>,
    next_job:         Option<NextJob>,
    cam_options:      CamOptions,
    guider:           Option<Guider>,
    ref_stars:        Option<Vec<Point>>,
    progress:         Option<Progress>,
    cur_exposure:     f64,
    cur_shot_id:      Option<u64>,
    shot_id_to_ign:   Option<u64>,
    live_stacking:    Option<Arc<LiveStackingData>>,
    autofocuser:      Option<AutoFocuser>,
    flags:            Flags,
    fname_utils:      FileNameUtils,
    out_file_names:   OutFileNames,
    camera_offset:    Option<u16>,
    cam_offset_calc:  Option<CamOffsetCalc>,
    next_mode:        Option<ModeBox>,
    queue_overflowed: bool,
    slow_down_flag:   bool,
}

impl TackingPicturesMode {
    pub fn new(
        indi:        &Arc<indi::Connection>,
        subscribers: &Arc<EventSubscriptions>,
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

        let mut cam_options = opts.cam.clone();

        match cam_mode {
            CameraMode::LiveStacking =>
                cam_options.frame.frame_type = crate::image::raw::FrameType::Lights,

            CameraMode::MasterDark|
            CameraMode::DefectPixels =>
                cam_options.frame.frame_type = crate::image::raw::FrameType::Darks,

            CameraMode::MasterBias =>
                cam_options.frame.frame_type = crate::image::raw::FrameType::Biases,

            _ => {}
        }

        let working_with_light_frames =
            opts.cam.frame.frame_type == FrameType::Lights &&
            (cam_mode == CameraMode::SavingRawFrames ||
            cam_mode == CameraMode::LiveStacking);

        let guider = if working_with_light_frames {
            Some(Guider {
                options:        opts.guiding.clone(),
                dither_exp_sum: 0.0,
                simple:         None,
                external:       None,
            })
        } else {
            None
        };

        let autofocuser = if working_with_light_frames {
            Some(AutoFocuser {
                options:    opts.focuser.clone(),
                exp_sum:    0.0,
                start_temp: None,
                fwhm:       Vec::new(),
            })
        } else {
            None
        };

        Ok(Self {
            cam_mode,
            state:            State::Common,
            device:           cam_device.clone(),
            mount_device:     opts.mount.device.to_string(),
            fn_gen:           Arc::new(Mutex::new(SeqFileNameGen::new())),
            indi:             Arc::clone(indi),
            subscribers:      Arc::clone(subscribers),
            raw_stacker:      RawStacker::new(),
            options:          Arc::clone(options),
            next_job:         None,
            ref_stars:        None,
            cur_exposure:     0.0,
            cur_shot_id:      None,
            shot_id_to_ign:   None,
            live_stacking:    None,
            out_file_names:   OutFileNames::default(),
            camera_offset:    None,
            cam_offset_calc:  None,
            next_mode:        None,
            flags:            Flags::default(),
            fname_utils:      FileNameUtils::default(),
            queue_overflowed: false,
            slow_down_flag:   false,
            cam_options,
            autofocuser,
            progress,
            guider,
        })
    }

    pub fn set_external_guider(
        &mut self,
        ext_guider: &Arc<ExternalGuiderCtrl>
    ) {
        let Some(guider) = &mut self.guider else { return; };
        guider.external = Some(Arc::clone(ext_guider));
    }

    pub fn set_live_stacking(&mut self, live_stacking: &Arc<LiveStackingData>) {
        self.live_stacking = Some(Arc::clone(live_stacking));
    }

    pub fn set_dark_creation_program_item(&mut self, item: &MasterFileCreationProgramItem) {
        self.progress = Some(Progress {cur: 0, total: item.count});
        if let Some(temperature) = item.temperature {
            self.cam_options.ctrl.temperature = temperature;
            self.cam_options.ctrl.enable_cooler = true;
        }
        self.cam_options.frame.exp_main = item.exposure;
        self.cam_options.frame.exp_bias = item.exposure;
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
        if let Some(autofocuser) = &mut self.autofocuser {
            autofocuser.options = opts.focuser.clone();
        }
        if let Some(guider) = &mut self.guider {
            guider.options = opts.guiding.clone();
        }
        drop(opts);
    }

    fn correct_options_before_start(&mut self) {
        match self.cam_mode {
            CameraMode::LiveStacking => {
                let mut options = self.options.write().unwrap();
                options.cam.frame.frame_type = FrameType::Lights;
                self.cam_options.frame.frame_type = FrameType::Lights;
            }
            _ => {}
        }
    }

    fn take_shot_with_options(&mut self, frame_options: FrameOptions) -> anyhow::Result<()> {
        let cur_shot_id = apply_camera_options_and_take_shot(
            &self.indi,
            &self.device,
            &frame_options,
            &self.cam_options.ctrl
        )?;
        self.cur_shot_id = Some(cur_shot_id);
        self.cur_exposure = frame_options.exposure();
        Ok(())
    }

    fn start_or_continue(&mut self) -> anyhow::Result<()> {
        // First frame must be skiped
        // for saving frames and live stacking mode
        let need_skip_first_frame = matches!(
            self.cam_mode,
            CameraMode::SavingRawFrames|
            CameraMode::LiveStacking|
            CameraMode::DefectPixels|
            CameraMode::MasterDark|
            CameraMode::MasterBias
        );
        if !self.flags.skip_frame_done && need_skip_first_frame {
            self.start_first_shot_that_will_be_skipped()?;
            self.state = State::FrameToSkip;
            return Ok(());
        }

        if self.cam_mode == CameraMode::SavingRawFrames
        && self.cam_options.frame.frame_type == FrameType::Flats
        && self.cam_options.frame.offset != 0
        && self.camera_offset.is_none()
        && self.flags.save_master_file {
            let options = self.options.read().unwrap();
            let (subtract_file_name, _) = self.fname_utils.get_subtrack_master_fname(
                &FileNameArg::Options(&self.cam_options),
                &options.calibr.dark_library_path
            );
            drop(options);

            // we need to calculate real camera offset before creating master flat file
            // if no calibration file exists
            if !subtract_file_name.is_file() {
                self.cam_offset_calc = Some(CamOffsetCalc {
                    step: 0,
                    low_values: Vec::new(),
                    high_values: Vec::new(),
                });
                self.start_offset_calculation_shot()?;
                self.state = State::CameraOffsetCalculation;
                return Ok(());
            }
        }

        self.take_shot_with_options(self.cam_options.frame.clone())?;
        self.state = State::Common;
        Ok(())
    }

    fn start_first_shot_that_will_be_skipped(&mut self) -> anyhow::Result<()> {
        let mut frame_opts = self.cam_options.frame.clone();
        const MAX_EXP: f64 = 1.0;
        if frame_opts.exposure() > MAX_EXP {
            frame_opts.set_exposure(MAX_EXP);
        }
        self.take_shot_with_options(frame_opts)?;
        Ok(())
    }

    fn start_offset_calculation_shot(&mut self) -> anyhow::Result<()> {
        if let Some(offset_calc) = &self.cam_offset_calc {
            let mut frame_opts = self.cam_options.frame.clone();
            if offset_calc.step % 2 == 0 { frame_opts.offset = 0; }
            self.take_shot_with_options(frame_opts)?;
        }
        Ok(())
    }

    const MIN_EXPOSURE_FOR_DELAYED_CAPTURE_START: f64 = 3.0;

    fn have_to_start_new_exposure_at_blob_start(&mut self) -> bool {
        (
            self.cam_mode == CameraMode::MasterDark ||
            self.cam_options.frame.exposure() >= Self::MIN_EXPOSURE_FOR_DELAYED_CAPTURE_START
        ) && !self.slow_down_flag
    }

    fn generate_output_file_names(&mut self) -> anyhow::Result<()> {
        let options = self.options.read().unwrap();

        let time = Utc::now();

        // Calibration master file for saving

        if self.flags.save_master_file {
            let mut path = PathBuf::new();
            if matches!(self.cam_mode, CameraMode::MasterDark|CameraMode::MasterBias) {
                path.push(&options.calibr.dark_library_path);
                path.push(self.device.to_file_name_part());
            } else {
                path.push(&options.raw_frames.out_path);
            }
            let file_name = self.fname_utils.master_only_file_name(
                Some(time),
                &FileNameArg::Options(&self.cam_options),
                self.cam_options.frame.frame_type
            );
            path.push(&file_name);
            self.out_file_names.master_fname = path;
        }

        // Defect pixels file for saving

        if self.flags.save_defect_pixels {
            self.out_file_names.defect_pixels_fname = self.fname_utils.defect_pixels_file_name(
                &FileNameArg::Options(&self.cam_options),
                &options.calibr.dark_library_path
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
        info: &LightFrameInfoData,
        shot_id: Option<u64>,
    ) -> anyhow::Result<()> {
        let Some(autofocuser) = &mut self.autofocuser else {
            return Ok(());
        };
        if !self.indi.is_device_enabled(&autofocuser.options.device).unwrap_or(false) {
            return Ok(());
        }

        // push fwhm
        if let Some(fwhm) = info.stars.info.fwhm {
            if info.stars.info.ovality_is_ok {
                autofocuser.fwhm.push(fwhm);
            }
        }

        // Update exposure sum
        autofocuser.exp_sum += self.cam_options.frame.exposure();

        // Temperature measurement
        let temperature = self.indi
            .focuser_get_temperature(&autofocuser.options.device)
            .unwrap_or(f64::NAN);
        if !temperature.is_nan()
        && !temperature.is_infinite()
        && autofocuser.start_temp.is_none() {
            autofocuser.start_temp = Some(temperature);
            self.subscribers.notify(
                Event::Focusing(FocuserEvent::StartingTemperature(temperature))
            );
        }

        if self.next_job.is_some() {
            // Next job already assigned
            return Ok(());
        };

        let mut have_to_refocus = false;

        // Periodically
        if autofocuser.options.periodically
        && autofocuser.options.period_minutes != 0 {
            let max_exp_sum = (autofocuser.options.period_minutes * 60) as f64;
            if autofocuser.exp_sum >= max_exp_sum {
                log::info!(
                    "Start autofocus after period {} minutes",
                    autofocuser.options.period_minutes
                );
                have_to_refocus = true;
            }
        }

        // When temperature changed
        if autofocuser.options.on_temp_change
        && autofocuser.options.max_temp_change > 0.0
        && autofocuser.start_temp.is_some() {
            let first = autofocuser.start_temp.unwrap();
            let last = temperature;
            let delta = last - first;
            if delta.abs() >= autofocuser.options.max_temp_change {
                log::info!(
                    "Start autofocus after temperature change. \
                    first={:.1}°, last={:.1}°, delta={:.1}°",
                    first, last, delta
                );
                have_to_refocus = true;
            }
        }

        // On FWHM increase
        if autofocuser.options.on_fwhm_change
        && autofocuser.options.max_fwhm_change != 0
        && autofocuser.fwhm.len() >= 2 {
            let min = autofocuser.fwhm
                .iter()
                .min_by(|a, b| a.total_cmp(b))
                .copied()
                .unwrap_or_default() as f64;
            let max = autofocuser.fwhm
                .iter()
                .max_by(|a, b| a.total_cmp(b))
                .copied()
                .unwrap_or_default() as f64;
            if max > min && min != 0.0 {
                let diff_percent = (100.0 * (max - min) / min) as u32;
                if diff_percent > autofocuser.options.max_fwhm_change {
                    log::info!(
                        "Start autofocus after FWHM increase: \
                        min={:.1}, max={:.1}, diff={:.0}%",
                        min, max, diff_percent
                    );
                    have_to_refocus = true;
                }
            }
        }

        if have_to_refocus {
            autofocuser.exp_sum = 0.0;
            autofocuser.start_temp = Some(temperature);
            autofocuser.fwhm.clear();
            self.abort_current_unfinised_exposure(shot_id)?;
            self.next_job = Some(NextJob::Autofocus);
        }

        Ok(())
    }

    fn process_light_frame_info_and_dither_by_main_camera(
        &mut self,
        info:    &LightFrameInfoData,
        shot_id: Option<u64>,
    ) -> anyhow::Result<()> {
        if !info.stars.info.is_ok() {
            return Ok(());
        }

        let mount_device_active = self.indi.is_device_enabled(&self.mount_device).unwrap_or(false);
        if !mount_device_active {
            return Ok(());
        }

        let Some(guider) = &mut self.guider else {
            return Ok(());
        };

        if guider.options.mode != GuidingMode::MainCamera {
            return Ok(());
        }

        let guider_data = guider.simple.get_or_insert_with(SimpleGuider::new);
        if guider.options.is_used()
        && guider_data.mnt_calibr.is_none()
        && self.next_job.is_none() {
            self.abort_current_unfinised_exposure(shot_id)?;
            self.next_job = Some(NextJob::MountCalibration);
            return Ok(());
        }

        let mut move_offset = None;
        let mut dithering_flag = false;

        // dithering
        if guider.options.dith_period != 0 {
            guider.dither_exp_sum += info.image.exposure;
            if guider.dither_exp_sum >= (guider.options.dith_period * 60) as f64 {
                guider.dither_exp_sum = 0.0;
                use rand::prelude::*;
                let mut rng = rand::thread_rng();
                guider_data.dither_x = guider.options.main_cam.dith_dist as f64 * (rng.gen::<f64>() - 0.5);
                guider_data.dither_y = guider.options.main_cam.dith_dist as f64 * (rng.gen::<f64>() - 0.5);
                log::debug!("dithering position = {}px,{}px", guider_data.dither_x, guider_data.dither_y);
                dithering_flag = true;
            }
        }

        // guiding
        if let Some(offset) = &info.stars.offset {
            let mut offset_x = offset.x;
            let mut offset_y = offset.y;
            offset_x -= guider_data.dither_x;
            offset_y -= guider_data.dither_y;
            let diff_dist = f64::sqrt(offset_x * offset_x + offset_y * offset_y);
            log::debug!(
                "offset_x = {:.1}px, offset_y ={:.1}px, diff_dist = {:.1}px",
                offset_x, offset_y, diff_dist
            );
            if diff_dist > guider.options.main_cam.max_error || dithering_flag {
                if diff_dist < 2.0 * guider.options.main_cam.max_error {
                    offset_x *= 0.5;
                    offset_y *= 0.5;
                }
                move_offset = Some((-offset_x, -offset_y));
                log::debug!(
                    "diff_dist > guid_options.max_error ({} > {}), start mount correction",
                    diff_dist,
                    guider.options.main_cam.max_error
                );
            }
        }

        // Move mount position
        if let (Some((offset_x, offset_y)), Some(mnt_calibr)) = (move_offset, &guider_data.mnt_calibr) {
            if mnt_calibr.is_ok() && self.next_job.is_none() {
                if let Some((ra_pulse, dec_pulse)) = mnt_calibr.calc(offset_x, offset_y) {
                    self.abort_current_unfinised_exposure(shot_id)?;
                    self.next_job = Some(NextJob::InternalDithering { ra_pulse, dec_pulse });
                }
            }
        }

        Ok(())
    }

    fn start_dithering_in_main_cam_mode(
        &mut self,
        mut ra_pulse: f64,
        mut dec_pulse: f64
    ) -> anyhow::Result<()> {
        let can_set_guide_rate =
            self.indi.mount_is_guide_rate_supported(&self.mount_device)? &&
            self.indi.mount_get_guide_rate_prop_data(&self.mount_device)?.permition == indi::PropPermition::RW;
        if can_set_guide_rate {
            self.indi.mount_set_guide_rate(
                &self.mount_device,
                MOUNT_CALIBR_SPEED,
                MOUNT_CALIBR_SPEED,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }
        let (max_dec, max_ra) = self.indi.mount_get_timed_guide_max(&self.mount_device)?;
        let max_dec = f64::min(MAX_TIMED_GUIDE_TIME * 1000.0, max_dec).round();
        let max_ra = f64::min(MAX_TIMED_GUIDE_TIME * 1000.0, max_ra).round();
        ra_pulse = (ra_pulse * 1000.0).round();
        dec_pulse = (dec_pulse * 1000.0).round();
        if ra_pulse > max_ra { ra_pulse = max_ra; }
        if ra_pulse < -max_ra { ra_pulse = -max_ra; }
        if dec_pulse > max_dec { dec_pulse = max_dec; }
        if dec_pulse < -max_dec { dec_pulse = -max_dec; }
        log::debug!("Timed guide, NS = {:.2}ms, WE = {:.2}ms", dec_pulse, ra_pulse);
        self.indi.mount_timed_guide(&self.mount_device, dec_pulse, ra_pulse)?;
        self.state = State::InternalMountCorrection(0);
        Ok(())
    }

    fn process_light_frame_info_and_dither_by_ext_guider(
        &mut self,
        info:    &LightFrameInfoData,
        shot_id: Option<u64>,
    ) -> anyhow::Result<()> {
        if !info.stars.info.is_ok() {
            return Ok(());
        }

        let Some(guider) = &mut self.guider else {
            return Ok(());
        };

        if guider.options.mode != GuidingMode::External
        || guider.options.dith_period == 0 {
            return Ok(())
        }

        let Some(ext_guider) = &guider.external else {
            return Ok(());
        };

        if !ext_guider.is_connected() {
            return Ok(());
        }

        guider.dither_exp_sum += info.image.exposure;
        if guider.dither_exp_sum >= (guider.options.dith_period * 60) as f64
        && self.next_job.is_none() {
            guider.dither_exp_sum = 0.0;
            self.abort_current_unfinised_exposure(shot_id)?;
            self.next_job = Some(NextJob::ExternalDithering);
        }
        Ok(())
    }

    fn start_dithering_in_external_guider_mode(&mut self) -> anyhow::Result<()> {
        let guider = self.guider.as_ref().expect("self.guider");
        let external_guider = guider.external.as_ref().expect("guider.external");
        let dist = guider.options.ext_guider.dith_dist;
        log::info!("Starting dithering by external guider with {} pixels...", dist);
        external_guider.start_dithering(dist)?;
        self.state = State::ExternalDithering;
        Ok(())
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
                // continue after camera offset calculation for flats
                self.start_or_continue()?;
                return Ok(NotifyResult::ProgressChanges);
            }
        }

        let mut result = NotifyResult::Empty;

        if self.state == State::Common {
            if frame_is_ok && self.flags.save_raw_files {
                // Save raw image
                self.save_raw_image(blob, raw_image_info)?;
            }

            let mut is_last_frame = false;
            if let Some(progress) = &mut self.progress {
                if frame_is_ok && progress.cur != progress.total {
                    progress.cur += 1;
                    result = NotifyResult::ProgressChanges;
                }
                if progress.cur == progress.total {
                    self.abort_current_unfinised_exposure(blob.shot_id)?;
                    result = NotifyResult::Finished {
                        next_mode: self.next_mode.take()
                    };
                    is_last_frame = true;
                }
            }

            if is_last_frame && self.flags.save_master_file {
                // Save master file
                self.save_master_file()?;

                let result = FrameProcessResultData::MasterSaved {
                    frame_type: raw_image_info.frame_type,
                    file_name: self.out_file_names.master_fname.clone()
                };

                let event_data = FrameProcessResult {
                    camera:        self.device.clone(),
                    shot_id:       blob.shot_id,
                    cmd_stop_flag: Arc::clone(cmd_stop_flag),
                    mode_type:     self.get_type(),
                    data:          result,
                };

                self.subscribers.notify(Event::FrameProcessing(event_data));
            }

            if is_last_frame && self.flags.save_defect_pixels {
                self.save_defect_pixels_file()?;
            }
        }

        let finished = matches!(result, NotifyResult::Finished {..});

        if !finished {
            // Start next job/mode
            match self.next_job.take() {
                Some(NextJob::MountCalibration) =>
                    return Ok(NotifyResult::StartMountCalibr),

                Some(NextJob::InternalDithering { ra_pulse, dec_pulse }) => {
                    self.start_dithering_in_main_cam_mode(ra_pulse, dec_pulse)?;
                    return Ok(NotifyResult::ProgressChanges);
                }

                Some(NextJob::ExternalDithering) => {
                    self.start_dithering_in_external_guider_mode()?;
                    return Ok(NotifyResult::ProgressChanges);
                }

                Some(NextJob::Autofocus) =>
                    return Ok(NotifyResult::StartFocusing),

                _ => {},
            }
        }

        // Start next exposure
        if self.state == State::Common
        && self.cam_mode != CameraMode::SingleShot
        && !finished
        && !self.have_to_start_new_exposure_at_blob_start() {
            self.take_shot_with_options(self.cam_options.frame.clone())?;
        }

        // Do we have to slow down with period of tacking camera images?
        if self.queue_overflowed {
            self.queue_overflowed = false;
            self.slow_down_flag = true;
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
                let mut min_deviation_diff = f32::MAX;
                let mut result_value = 0i32;
                for (m1, d1) in &offset_calc.low_values {
                    for (m2, d2) in &offset_calc.high_values {
                        let dev_diff = f32::abs(d1 - d2);
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
                result = NotifyResult::ProgressChanges;
            }
        }

        Ok(result)
    }

    fn add_raw_image(&mut self, raw_image: &RawImage) -> anyhow::Result<()> {
        if raw_image.info().frame_type == FrameType::Flats {
            let mut normalized_flat = raw_image.clone();
            let tmr = TimeLogger::start();
            let flat_offset = self.camera_offset.unwrap_or_default();
            if flat_offset != 0 {
                normalized_flat.set_offset(flat_offset as i32);
            }
            normalized_flat.normalize_flat();
            tmr.log("Normalizing flat");
            let tmr = TimeLogger::start();
            self.raw_stacker.add(&normalized_flat, false)?;
            tmr.log("Adding raw calibration frame");
        } else {
            let tmr = TimeLogger::start();
            self.raw_stacker.add(raw_image, true)?;
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
        let raw_image = self.raw_stacker.get()?;
        self.raw_stacker.clear();

        if let Some(parent) = self.out_file_names.master_fname.parent() {
            if !parent.is_dir() {
                log::debug!("Creating directory {} ...", parent.to_str().unwrap_or_default());
                std::fs::create_dir_all(parent)?;
            }
        }

        raw_image.save_to_fits_file(&self.out_file_names.master_fname)?;

        log::debug!("Master frame saved!");
        Ok(())
    }

    fn save_defect_pixels_file(&mut self) -> anyhow::Result<()> {
        log::debug!("Saving defect pixels file...");
        let raw_image = self.raw_stacker.get()?;
        self.raw_stacker.clear();

        if let Some(parent) = self.out_file_names.defect_pixels_fname.parent() {
            if !parent.is_dir() {
                log::debug!("Creating directory {} ...", parent.to_str().unwrap_or_default());
                std::fs::create_dir_all(parent)?;
            }
        }

        let defect_pixels = raw_image.find_hot_pixels_in_master_dark();

        defect_pixels.save_to_file(&self.out_file_names.defect_pixels_fname)?;
        log::debug!("Defect pixels file saved!");

        Ok(())
    }

    fn is_frame_type_for_raw_stacker(frame_type: FrameType) -> bool {
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

        let frame_for_raw_stacker =
            Self::is_frame_type_for_raw_stacker(raw_image.info().frame_type);

        if frame_for_raw_stacker && self.flags.use_raw_stacker {
            self.add_raw_image(raw_image)?;
        }
        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info(
        &mut self,
        info:    &LightFrameInfoData,
        shot_id: Option<u64>,
    ) -> anyhow::Result<NotifyResult> {
        if self.state != State::Common {
            return Ok(NotifyResult::Empty);
        }

        if info.stars.info.is_ok() && self.ref_stars.is_none() {
            let ref_stars = info.stars.items.iter().map(|s| Point {x: s.x, y: s.y}).collect();
            self.ref_stars = Some(ref_stars);
        }

        self.process_light_frame_info_and_refocus(info, shot_id)?;

        self.process_light_frame_info_and_dither_by_main_camera(info, shot_id)?;

        self.process_light_frame_info_and_dither_by_ext_guider(info, shot_id)?;

        Ok(NotifyResult::Empty)
    }

    fn get_dark_or_bias_creation_short_info(&self) -> String {
        let mut result = String::new();
        if self.cam_options.ctrl.enable_cooler {
            result += &format!("{:.1}°С ", self.cam_options.ctrl.temperature);
        }
        result += &format!(
            "{}s g:{:.0} offs:{}",
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

    fn abort_current_unfinised_exposure(&mut self, shot_id: Option<u64>) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.device)?;
        if self.cur_shot_id != shot_id {
            self.shot_id_to_ign = self.cur_shot_id;
        }
        Ok(())
    }

}

impl Mode for TackingPicturesMode {
    fn get_type(&self) -> ModeType {
        match self.cam_mode {
            CameraMode::SingleShot      => ModeType::SingleShot,
            CameraMode::LiveView        => ModeType::LiveView,
            CameraMode::SavingRawFrames => ModeType::SavingRawFrames,
            CameraMode::LiveStacking    => ModeType::LiveStacking,
            CameraMode::DefectPixels    => ModeType::DefectPixels,
            CameraMode::MasterDark      => ModeType::MasterDark,
            CameraMode::MasterBias      => ModeType::MasterBias,
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.device)
    }

    fn progress_string(&self) -> String {
        let mut mode_str = match (&self.state, &self.cam_mode) {
            (State::FrameToSkip, _) =>
                "First frame (will be skipped)".to_string(),
            (State::InternalMountCorrection(_), _) =>
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
            (_, CameraMode::DefectPixels) =>
                format!(
                    "Creating defective pixels files ({})",
                    self.get_defect_pixels_creation_short_info()
                ),
            (_, CameraMode::MasterDark) =>
                format!(
                    "Creating master dark ({})",
                    self.get_dark_or_bias_creation_short_info()
                ),
            (_, CameraMode::MasterBias) =>
                format!(
                    "Creating master bias ({})",
                    self.get_dark_or_bias_creation_short_info()
                ),
            (_, CameraMode::LiveStacking) =>
                "Live stacking".to_string(),
        };
        let mut extra_modes = Vec::new();
        if matches!(self.cam_mode, CameraMode::SavingRawFrames|CameraMode::LiveStacking)
        && self.cam_options.frame.frame_type == FrameType::Lights
        && self.state == State::Common {
            if let Some(autofocuser) = &self.autofocuser {
                if autofocuser.options.on_fwhm_change
                || autofocuser.options.on_temp_change
                || autofocuser.options.periodically {
                    extra_modes.push("F");
                }
            }
            if let Some(guider) = &self.guider {
                if guider.options.is_used() {
                    extra_modes.push("G");
                    if guider.options.dith_period != 0 {
                        extra_modes.push("D");
                    }
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
        Some(self.cur_exposure)
    }

    fn can_be_stopped(&self) -> bool {
        matches!(
            &self.cam_mode,
            CameraMode::SingleShot |
            CameraMode::SavingRawFrames|
            CameraMode::DefectPixels|
            CameraMode::MasterDark|
            CameraMode::MasterBias|
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
            CameraMode::MasterDark|CameraMode::MasterBias =>
                true,
            _ =>
                false,
        };
        self.flags.save_defect_pixels = matches!(self.cam_mode, CameraMode::DefectPixels);
        self.flags.use_raw_stacker =
            self.flags.save_master_file ||
            self.flags.save_defect_pixels;

        drop(options);

        self.fname_utils.init(&self.indi, &self.device);
        self.generate_output_file_names()?;

        if self.flags.use_raw_stacker {
            self.raw_stacker.clear();
        }

        if let Some(autofocuser) = &mut self.autofocuser {
            let temperature = self.indi
                .focuser_get_temperature(&autofocuser.options.device)
                .unwrap_or(f64::NAN);
            if !temperature.is_nan()
            && !temperature.is_infinite()
            && autofocuser.start_temp.is_none() {
                autofocuser.start_temp = Some(temperature);
                self.subscribers.notify(
                    Event::Focusing(FocuserEvent::StartingTemperature(temperature))
                );
            }
        }

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
            let Some(guider) = &mut self.guider else { return; };
            let dith_data = guider.simple.get_or_insert_with(SimpleGuider::new);
            dith_data.mnt_calibr = Some(value.clone());
            log::debug!("New mount calibration set: {:?}", dith_data.mnt_calibr);
        }
    }

    fn notify_blob_start_event(
        &mut self,
        event: &indi::BlobStartEvent
    ) -> anyhow::Result<NotifyResult> {
        if self.shot_id_to_ign == event.shot_id {
            return Ok(NotifyResult::Empty);
        }

        if *event.device_name != self.device.name
        || *event.prop_name != self.device.prop
        || self.state == State::FrameToSkip {
            return Ok(NotifyResult::Empty);
        }

        if self.cam_mode == CameraMode::LiveView {
            // We need fresh frame options in live view mode
            let options = self.options.read().unwrap();
            self.cam_options = options.cam.clone();
        }

        if self.cam_mode != CameraMode::SingleShot
        && self.have_to_start_new_exposure_at_blob_start() {
            self.take_shot_with_options(self.cam_options.frame.clone())?;
        }

        Ok(NotifyResult::Empty)
    }

    fn notify_before_frame_processing_start(
        &mut self,
        blob: &Arc<indi::BlobPropValue>,
        should_be_processed: &mut bool
    ) -> anyhow::Result<NotifyResult> {
        self.next_job = None;

        if self.shot_id_to_ign == blob.shot_id {
            *should_be_processed = false;
            return Ok(NotifyResult::Empty);
        }

        if self.state == State::FrameToSkip {
            *should_be_processed = false;
            self.state = State::Common;
            self.flags.skip_frame_done = true;
            self.start_or_continue()?;
            return Ok(NotifyResult::ProgressChanges)
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        if self.shot_id_to_ign == fp_result.shot_id {
            return Ok(NotifyResult::Empty);
        }

        match &fp_result.data {
            FrameProcessResultData::RawFrame(raw_image) =>
                self.process_raw_image(raw_image),

            FrameProcessResultData::LightFrameInfo(info) =>
                self.process_light_frame_info(info, fp_result.shot_id),

            FrameProcessResultData::HistorgamRaw(histogram) =>
                self.process_raw_histogram(histogram),

            FrameProcessResultData::ShotProcessingFinished {
                frame_is_ok, blob, raw_image_info, ..
            } =>
                self.process_frame_processing_finished_event(
                    *frame_is_ok,
                    blob,
                    raw_image_info,
                    &fp_result.cmd_stop_flag,
                ),

            _ =>
                Ok(NotifyResult::Empty),
        }
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        if self.shot_id_to_ign == cmd.shot_id {
            return;
        }

        let options = self.options.read().unwrap();

        match self.cam_mode {
            CameraMode::SavingRawFrames => {
                if options.cam.frame.frame_type == FrameType::Lights
                && !options.mount.device.is_empty()
                && options.guiding.mode == GuidingMode::MainCamera {
                    cmd.ref_stars = self.ref_stars.clone();
                }
            },
            CameraMode::LiveStacking => {
                cmd.live_stacking = Some(LiveStackingParams {
                    data:    Arc::clone(self.live_stacking.as_ref().unwrap()),
                    options: options.live.clone(),
                });
                cmd.ref_stars = self.ref_stars.clone();
             },
            _ => {},
        }

        if let Some(calibr_params) = &mut cmd.calibr_params {
            calibr_params.flat_fname =
                if options.calibr.flat_frame_en
                && !options.calibr.flat_frame_fname.as_os_str().is_empty() {
                    Some(options.calibr.flat_frame_fname.clone())
                } else {
                    None
                };
        }
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        match &mut self.state {
            State::InternalMountCorrection(ok_time) => {
                let guide_pulse_finished = self.indi.mount_is_timed_guide_finished(&self.mount_device)?;
                if guide_pulse_finished {
                    *ok_time += 1;
                    if *ok_time == AFTER_MOUNT_MOVE_WAIT_TIME {
                        self.indi.mount_abort_motion(&self.mount_device)?;
                        self.take_shot_with_options(self.cam_options.frame.clone())?;
                        self.state = State::Common;
                        result = NotifyResult::ProgressChanges;
                    }
                }
            }
            _ => {}
        }
        Ok(result)
    }

    fn notify_guider_event(
        &mut self,
        event: ExtGuiderEvent
    ) -> anyhow::Result<NotifyResult> {
        if let Some(guider) = &self.guider {
            if guider.options.mode == GuidingMode::External
            && self.state == State::ExternalDithering {
                match event {
                    ExtGuiderEvent::DitheringFinished => {
                        self.flags.skip_frame_done = false;
                        self.start_or_continue()?;
                        return Ok(NotifyResult::ProgressChanges);
                    }
                    ExtGuiderEvent::DitheringFinishedWithErr(err) => {
                        log::error!("Dithering finished with error: {}", err);
                        // continue any way
                        self.flags.skip_frame_done = false;
                        self.start_or_continue()?;
                        return Ok(NotifyResult::ProgressChanges);
                    }
                    ExtGuiderEvent::Error(error) =>
                        return Err(anyhow::anyhow!("External guider error: {}", error)),
                    _ => {}
                }
            }
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_processing_queue_overflow(&mut self) -> anyhow::Result<NotifyResult> {
        self.queue_overflowed = true;
        Ok(NotifyResult::Empty)
    }
}
