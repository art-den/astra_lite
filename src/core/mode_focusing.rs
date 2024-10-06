use std::{
    sync::{Arc, RwLock},
    collections::VecDeque
};
use crate::{
    indi,
    options::*,
    utils::math::*,
    image::info::LightFrameInfo,
};
use super::{core::*, frame_processing::*, utils::*};

const MAX_FOCUS_TOTAL_TRY_CNT: usize = 8;
const MAX_FOCUS_SAMPLE_TRY_CNT: usize = 4;
const MAX_FOCUS_STAR_OVALITY: f32 = 2.0;

#[derive(Clone)]
pub struct FocusingResultData {
    pub samples: Vec<FocuserSample>,
    pub coeffs:  Option<SquareCoeffs>,
    pub result:  Option<f64>,
}

#[derive(Clone)]
pub enum FocusingStateEvent {
    Data(FocusingResultData),
    Result { value: f64 }
}

#[derive(PartialEq, Debug)]
enum FocusingStage {
    Undef,
    Preliminary,
    Final
}

pub struct FocusingMode {
    indi:        Arc<indi::Connection>,
    subscribers: Arc<RwLock<Subscribers>>,
    state:       FocusingState,
    camera:      DeviceAndProp,
    f_options:   FocuserOptions,
    cam_opts:    CamOptions,
    before_pos:  f64,
    to_go:       VecDeque<f64>,
    samples:     Vec<FocuserSample>,
    result_pos:  Option<f64>,
    try_cnt:     usize,
    stage:       FocusingStage,
    next_mode:   Option<Box<dyn Mode + Sync + Send>>,
}

#[derive(PartialEq)]
enum FocusingState {
    Undefined,
    WaitingPositionAntiBacklash{
        anti_backlash_pos: f64,
        target_pos: f64
    },
    WaitingPosition(f64),
    WaitingFrame(f64),
    WaitingResultPosAntiBacklash{
        anti_backlash_pos: f64,
        target_pos: f64
    },
    WaitingResultPos(f64),
    WaitingResultImg,
}

#[derive(Clone)]
pub struct FocuserSample {
    pub focus_pos:     f64,
    pub stars_fwhm:    f32,
}

impl FocusingMode {
    pub fn new(
        indi:        &Arc<indi::Connection>,
        options:     &Arc<RwLock<Options>>,
        subscribers: &Arc<RwLock<Subscribers>>,
        next_mode:   Option<Box<dyn Mode + Sync + Send>>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };
        let mut cam_opts = opts.cam.clone();
        cam_opts.frame.frame_type = crate::image::raw::FrameType::Lights;
        cam_opts.frame.exp_main = opts.focuser.exposure;
        cam_opts.frame.gain = gain_to_value(
            opts.focuser.gain,
            opts.cam.frame.gain,
            &cam_device,
            indi
        )?;

        Ok(FocusingMode {
            indi:        Arc::clone(indi),
            subscribers: Arc::clone(subscribers),
            state:       FocusingState::Undefined,
            f_options:   opts.focuser.clone(),
            cam_opts,
            before_pos:  0.0,
            to_go:       VecDeque::new(),
            samples:     Vec::new(),
            result_pos:  None,
            stage:       FocusingStage::Undef,
            try_cnt:     0,
            next_mode,
            camera:      cam_device.clone(),
        })
    }

    fn start_stage(
        &mut self,
        middle_pos: f64,
        stage:      FocusingStage
    ) -> anyhow::Result<()> {
        log::debug!("Starting autofocus stage {:?} for midle value {}", stage, middle_pos);
        self.samples.clear();
        self.to_go.clear();
        for step in 0..self.f_options.measures {
            let step = step as f64;
            let half_progress = (self.f_options.measures as f64 - 1.0) / 2.0;
            let pos_to_go = middle_pos + self.f_options.step * (step - half_progress);
            self.to_go.push_back(pos_to_go);
        }
        self.stage = stage;
        self.start_sample(true)?;
        Ok(())
    }

    fn start_sample(
        &mut self,
        first_time: bool
    ) -> anyhow::Result<()> {
        let Some(pos) = self.to_go.pop_front() else {
            return Ok(());
        };
        if !first_time {
            log::debug!("Setting focuser value: {}", pos);
            self.indi.focuser_set_abs_value(&self.f_options.device, pos, true, None)?;
            self.state = FocusingState::WaitingPosition(pos);
        } else {
            let mut anti_backlash_pos = pos - self.f_options.step;
            let cur_pos = self.indi.focuser_get_abs_value(&self.f_options.device)?;
            if f64::abs(anti_backlash_pos - cur_pos) < 1.0 {
                anti_backlash_pos -= 1.0;
            }
            log::debug!("Setting focuser value for avoiding backlash: {}", pos);
            self.indi.focuser_set_abs_value(&self.f_options.device, anti_backlash_pos, true, None)?;
            self.state = FocusingState::WaitingPositionAntiBacklash{
                anti_backlash_pos,
                target_pos: pos
            };
        }
        Ok(())
    }

    fn process_light_frame_info(
        &mut self,
        info: &LightFrameInfo,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if let FocusingState::WaitingFrame(focus_pos) = self.state {
            log::debug!(
                "New frame with ovality={:?} and {:?}",
                info.stars.ovality, info.stars.fwhm
            );

            let mut ok = false;
            if let (Some(stars_ovality), Some(stars_fwhm))
            = (info.stars.ovality, info.stars.fwhm) {
                self.try_cnt = 0;
                if stars_ovality < MAX_FOCUS_STAR_OVALITY {
                    let sample = FocuserSample {
                        focus_pos,
                        stars_fwhm
                    };
                    self.samples.push(sample);
                    self.samples.sort_by(|s1, s2| cmp_f64(&s1.focus_pos, &s2.focus_pos));
                    ok = true;
                    self.try_cnt = 0;
                    log::debug!("Ovality is Ok. Samples count = {}", self.samples.len());
                }
                let event_data = FocusingResultData {
                    samples: self.samples.clone(),
                    coeffs: None,
                    result: None,
                };
                self.subscribers.read().unwrap().inform_event(CoreEvent::Focusing(
                    FocusingStateEvent::Data(event_data)
                ));
            } else {
                self.try_cnt += 1;
            }
            let too_much_total_tries =
                self.try_cnt >= MAX_FOCUS_TOTAL_TRY_CNT &&
                !self.samples.is_empty();
            if ok
            || self.try_cnt >= MAX_FOCUS_SAMPLE_TRY_CNT
            || too_much_total_tries {
                result = NotifyResult::ProgressChanges;
                if self.to_go.is_empty() || too_much_total_tries {
                    log::debug!(
                        "Trying to calcilate extremum. Ok={}, self.try_cnt={}, self.samples.len={}",
                        ok, self.try_cnt, self.samples.len(),
                    );

                    let mut x = Vec::new();
                    let mut y = Vec::new();
                    for sample in &self.samples {
                        x.push(sample.focus_pos);
                        y.push(sample.stars_fwhm as f64);
                    }
                    let coeffs = square_ls(&x, &y)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus function"))?;

                    log::debug!("Calculated coefficients = {:?}", coeffs);
                    if coeffs.a2 <= 0.0 {
                        let event_data = FocusingResultData {
                            samples: self.samples.clone(),
                            coeffs: Some(coeffs.clone()),
                            result: None,
                        };
                        self.subscribers.read().unwrap().inform_event(CoreEvent::Focusing(
                            FocusingStateEvent::Data(event_data)
                        ));
                        anyhow::bail!("Wrong focuser curve result");
                    }
                    let extr = parabola_extremum(&coeffs)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus extremum"))?;

                    log::debug!("Calculated extremum = {}", extr);

                    let event_data = FocusingResultData {
                        samples: self.samples.clone(),
                        coeffs: Some(coeffs.clone()),
                        result: Some(extr),
                    };
                    self.subscribers.read().unwrap().inform_event(CoreEvent::Focusing(
                        FocusingStateEvent::Data(event_data)
                    ));
                    let focuser_info = self.indi.focuser_get_abs_value_prop_info(&self.f_options.device)?;
                    if extr < focuser_info.min || extr > focuser_info.max {
                        anyhow::bail!(
                            "Focuser extremum {0:.1} out of focuser range ({1:.1}..{2:.1})",
                            extr, focuser_info.min, focuser_info.max
                        );
                    }
                    let min_sample_pos = self.samples.iter().map(|v|v.focus_pos).min_by(cmp_f64).unwrap_or_default();
                    let max_sample_pos = self.samples.iter().map(|v|v.focus_pos).max_by(cmp_f64).unwrap_or_default();
                    let min_acceptable = min_sample_pos + (max_sample_pos-min_sample_pos) * 0.33;
                    let max_acceptable = min_sample_pos + (max_sample_pos-min_sample_pos) * 0.66;
                    if extr < min_acceptable || extr > max_acceptable {
                        // Result is too far from center of samples.
                        // Will do more measures.

                        log::debug!("Results too far from center. Do more measures...");
                        self.to_go.clear();
                        if extr < min_acceptable {
                            log::debug!("... in minimum area");
                            for i in (1..(self.f_options.measures+1)/2).rev() {
                                self.to_go.push_back(min_sample_pos - i as f64 * self.f_options.step);
                            }
                        } else {
                            log::debug!("... in maximum area");
                            for i in 1..(self.f_options.measures+1)/2 {
                                self.to_go.push_back(max_sample_pos + i as f64 * self.f_options.step);
                            }
                        }

                        self.start_sample(true)?;
                        return Ok(result);
                    }

                    let result_pos = extr.round();

                    if self.stage == FocusingStage::Preliminary {
                        self.start_stage(result_pos, FocusingStage::Final)?;
                        result = NotifyResult::ModeStrChanged;
                        return Ok(result)
                    }

                    self.result_pos = Some(result_pos);

                    // for anti-backlash
                    let anti_backlash_pos = result_pos - self.f_options.step;
                    log::debug!(
                        "Set RESULT focuser value for anti backlash {}",
                        anti_backlash_pos
                    );
                    self.indi.focuser_set_abs_value(
                        &self.f_options.device,
                        anti_backlash_pos,
                        true,
                        None
                    )?;
                    self.state = FocusingState::WaitingResultPosAntiBacklash {
                        anti_backlash_pos,
                        target_pos: result_pos
                    };
                    let result_event = FocusingStateEvent::Result { value: result_pos };
                    self.subscribers.read().unwrap().inform_event(CoreEvent::Focusing(result_event));
                } else {
                    self.start_sample(false)?;
                }
            } else {
                log::debug!("Received image is not Ok. Taking another one...");
                init_cam_continuous_mode(&self.indi, &self.camera, &self.cam_opts.frame, false)?;
                apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
            }
        }
        if self.state == FocusingState::WaitingResultImg {
            log::debug!(
                "RESULT shot is finished. Exiting focusing mode. Final FWHM = {:?}",
                info.stars.fwhm
            );
            result = NotifyResult::Finished { next_mode: self.next_mode.take() };
        }
        Ok(result)
    }
}

impl Mode for FocusingMode {
    fn get_type(&self) -> ModeType {
        ModeType::Focusing
    }

    fn progress_string(&self) -> String {
        match self.stage {
            FocusingStage::Preliminary =>
                "Focusing (preliminary)".to_string(),
            FocusingStage::Final =>
                "Focusing (final)".to_string(),
            _ => unreachable!(),
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn cam_opts(&self) -> Option<&CamOptions> {
        Some(&self.cam_opts)
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.samples.len(),
            total: self.samples.len() + self.to_go.len() + 1
        })
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exposure())
    }

    fn can_be_continued_after_stop(&self) -> bool {
        false
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let cur_pos = self.indi.focuser_get_abs_value(&self.f_options.device)?.round();
        self.before_pos = cur_pos;
        self.start_stage(cur_pos, FocusingStage::Preliminary)?;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.camera)?;
        self.indi.focuser_set_abs_value(&self.f_options.device, self.before_pos, true, None)?;
        Ok(())
    }

    fn take_next_mode(&mut self) -> Option<ModeBox> {
        self.next_mode.take()
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        if let Some(quality_options) = &mut cmd.quality_options {
            quality_options.use_max_fwhm = false;
        }
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        if *prop_change.device_name != self.f_options.device {
            return Ok(NotifyResult::Empty);
        }
        if let ("ABS_FOCUS_POSITION", indi::PropChange::Change { value, .. })
        = (prop_change.prop_name.as_str(), &prop_change.change) {
            let cur_focus = value.prop_value.to_f64()?;
            match self.state {
                FocusingState::WaitingPositionAntiBacklash { anti_backlash_pos, target_pos } => {
                    if f64::abs(cur_focus-anti_backlash_pos) < 1.01 {
                        log::debug!("Setting focuser value after backlash: {}", target_pos);
                        self.indi.focuser_set_abs_value(&self.f_options.device, target_pos, true, None)?;
                        self.state = FocusingState::WaitingPosition(target_pos);
                    }
                }
                FocusingState::WaitingPosition(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        log::debug!("Taking shot for focuser value: {}", desired_focus);
                        init_cam_continuous_mode(&self.indi, &self.camera, &self.cam_opts.frame, false)?;
                        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
                        self.state = FocusingState::WaitingFrame(desired_focus);
                    }
                }
                FocusingState::WaitingResultPosAntiBacklash { anti_backlash_pos, target_pos } => {
                    log::info!("cur_focus = {}, anti_backlash_pos = {}, target_pos = {}", cur_focus, anti_backlash_pos, target_pos);
                    if f64::abs(cur_focus-anti_backlash_pos) < 1.01 {
                        log::debug!("Setting RESULT focuser value after backlash: {}", target_pos);
                        self.indi.focuser_set_abs_value(&self.f_options.device, target_pos, true, None)?;
                        self.state = FocusingState::WaitingResultPos(target_pos);
                    }
                }
                FocusingState::WaitingResultPos(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        log::debug!("Taking RESULT shot for focuser value: {}", desired_focus);
                        init_cam_continuous_mode(&self.indi, &self.camera, &self.cam_opts.frame, false)?;
                        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
                        self.state = FocusingState::WaitingResultImg;
                    }
                }
                _ => {}
            }
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        match &fp_result.data {
            FrameProcessResultData::LightFrameInfo(info) =>
                self.process_light_frame_info(info),

            _ =>
                Ok(NotifyResult::Empty)
        }
    }
}
