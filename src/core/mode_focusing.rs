use std::{
    sync::{Arc, RwLock},
    collections::VecDeque
};
use crate::{
    indi,
    options::*,
    utils::math::*
};
use super::{core::*, events::*, frame_processing::*, utils::*};

const ONE_POS_TRY_CNT: usize = 3;
const MAX_FOCUS_TOTAL_TRY_CNT: usize = 8;
const MAX_FOCUS_SAMPLE_TRY_CNT: usize = 4;
const BACKLASH_STEPS: f64 = 4.0;

const MAX_FOCUS_CHANGE_TIME: usize = 15;
const MAX_FOCUS_CHANGE_CNT: usize = 3;

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
enum Stage {
    Undef,
    Preliminary,
    Final
}

pub struct FocusingMode {
    indi:          Arc<indi::Connection>,
    subscribers:   Arc<EventSubscriptions>,
    state:         FocusingState,
    camera:        DeviceAndProp,
    f_opts:        FocuserOptions,
    cam_opts:      CamOptions,
    before_pos:    f64,
    to_go:         VecDeque<f64>,
    samples:       Vec<FocuserSample>,
    one_pos_fwhm:  Vec<f32>,
    result_pos:    Option<f64>,
    try_cnt:       usize,
    stage:         Stage,
    desired_focus: f64,
    change_time:   Option<usize>,
    change_cnt:    usize,
    next_mode:     Option<Box<dyn Mode + Sync + Send>>,
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
        subscribers: &Arc<EventSubscriptions>,
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
            indi:          Arc::clone(indi),
            subscribers:   Arc::clone(subscribers),
            state:         FocusingState::Undefined,
            f_opts:        opts.focuser.clone(),
            before_pos:    0.0,
            to_go:         VecDeque::new(),
            samples:       Vec::new(),
            one_pos_fwhm:  Vec::new(),
            result_pos:    None,
            stage:         Stage::Undef,
            change_time:   None,
            change_cnt:    0,
            desired_focus: 0.0,
            try_cnt:       0,
            camera:        cam_device.clone(),
            next_mode,
            cam_opts,
        })
    }

    fn start_stage(
        &mut self,
        middle_pos: f64,
        stage:      Stage
    ) -> anyhow::Result<()> {
        log::debug!("Starting autofocus stage {:?} for midle value {}", stage, middle_pos);
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
        first_time: bool
    ) -> anyhow::Result<()> {
        let Some(pos) = self.to_go.pop_front() else {
            return Ok(());
        };

        if !first_time {
            log::debug!("Setting focuser value: {}", pos);
            self.set_new_focus_value(pos)?;
            self.state = FocusingState::WaitingPosition(pos);
        } else {
            let anti_backlash_pos = pos - BACKLASH_STEPS * self.f_opts.step;
            let anti_backlash_pos = anti_backlash_pos.max(0.0);
            log::debug!("Setting focuser value for avoiding backlash: {}", pos);
            self.set_new_focus_value(anti_backlash_pos)?;
            self.state = FocusingState::WaitingPositionAntiBacklash{
                anti_backlash_pos,
                target_pos: pos
            };
        }
        Ok(())
    }

    fn check_cur_focus_value(&mut self, cur_focus: f64) -> anyhow::Result<NotifyResult> {
        match self.state {
            FocusingState::WaitingPositionAntiBacklash { anti_backlash_pos, target_pos } => {
                if cur_focus as i64 == anti_backlash_pos as i64 {
                    log::debug!("Setting focuser value after backlash: {}", target_pos);
                    self.set_new_focus_value(target_pos)?;
                    self.state = FocusingState::WaitingPosition(target_pos);
                }
            }
            FocusingState::WaitingPosition(desired_focus) => {
                if cur_focus as i64 == desired_focus as i64 {
                    log::debug!("Taking shot for focuser value: {}", desired_focus);
                    self.change_time = None;
                    apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
                    self.state = FocusingState::WaitingFrame(desired_focus);
                }
            }
            FocusingState::WaitingResultPosAntiBacklash { anti_backlash_pos, target_pos } => {
                log::info!(
                    "cur_focus = {}, anti_backlash_pos = {}, target_pos = {}",
                    cur_focus, anti_backlash_pos, target_pos
                );
                if cur_focus as i64 == anti_backlash_pos as i64 {
                    log::debug!("Setting RESULT focuser value after backlash: {}", target_pos);
                    self.set_new_focus_value(target_pos)?;
                    self.state = FocusingState::WaitingResultPos(target_pos);
                }
            }
            FocusingState::WaitingResultPos(desired_focus) => {
                if cur_focus as i64 == desired_focus as i64 {
                    log::debug!("Taking RESULT shot for focuser value: {}", desired_focus);
                    self.change_time = None;
                    apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
                    self.state = FocusingState::WaitingResultImg;
                    return Ok(NotifyResult::ProgressChanges);
                }
            }
            _ => {}
        }
        Ok(NotifyResult::Empty)
    }

    fn process_light_frame_info(
        &mut self,
        info: &LightFrameInfoData,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if let FocusingState::WaitingFrame(focus_pos) = self.state {
            log::debug!(
                "New frame with ovality={:?} and fwhm={:?}",
                info.stars.info.ovality, info.stars.info.fwhm
            );
            let mut ok = false;
            if let Some(stars_fwhm) = info.stars.info.fwhm {
                self.try_cnt = 0;
                self.one_pos_fwhm.push(stars_fwhm);

                log::debug!(
                    "Added new fwhm into one pos values (len={}/{})",
                    self.one_pos_fwhm.len(), ONE_POS_TRY_CNT
                );

                if self.one_pos_fwhm.len() == ONE_POS_TRY_CNT {
                    let stars_fwhm = self.one_pos_fwhm
                        .iter()
                        .copied()
                        .min_by(f32::total_cmp)
                        .unwrap_or_default();

                    log::debug!("Best fwhm={:.1}", stars_fwhm);

                    let sample = FocuserSample {
                        focus_pos,
                        stars_fwhm
                    };
                    self.samples.push(sample);
                    self.samples.sort_by(|s1, s2| cmp_f64(&s1.focus_pos, &s2.focus_pos));
                    self.one_pos_fwhm.clear();
                    ok = true;

                    log::debug!("Samples count = {}", self.samples.len());
                    let event_data = FocusingResultData {
                        samples: self.samples.clone(),
                        coeffs: None,
                        result: None,
                    };
                    self.subscribers.notify(Event::Focusing(
                        FocusingStateEvent::Data(event_data)
                    ));
                }
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
                        "Trying to calculate extremum. Ok={}, self.try_cnt={}, self.samples.len={}",
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
                        self.subscribers.notify(Event::Focusing(
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
                    self.subscribers.notify(Event::Focusing(
                        FocusingStateEvent::Data(event_data)
                    ));
                    let focuser_info = self.indi.focuser_get_abs_value_prop_info(&self.f_opts.device)?;
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
                            for i in (1..(self.f_opts.measures+1)/2).rev() {
                                self.to_go.push_back(min_sample_pos - i as f64 * self.f_opts.step);
                            }
                        } else {
                            log::debug!("... in maximum area");
                            for i in 1..(self.f_opts.measures+1)/2 {
                                self.to_go.push_back(max_sample_pos + i as f64 * self.f_opts.step);
                            }
                        }

                        self.start_sample(true)?;
                        return Ok(result);
                    }

                    let result_pos = extr.round();

                    if self.stage == Stage::Preliminary {
                        self.start_stage(result_pos, Stage::Final)?;
                        result = NotifyResult::ProgressChanges;
                        return Ok(result)
                    }

                    self.result_pos = Some(result_pos);

                    // for anti-backlash
                    let anti_backlash_pos = result_pos - BACKLASH_STEPS * self.f_opts.step;
                    let anti_backlash_pos = anti_backlash_pos.max(0.0);
                    log::debug!(
                        "Set RESULT focuser value for anti backlash {}",
                        anti_backlash_pos
                    );
                    self.set_new_focus_value(anti_backlash_pos)?;
                    self.state = FocusingState::WaitingResultPosAntiBacklash {
                        anti_backlash_pos,
                        target_pos: result_pos
                    };
                    let result_event = FocusingStateEvent::Result { value: result_pos };
                    self.subscribers.notify(Event::Focusing(result_event));
                } else {
                    self.start_sample(false)?;
                }
            } else {
                log::debug!("Received image is not Ok. Taking another one...");
                self.change_time = None;
                apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
            }
        }
        if self.state == FocusingState::WaitingResultImg {
            log::debug!(
                "RESULT shot is finished. Exiting focusing mode. Final FWHM = {:?}",
                info.stars.info.fwhm
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
            Stage::Preliminary =>
                "Focusing (preliminary)".to_string(),
            Stage::Final =>
                "Focusing (final)".to_string(),
            _ => unreachable!(),
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn progress(&self) -> Option<Progress> {
        let total = self.samples.len() + self.to_go.len() + 1;
        let mut cur = self.samples.len();
        if matches!(self.state, FocusingState::WaitingResultImg) {
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
        let cur_pos = self.indi.focuser_get_abs_value(&self.f_opts.device)?.round();
        self.before_pos = cur_pos;
        self.start_stage(cur_pos, Stage::Preliminary)?;
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
                if self.change_cnt > MAX_FOCUS_CHANGE_CNT {
                    anyhow::bail!("Can't set new focus value for focuser!");
                }
                log::error!("Setting new value {:.0} again. Try = {}", self.desired_focus, self.change_cnt);
                self.indi.focuser_set_abs_value(&self.f_opts.device, self.desired_focus, true, None)?;
                *change_time = 0;
            }
        }
        let cur_focus = self.indi.focuser_get_abs_value(&self.f_opts.device)?;
        self.check_cur_focus_value(cur_focus)
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
