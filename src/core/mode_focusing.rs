use std::{
    sync::{atomic::AtomicBool, Mutex, Arc, RwLock},
    collections::VecDeque
};
use crate::{
    indi,
    options::*,
    utils::math::*,
    image::info::LightFrameInfo,
};
use super::{core::*, frame_processing::*};

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

#[derive(PartialEq)]
enum FocusingStage {
    Undef,
    Preliminary,
    Final
}

pub struct FocusingMode {
    indi:               Arc<indi::Connection>,
    state:              FocusingState,
    camera:             DeviceAndProp,
    options:            FocuserOptions,
    frame:              FrameOptions,
    before_pos:         f64,
    to_go:              VecDeque<f64>,
    samples:            Vec<FocuserSample>,
    result_pos:         Option<f64>,
    try_cnt:            usize,
    stage:              FocusingStage,
    next_mode:          Option<Box<dyn Mode + Sync + Send>>,
    img_proc_stop_flag: Arc<Mutex<Arc<AtomicBool>>>,
}

#[derive(PartialEq)]
enum FocusingState {
    Undefined,
    WaitingPositionAntiBacklash{
        before_pos: f64,
        begin_pos: f64
    },
    WaitingPosition(f64),
    WaitingFrame(f64),
    WaitingResultPosAntiBacklash{
        before_pos: f64,
        begin_pos: f64
    },
    WaitingResultPos(f64),
    WaitingResultImg,
}

#[derive(Clone)]
pub struct FocuserSample {
    pub focus_pos:     f64,
    pub stars_fwhm:    f32,
    pub stars_ovality: f32,
}

impl FocusingMode {
    pub fn new(
        indi:               &Arc<indi::Connection>,
        options:            &Arc<RwLock<Options>>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
        next_mode:          Option<Box<dyn Mode + Sync + Send>>,
    ) -> anyhow::Result<Self> {
        let options = options.read().unwrap();
        let Some(cam_device) = &options.cam.device else {
            anyhow::bail!("Camera is not selected");
        };
        let mut frame = options.cam.frame.clone();
        frame.exp_main = options.focuser.exposure;
        frame.gain = options.focuser.gain;
        Ok(FocusingMode {
            indi:               Arc::clone(indi),
            state:              FocusingState::Undefined,
            options:            options.focuser.clone(),
            frame,
            before_pos:         0.0,
            to_go:              VecDeque::new(),
            samples:            Vec::new(),
            result_pos:         None,
            stage:              FocusingStage::Undef,
            try_cnt:            0,
            next_mode,
            camera:             cam_device.clone(),
            img_proc_stop_flag: Arc::clone(img_proc_stop_flag),
        })
    }

    fn start_stage(
        &mut self,
        middle_pos: f64,
        stage:      FocusingStage
    ) -> anyhow::Result<()> {
        self.samples.clear();
        self.to_go.clear();
        for step in 0..self.options.measures {
            let step = step as f64;
            let half_progress = (self.options.measures as f64 - 1.0) / 2.0;
            let pos_to_go = middle_pos + self.options.step * (step - half_progress);
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
            self.indi.focuser_set_abs_value(&self.options.device, pos, true, None)?;
            self.state = FocusingState::WaitingPosition(pos);
        } else {
            let mut before_pos = pos - self.options.step;
            let cur_pos = self.indi.focuser_get_abs_value(&self.options.device)?;
            if f64::abs(before_pos - cur_pos) < 1.0 {
                before_pos -= 1.0;
            }
            self.indi.focuser_set_abs_value(&self.options.device, before_pos, true, None)?;
            self.state = FocusingState::WaitingPositionAntiBacklash{
                before_pos,
                begin_pos: pos
            };
        }
        Ok(())
    }

    fn process_light_frame_info(
        &mut self,
        info:        &LightFrameInfo,
        subscribers: &Arc<RwLock<Subscribers>>,
    ) -> anyhow::Result<NotifyResult> {
        let subscribers = subscribers.read().unwrap();
        let mut result = NotifyResult::Empty;
        if let FocusingState::WaitingFrame(focus_pos) = self.state {
            let mut ok = false;
            if let (Some(stars_ovality), Some(stars_fwhm))
            = (info.stars.ovality, info.stars.fwhm) {
                self.try_cnt = 0;
                if stars_ovality < MAX_FOCUS_STAR_OVALITY {
                    let sample = FocuserSample {
                        focus_pos,
                        stars_fwhm,
                        stars_ovality
                    };
                    self.samples.push(sample);
                    self.samples.sort_by(|s1, s2| cmp_f64(&s1.focus_pos, &s2.focus_pos));
                    ok = true;
                    self.try_cnt = 0;
                }
                subscribers.inform_focusing(FocusingStateEvent::Data( FocusingResultData {
                    samples: self.samples.clone(),
                    coeffs: None,
                    result: None,
                }));
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
                    let mut x = Vec::new();
                    let mut y = Vec::new();
                    for sample in &self.samples {
                        x.push(sample.focus_pos);
                        y.push(sample.stars_fwhm as f64);
                    }
                    let coeffs = square_ls(&x, &y)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus function"))?;

                    if coeffs.a2 <= 0.0 {
                        subscribers.inform_focusing(FocusingStateEvent::Data( FocusingResultData {
                            samples: self.samples.clone(),
                            coeffs: Some(coeffs.clone()),
                            result: None,
                        }));
                        anyhow::bail!("Wrong focuser curve result");
                    }
                    let extr = parabola_extremum(&coeffs)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus extremum"))?;
                    subscribers.inform_focusing(FocusingStateEvent::Data( FocusingResultData {
                        samples: self.samples.clone(),
                        coeffs: Some(coeffs.clone()),
                        result: Some(extr),
                    }));
                    let focuser_info = self.indi.focuser_get_abs_value_prop_info(&self.options.device)?;
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
                        self.to_go.clear();
                        if extr < min_acceptable {
                            for i in (1..(self.options.measures+1)/2).rev() {
                                self.to_go.push_back(min_sample_pos - i as f64 * self.options.step);
                            }
                        } else {
                            for i in 1..(self.options.measures+1)/2 {
                                self.to_go.push_back(max_sample_pos + i as f64 * self.options.step);
                            }
                        }
                        self.start_sample(true)?;
                        return Ok(result);
                    }
                    if self.stage == FocusingStage::Preliminary {
                        self.start_stage(extr, FocusingStage::Final)?;
                        result = NotifyResult::ModeChanged;
                        return Ok(result)
                    }

                    self.result_pos = Some(extr);
                    // for anti-backlash first move to minimum position
                    self.indi.focuser_set_abs_value(
                        &self.options.device,
                        extr - self.options.step,
                        true,
                        None
                    )?;
                    self.state = FocusingState::WaitingResultPosAntiBacklash {
                        before_pos: extr - self.options.step,
                        begin_pos: extr
                    };
                    subscribers.inform_focusing(FocusingStateEvent::Result {
                        value: extr
                    });
                } else {
                    self.start_sample(false)?;
                }
            } else {
                init_cam_continuous_mode(&self.indi, &self.camera, &self.frame, false)?;
                apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.frame, &self.img_proc_stop_flag)?;
            }
        }
        if self.state == FocusingState::WaitingResultImg {
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

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.samples.len(),
            total: self.samples.len() + self.to_go.len() + 1
        })
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.frame.exposure())
    }

    fn can_be_continued_after_stop(&self) -> bool {
        false
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let cur_pos = self.indi.focuser_get_abs_value(&self.options.device)?.round();
        self.before_pos = cur_pos;
        self.start_stage(cur_pos, FocusingStage::Preliminary)?;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.camera, &self.img_proc_stop_flag)?;
        self.indi.focuser_set_abs_value(&self.options.device, self.before_pos, true, None)?;
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
        if *prop_change.device_name != self.options.device {
            return Ok(NotifyResult::Empty);
        }
        if let ("ABS_FOCUS_POSITION", indi::PropChange::Change { value, .. })
        = (prop_change.prop_name.as_str(), &prop_change.change) {
            let cur_focus = value.prop_value.to_f64()?;
            match self.state {
                FocusingState::WaitingPositionAntiBacklash {before_pos, begin_pos} => {
                    if f64::abs(cur_focus-before_pos) < 1.01 {
                        self.indi.focuser_set_abs_value(&self.options.device, begin_pos, true, None)?;
                        self.state = FocusingState::WaitingPosition(begin_pos);
                    }
                }
                FocusingState::WaitingPosition(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        init_cam_continuous_mode(&self.indi, &self.camera, &self.frame, false)?;
                        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.frame, &self.img_proc_stop_flag)?;
                        self.state = FocusingState::WaitingFrame(desired_focus);
                    }
                }
                FocusingState::WaitingResultPosAntiBacklash { before_pos, begin_pos } => {
                    if f64::abs(cur_focus-before_pos) < 1.01 {
                        self.indi.focuser_set_abs_value(&self.options.device, begin_pos, true, None)?;
                        self.state = FocusingState::WaitingResultPos(begin_pos);
                    }
                }
                FocusingState::WaitingResultPos(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        init_cam_continuous_mode(&self.indi, &self.camera, &self.frame, false)?;
                        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.frame, &self.img_proc_stop_flag)?;
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
        fp_result: &FrameProcessResult,
        subscribers: &Arc<RwLock<Subscribers>>
    ) -> anyhow::Result<NotifyResult> {
        match &fp_result.data {
            FrameProcessResultData::LightFrameInfo(info) =>
                self.process_light_frame_info(info, subscribers),

            _ =>
                Ok(NotifyResult::Empty)
        }
    }
}
