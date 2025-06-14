use std::{
    sync::{Arc, RwLock},
    collections::VecDeque
};
use itertools::{izip, Itertools};

use crate::{
    indi,
    options::*,
    utils::math::*
};
use super::{core::*, events::*, frame_processing::*, utils::*};

const MAX_FOCUS_TOTAL_TRY_CNT: usize = 8;
const MAX_FOCUS_SAMPLE_TRY_CNT: usize = 4;
const MAX_FOCUS_CHANGE_TIME: usize = 15;
const MAX_FOCUS_TRY_SET_CNT: usize = 2;

pub enum FocusingErrorReaction {
    Fail,
    IgnoreAndExit,
}

#[derive(Clone)]
pub struct FocusingResultData {
    pub samples: Vec<FocuserSample>,
    pub coeffs:  Option<QuadraticCoeffs>,
    pub result:  Option<f64>,
}

#[derive(Clone)]
pub enum FocuserEvent {
    StartingTemperature(f64),
    Data(FocusingResultData),
    Result { value: f64 }
}

#[derive(PartialEq, Debug)]
enum Stage {
    Undef,
    Preliminary,
    Final
}

pub struct FocusingMode {
    indi:           Arc<indi::Connection>,
    subscribers:    Arc<EventSubscriptions>,
    state:          FocusingState,
    camera:         DeviceAndProp,
    f_opts:         FocuserOptions,
    cam_opts:       CamOptions,
    before_pos:     f64,
    to_go:          VecDeque<f64>,
    samples:        Vec<FocuserSample>,
    one_pos_hfd:    Vec<f32>,
    result_pos:     Option<f64>,
    max_try:        usize,
    try_cnt:        usize,
    prelim_step:    bool,
    stage:          Stage,
    desired_focus:  f64,
    change_time:    Option<usize>,
    change_cnt:     usize,
    next_mode:      Option<Box<dyn Mode + Sync + Send>>,
    start_temp:     f64, // temperature when autofocusing is started
    error_reaction: FocusingErrorReaction,
}

#[derive(PartialEq)]
enum FocusingState {
    Undefined,
    WaitingFirstImage,
    WaitingPositionAntiBacklash{
        anti_backlash_pos: f64,
        target_pos: f64
    },
    WaitingPosition(f64),
    WaitingMeasureFrame(f64),
    WaitingResultPosAntiBacklash{
        anti_backlash_pos: f64,
        target_pos: f64
    },
    WaitingResultPos(f64),
    WaitingResultImg(f64),
}

#[derive(Clone)]
pub struct FocuserSample {
    pub position: f64,
    pub hfd:      f32,
}

#[derive(Debug, Clone)]
enum CalcResult {
    Value {
        value:  f64,
        coeffs: QuadraticCoeffs,
    },
    Rising(QuadraticCoeffs),
    Falling(QuadraticCoeffs),
}

impl FocusingMode {
    pub fn new(
        indi:           &Arc<indi::Connection>,
        options:        &Arc<RwLock<Options>>,
        subscribers:    &Arc<EventSubscriptions>,
        next_mode:      Option<Box<dyn Mode + Sync + Send>>,
        prelim_step:    bool,
        error_reaction: FocusingErrorReaction,
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

        let exposure_u = (cam_opts.frame.exposure() as usize).max(1);
        let max_try = (10 / exposure_u).max(1).min(3);

        log::debug!("Creating autofocus mode. max_try={}", max_try);

        Ok(FocusingMode {
            indi:          Arc::clone(indi),
            subscribers:   Arc::clone(subscribers),
            state:         FocusingState::Undefined,
            f_opts:        opts.focuser.clone(),
            before_pos:    0.0,
            to_go:         VecDeque::new(),
            samples:       Vec::new(),
            one_pos_hfd:   Vec::new(),
            result_pos:    None,
            stage:         Stage::Undef,
            change_time:   None,
            change_cnt:    0,
            desired_focus: 0.0,
            try_cnt:       0,
            camera:        cam_device.clone(),
            start_temp:    0.0,
            prelim_step,
            next_mode,
            cam_opts,
            max_try,
            error_reaction,
        })
    }

    fn start_stage(
        &mut self,
        middle_pos: f64,
        stage:      Stage
    ) -> anyhow::Result<()> {
        log::info!("Start autofocus stage {:?} for central focuser value {}", stage, middle_pos);
        self.samples.clear();
        self.to_go.clear();
        for step in 0..self.f_opts.measures {
            let step = step as f64;
            let half_progress = (self.f_opts.measures as f64 - 1.0) / 2.0;
            let pos_to_go = middle_pos + self.f_opts.step * (step - half_progress);
            self.to_go.push_back(pos_to_go);
        }
        self.stage = stage;
        self.start_sample(true)?;
        Ok(())
    }

    fn set_new_focus_value(&mut self, value: f64) -> anyhow::Result<()> {
        self.indi.focuser_set_abs_value(&self.f_opts.device, value, true, None)?;
        self.desired_focus = value;
        self.change_time = Some(0);
        self.change_cnt = 0;
        Ok(())
    }

    fn start_sample(
        &mut self,
        anti_backlash: bool
    ) -> anyhow::Result<()> {
        let Some(pos) = self.to_go.pop_front() else {
            return Ok(());
        };
        log::debug!("Setting focuser value={:.1}, anti_backlash={}", pos, anti_backlash);
        if anti_backlash {
            let anti_backlash_pos = pos - self.f_opts.anti_backlash_steps as f64;
            let anti_backlash_pos = anti_backlash_pos.max(0.0);

            self.set_new_focus_value(anti_backlash_pos)?;
            self.state = FocusingState::WaitingPositionAntiBacklash{
                anti_backlash_pos,
                target_pos: pos
            };
        } else {
            self.set_new_focus_value(pos)?;
            self.state = FocusingState::WaitingPosition(pos);
        }
        Ok(())
    }

    fn check_cur_focus_value(&mut self, cur_focus: f64) -> anyhow::Result<NotifyResult> {
        match self.state {
            FocusingState::WaitingPositionAntiBacklash { anti_backlash_pos, target_pos } => {
                if cur_focus as i64 == anti_backlash_pos as i64 {
                    log::debug!("Setting focuser value after anti backlash move: {}", target_pos);
                    self.set_new_focus_value(target_pos)?;
                    self.state = FocusingState::WaitingPosition(target_pos);
                }
            }
            FocusingState::WaitingPosition(desired_focus) => {
                if cur_focus as i64 == desired_focus as i64 {
                    log::debug!("Taking picture for focuser value: {}", desired_focus);
                    self.change_time = None;
                    apply_camera_options_and_take_shot(
                        &self.indi,
                        &self.camera,
                        &self.cam_opts.frame,
                        &self.cam_opts.ctrl
                    )?;
                    self.state = FocusingState::WaitingMeasureFrame(desired_focus);
                }
            }
            FocusingState::WaitingResultPosAntiBacklash { anti_backlash_pos, target_pos } => {
                if cur_focus as i64 == anti_backlash_pos as i64 {
                    log::debug!("Setting RESULT focuser value {} after backlash correction", target_pos);
                    self.set_new_focus_value(target_pos)?;
                    self.state = FocusingState::WaitingResultPos(target_pos);
                }
            }
            FocusingState::WaitingResultPos(desired_focus) => {
                if cur_focus as i64 == desired_focus as i64 {
                    log::debug!("Taking RESULT shot for focuser value: {}", desired_focus);
                    self.change_time = None;
                    apply_camera_options_and_take_shot(
                        &self.indi,
                        &self.camera,
                        &self.cam_opts.frame,
                        &self.cam_opts.ctrl
                    )?;
                    self.state = FocusingState::WaitingResultImg(desired_focus);
                    return Ok(NotifyResult::ProgressChanges);
                }
            }
            _ => {}
        }
        Ok(NotifyResult::Empty)
    }

    fn process_img_info_when_waiting_first_img(
        &mut self,
        info: &LightFrameInfoData
    ) -> anyhow::Result<NotifyResult> {
        log::info!(
            "First image before autofocus. FWHM={:.2?}, ovality={:.2?}, initial focus={:.0}",
            info.stars.info.fwhm, info.stars.info.ovality, self.before_pos
        );
        self.start_stage(
            self.before_pos,
            if self.prelim_step { Stage::Preliminary } else { Stage::Final }
        )?;
        Ok(NotifyResult::ProgressChanges)
    }

    fn process_img_info_when_waiting_measure(
        &mut self,
        info:      &LightFrameInfoData,
        focus_pos: f64,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;

        log::debug!(
            "New frame with FWHM={:?}, HFD={:?}, ovality={:?}",
            info.stars.info.fwhm, info.stars.info.hfd, info.stars.info.ovality
        );

        let samples_count_before = self.samples.len();
        let info_is_ok = info.stars.info.hfd.is_some();
        if let Some(stars_hfd) = info.stars.info.hfd {
            self.try_cnt = 0;
            let add_res = self.add_measure(stars_hfd, focus_pos)?;
            if !matches!(add_res, NotifyResult::Empty) {
                return Ok(add_res);
            }
        } else {
            self.try_cnt += 1;
        }

        let sample_added = samples_count_before != self.samples.len();

        let too_much_total_tries =
            self.try_cnt >= MAX_FOCUS_TOTAL_TRY_CNT &&
            !self.samples.is_empty();

        if sample_added
        || self.try_cnt >= MAX_FOCUS_SAMPLE_TRY_CNT
        || too_much_total_tries {
            result = NotifyResult::ProgressChanges;
            if self.to_go.is_empty() || too_much_total_tries {
                self.to_go.clear();
                log::debug!(
                    "Trying to calculate focuser position. \
                    result_added={}, self.try_cnt={}, self.samples.len={}",
                    sample_added, self.try_cnt, self.samples.len(),
                );
                self.calculate_and_process_result()?;
            } else {
                self.start_sample(false)?;
            }
        } else if !info_is_ok {
            result = NotifyResult::ProgressChanges;
            log::info!("Stars on received image are not Ok. Taking another image...");
            self.change_time = None;
            apply_camera_options_and_take_shot(
                &self.indi,
                &self.camera,
                &self.cam_opts.frame,
                &self.cam_opts.ctrl
            )?;
        }
        Ok(result)
    }

    fn add_measure(&mut self, stars_hfd: f32, focus_pos: f64) -> anyhow::Result<NotifyResult> {
        self.one_pos_hfd.push(stars_hfd);

        log::debug!(
            "Added new HFD into one pos values (len={}/{})",
            self.one_pos_hfd.len(), self.max_try
        );

        if self.one_pos_hfd.len() == self.max_try {
            let stars_hfd = self.one_pos_hfd
                .iter()
                .copied()
                .min_by(f32::total_cmp)
                .unwrap_or_default();
            log::info!(
                "Best HFD={:.2} of {:.2?} at {:.0} pos",
                stars_hfd, self.one_pos_hfd, focus_pos
            );
            let sample = FocuserSample {
                position: focus_pos,
                hfd: stars_hfd
            };
            self.samples.push(sample);
            self.samples.sort_by(|s1, s2| cmp_f64(&s1.position, &s2.position));
            self.one_pos_hfd.clear();

            log::debug!("Samples count = {}", self.samples.len());
            let event_data = FocusingResultData {
                samples: self.samples.clone(),
                coeffs: None,
                result: None,
            };
            self.subscribers.notify(Event::Focusing(
                FocuserEvent::Data(event_data)
            ));
        } else {
            apply_camera_options_and_take_shot(
                &self.indi,
                &self.camera,
                &self.cam_opts.frame,
                &self.cam_opts.ctrl
            )?;
            return Ok(NotifyResult::ProgressChanges);
        }

        Ok(NotifyResult::Empty)
    }

    fn calculate_and_process_result(&mut self) -> anyhow::Result<()> {
        let calc_result = self.calc_result(self.stage == Stage::Preliminary);
        log::debug!("Autofocus result = {:?}", calc_result);

        match calc_result {
            Ok(CalcResult::Value { value: result_pos, coeffs }) => {
                let event_data = FocusingResultData {
                    samples: self.samples.clone(),
                    coeffs: Some(coeffs.clone()), // TODO: remove clone
                    result: Some(result_pos),
                };
                self.subscribers.notify(Event::Focusing(
                    FocuserEvent::Data(event_data)
                ));
                if self.stage == Stage::Preliminary {
                    self.start_stage(result_pos, Stage::Final)?;
                    return Ok(())
                }

                self.result_pos = Some(result_pos);

                // for anti-backlash
                let anti_backlash_pos = result_pos - self.f_opts.anti_backlash_steps as f64;
                let anti_backlash_pos = anti_backlash_pos.max(0.0).round();
                log::debug!(
                    "Set RESULT focuser value (anti backlash pos={:.1}, pos={:.1})",
                    anti_backlash_pos, result_pos
                );
                self.set_new_focus_value(anti_backlash_pos)?;
                self.state = FocusingState::WaitingResultPosAntiBacklash {
                    anti_backlash_pos,
                    target_pos: result_pos
                };
                let result_event = FocuserEvent::Result { value: result_pos };
                self.subscribers.notify(Event::Focusing(result_event));
            },
            Ok(CalcResult::Rising(coeffs)) => {
                log::info!("Results too far from center. Do more measures from left");
                let event_data = FocusingResultData {
                    samples: self.samples.clone(),
                    coeffs: Some(coeffs.clone()),
                    result: None,
                };
                self.subscribers.notify(Event::Focusing(
                    FocuserEvent::Data(event_data)
                ));
                let min_sample_pos = self.samples
                    .iter()
                    .map(|v|v.position)
                    .min_by(cmp_f64)
                    .unwrap_or_default();
                for i in (1..(self.f_opts.measures+1)/2).rev() {
                    self.to_go.push_back(min_sample_pos - i as f64 * self.f_opts.step);
                }
                self.start_sample(true)?;
            },
            Ok(CalcResult::Falling(coeffs)) => {
                let event_data = FocusingResultData {
                    samples: self.samples.clone(),
                    coeffs: Some(coeffs.clone()),
                    result: None,
                };
                self.subscribers.notify(Event::Focusing(
                    FocuserEvent::Data(event_data)
                ));
                log::info!("Results too far from center. Do more measures from right");
                let max_sample_pos = self.samples
                    .iter()
                    .map(|v|v.position)
                    .max_by(cmp_f64)
                    .unwrap_or_default();
                for i in 1..(self.f_opts.measures+1)/2 {
                    self.to_go.push_back(max_sample_pos + i as f64 * self.f_opts.step);
                }
                self.start_sample(true)?;
            },

            Err(error) => {
                log::error!(
                    "Position calculation failed with errror {}",
                    error.to_string()
                );

                match self.error_reaction {
                    // restore previous focuser position
                    FocusingErrorReaction::IgnoreAndExit => {
                        let anti_backlash_pos = self.before_pos - self.f_opts.anti_backlash_steps as f64;
                        let anti_backlash_pos = anti_backlash_pos.max(0.0).round();
                        log::debug!(
                            "Set PREVIOUS focuser value (anti backlash pos={:.1}, pos={:.1})",
                            anti_backlash_pos, self.before_pos
                        );
                        self.set_new_focus_value(anti_backlash_pos)?;
                        self.state = FocusingState::WaitingResultPosAntiBacklash {
                            anti_backlash_pos,
                            target_pos: self.before_pos
                        };
                    }
                    FocusingErrorReaction::Fail => {
                        // fail with error
                        anyhow::bail!(error);
                    }
                }
            }
        }

        Ok(())

    }

    fn process_img_info_when_waiting_result_img(
        &mut self,
        info:      &LightFrameInfoData,
        focus_pos: f64
    ) -> anyhow::Result<NotifyResult> {
        log::info!(
            "RESULT focuser shot is finished. \
            Final FWHM = {:.2?}, ovality={:.2?}, focuser change={:.0} -> {:.0}",
            info.stars.info.fwhm, info.stars.info.ovality, self.before_pos, focus_pos
        );

        if !self.start_temp.is_nan() {
            self.subscribers.notify(
                Event::Focusing(FocuserEvent::StartingTemperature(self.start_temp))
            );
        }

        Ok(NotifyResult::Finished {
            next_mode: self.next_mode.take()
        })
    }

    fn calc_result(&self, allow_more_measures: bool) -> anyhow::Result<CalcResult> {
        if self.samples.is_empty() {
            anyhow::bail!("No samples for position calculation!");
        }
        let coeffs = Self::calc_quadratic_coeffs(&self.samples, 2);
        log::debug!("coeffs = {:?}", coeffs);

        if let Some(coeffs) = coeffs {
            if coeffs.a2 > 0.0 {
                if let Some(value) = parabola_extremum(&coeffs) {
                    let value = value.round();
                    log::debug!("Extremum = {:.1}", value);
                    if !allow_more_measures {
                        let prop_elem = self.indi.focuser_get_abs_value_prop_elem(&self.f_opts.device)?;
                        if value < prop_elem.min || value > prop_elem.max {
                            anyhow::bail!(
                                "Result pos {0:.1} out of focuser range ({1:.1}..{2:.1})",
                                value, prop_elem.min, prop_elem.max
                            );
                        }
                        return Ok(CalcResult::Value { value, coeffs });
                    }
                    let min_sample_pos = self.samples
                        .iter()
                        .map(|v|v.position)
                        .min_by(cmp_f64)
                        .unwrap_or_default();
                    let max_sample_pos = self.samples
                        .iter()
                        .map(|v|v.position)
                        .max_by(cmp_f64)
                        .unwrap_or_default();
                    let min_acceptable = min_sample_pos + (max_sample_pos-min_sample_pos) * 0.20;
                    let max_acceptable = min_sample_pos + (max_sample_pos-min_sample_pos) * 0.80;
                    log::debug!("Min/Max acceptable focus extremums = {:.1}/{:.1}", min_acceptable, max_acceptable);
                    if min_acceptable <= value && value <= max_acceptable {
                        return Ok(CalcResult::Value { value, coeffs });
                    }
                }
            }
        }

        if allow_more_measures {
            let (x, y) = Self::samples_to_x_y(&self.samples, None);
            let linear_coeffs = linear_regression(&x, &y)
                .ok_or_else(|| anyhow::anyhow!("Can't find focus linear coefficients"))?;
            let (a, b) = linear_coeffs;
            if a > 0.0 {
                return Ok(CalcResult::Rising(QuadraticCoeffs { a2: 0.0, a1: a, a0: b }));
            } else if a < 0.0 {
                return Ok(CalcResult::Falling(QuadraticCoeffs { a2: 0.0, a1: a, a0: b }));
            }
        }

        anyhow::bail!("Can't calculate focuser result");
    }

    fn calc_quadratic_coeffs(
        samples0: &[FocuserSample],
        max_pt_to_skip: usize
    ) -> Option<QuadraticCoeffs> {
        let orig_len = samples0.len();
        let mut samples = Vec::from(samples0);

        let (mut coeff, mut err) = Self::calc_quadratic_coeffs_and_err(&samples, None)?;
        loop {
            let len = samples.len();
            if len <= 2 || orig_len-len >= max_pt_to_skip {
                return Some(coeff);
            }

            let mut best_index = None;
            for idx_to_skip in [0, len-1] {
                if let Some((new_coeff, new_err))
                = Self::calc_quadratic_coeffs_and_err(&samples, Some(idx_to_skip)) {
                    if new_err < err {
                        err = new_err;
                        coeff = new_coeff;
                        best_index = Some(idx_to_skip);
                        continue;
                    }
                }
            }

            if let Some(best_index) = best_index {
                log::debug!("Skip index {}", best_index);
                samples.remove(best_index);
            } else {
                return Some(coeff);
            }
        }
    }

    fn samples_to_x_y(
        samples:       &[FocuserSample],
        index_to_skip: Option<usize>,
    ) -> (Vec<f64>, Vec<f64>) {
        let x = samples.iter()
            .enumerate()
            .filter(|(idx, _)| Some(*idx) != index_to_skip)
            .map(|(_, v)| v.position)
            .collect_vec();
        let y = samples.iter()
            .enumerate()
            .filter(|(idx, _)| Some(*idx) != index_to_skip)
            .map(|(_, v)| v.hfd as f64)
            .collect_vec();
        (x, y)
    }

    fn calc_quadratic_coeffs_and_err(
        samples:       &[FocuserSample],
        index_to_skip: Option<usize>,
    ) -> Option<(QuadraticCoeffs, f64)> {
        let (x, y) = Self::samples_to_x_y(samples, index_to_skip);
        let coeffs = square_ls(&x, &y)?;
        let sum = izip!(&x, &y)
            .map(|(x, y)| {
                let yc = coeffs.calc(*x);
                (yc - y) * (yc - y)
            })
            .sum::<f64>();

        let err = f64::sqrt(sum / (x.len() as f64));
        Some((coeffs, err))
    }

}

impl Mode for FocusingMode {
    fn get_type(&self) -> ModeType {
        ModeType::Focusing
    }

    fn progress_string(&self) -> String {
        match self.stage {
            Stage::Undef =>
                "Preparing for autofocus".to_string(),
            Stage::Preliminary =>
                "Focusing (preliminary)".to_string(),
            Stage::Final =>
                "Focusing".to_string(),
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn progress(&self) -> Option<Progress> {
        let total = self.samples.len() + self.to_go.len() + 1;
        let mut cur = self.samples.len();
        if matches!(self.state, FocusingState::WaitingResultImg(_)) {
            cur = total;
        }
        Some(Progress { cur, total })
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exposure())
    }

    fn can_be_continued_after_stop(&self) -> bool {
        false
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let cur_pos = self.indi
            .focuser_get_abs_value_prop_elem(&self.f_opts.device)?.value
            .round();

        self.start_temp = self.indi
            .focuser_get_temperature(&self.f_opts.device)
            .unwrap_or(f64::NAN);

        self.before_pos = cur_pos;

        apply_camera_options_and_take_shot(
            &self.indi,
            &self.camera,
            &self.cam_opts.frame,
            &self.cam_opts.ctrl
        )?;
        self.stage = Stage::Undef;
        self.state = FocusingState::WaitingFirstImage;

        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.camera)?;
        self.indi.focuser_set_abs_value(&self.f_opts.device, self.before_pos, true, None)?;
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
        if *prop_change.device_name != self.f_opts.device {
            return Ok(NotifyResult::Empty);
        }
        if let ("ABS_FOCUS_POSITION", indi::PropChange::Change { value, .. })
        = (prop_change.prop_name.as_str(), &prop_change.change) {
            let cur_focus = value.prop_value.to_f64()?;
            return self.check_cur_focus_value(cur_focus);
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        if let Some(change_time) = &mut self.change_time {
            *change_time += 1;
            if *change_time > MAX_FOCUS_CHANGE_TIME {
                self.change_cnt += 1;
                log::error!("Time out waiting new focus value!");
                if self.change_cnt > MAX_FOCUS_TRY_SET_CNT {
                    anyhow::bail!("Can't set new focus value for focuser!");
                }
                log::error!("Setting new value {:.0} again. Try = {}", self.desired_focus, self.change_cnt);
                self.indi.focuser_set_abs_value(&self.f_opts.device, self.desired_focus, true, None)?;
                *change_time = 0;
            }
        }
        let cur_focus = self.indi.focuser_get_abs_value_prop_elem(&self.f_opts.device)?.value;
        self.check_cur_focus_value(cur_focus)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        match &fp_result.data {
            FrameProcessResultData::LightFrameInfo(info) =>
                match self.state {
                    FocusingState::WaitingFirstImage =>
                        return self.process_img_info_when_waiting_first_img(info),
                    FocusingState::WaitingMeasureFrame(focuser_pos) =>
                        return self.process_img_info_when_waiting_measure(info, focuser_pos),
                    FocusingState::WaitingResultImg(focuser_pos) =>
                        return self.process_img_info_when_waiting_result_img(info, focuser_pos),
                    _ =>
                        unreachable!("Wrong FocusingMode::state in notify_about_frame_processing_result"),
                }
            _ => {}
        };
        return Ok(NotifyResult::Empty)
    }
}
