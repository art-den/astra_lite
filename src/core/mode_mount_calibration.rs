use std::{sync::{atomic::AtomicBool, Mutex, Arc, RwLock}, f64::consts::PI};
use itertools::Itertools;
use crate::{
    indi,
    options::*,
    image::stars::*,
    image::stars_offset::*,
    utils::math::*,
    image::info::*,
};
use super::{consts::INDI_SET_PROP_TIMEOUT, core::*, frame_processing::*};

pub const DITHER_CALIBR_ATTEMPTS_CNT: usize = 11;
pub const DITHER_CALIBR_SPEED: f64 = 1.0;

#[derive(Debug, Default, Clone)]
pub struct MountMoveCalibrRes {
    move_x_ra: f64,
    move_y_ra: f64,
    move_x_dec: f64,
    move_y_dec: f64,
}

impl MountMoveCalibrRes {
    pub fn is_ok(&self) -> bool {
        self.move_x_ra != 0.0 ||
        self.move_y_ra != 0.0 ||
        self.move_x_dec != 0.0 ||
        self.move_y_dec != 0.0
    }

    pub fn calc(&self, x0: f64, y0: f64) -> Option<(f64, f64)> {
        let calc_t = |x1, y1, x2, y2| -> Option<f64> {
            let divider = y2 * x1 - x2 * y1;
            if divider != 0.0 {
                Some((y2 * x0 - x2 * y0) / divider)
            } else {
                None
            }
        };
        let t_ra = calc_t(self.move_x_ra, self.move_y_ra, self.move_x_dec, self.move_y_dec)?;
        let t_dec = calc_t(self.move_x_dec, self.move_y_dec, self.move_x_ra, self.move_y_ra)?;
        Some((t_ra, t_dec))
    }
}

pub struct MountCalibrMode {
    indi:               Arc<indi::Connection>,
    state:              DitherCalibrState,
    axis:               DitherCalibrAxis,
    frame:              FrameOptions,
    telescope:          TelescopeOptions,
    start_dec:          f64,
    start_ra:           f64,
    mount_device:       String,
    camera:             DeviceAndProp,
    attempt_num:        usize,
    attempts:           Vec<DitherCalibrAtempt>,
    cur_timed_guide_n:  f64,
    cur_timed_guide_s:  f64,
    cur_timed_guide_w:  f64,
    cur_timed_guide_e:  f64,
    cur_ra:             f64,
    cur_dec:            f64,
    image_width:        usize,
    image_height:       usize,
    move_period:        f64,
    result:             MountMoveCalibrRes,
    next_mode:          Option<Box<dyn Mode + Sync + Send>>,
    can_change_g_rate:  bool,
    calibr_speed:       f64,
    img_proc_stop_flag: Arc<Mutex<Arc<AtomicBool>>>,
}

#[derive(PartialEq)]
enum DitherCalibrAxis {
    Undefined,
    Ra,
    Dec,
}

#[derive(PartialEq)]
enum DitherCalibrState {
    Undefined,
    WaitForImage,
    WaitForSlew,
    WaitForOrigCoords,
}

struct DitherCalibrAtempt {
    stars: Stars,
}

impl MountCalibrMode {
    pub fn new(
        indi:               &Arc<indi::Connection>,
        options:            &Arc<RwLock<Options>>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
        next_mode:          Option<Box<dyn Mode + Sync + Send>>,
    ) -> Self {
        let opts = options.read().unwrap();
        let mut frame = opts.cam.frame.clone();
        frame.exp_main = opts.guiding.calibr_exposure;
        Self {
            indi:               Arc::clone(indi),
            state:              DitherCalibrState::Undefined,
            axis:               DitherCalibrAxis::Undefined,
            frame,
            telescope:          opts.telescope.clone(),
            start_dec:          0.0,
            start_ra:           0.0,
            mount_device:       opts.mount.device.clone(),
            camera:             opts.cam.device.clone(),
            attempt_num:        0,
            attempts:           Vec::new(),
            cur_timed_guide_n:  0.0,
            cur_timed_guide_s:  0.0,
            cur_timed_guide_w:  0.0,
            cur_timed_guide_e:  0.0,
            cur_ra:             0.0,
            cur_dec:            0.0,
            image_width:        0,
            image_height:       0,
            move_period:        0.0,
            result:             MountMoveCalibrRes::default(),
            next_mode,
            can_change_g_rate:  false,
            calibr_speed:       0.0,
            img_proc_stop_flag: Arc::clone(img_proc_stop_flag),
        }
    }

    fn start_for_axis(&mut self, axis: DitherCalibrAxis) -> anyhow::Result<()> {
        start_taking_shots(
            &self.indi,
            &self.frame,
            &self.camera,
            &self.img_proc_stop_flag,
            false
        )?;

        let guid_rate_supported = self.indi.mount_is_guide_rate_supported(&self.mount_device)?;
        self.can_change_g_rate =
            guid_rate_supported &&
            self.indi.mount_get_guide_rate_prop_data(&self.mount_device)?.permition == indi::PropPermition::RW;

        if self.can_change_g_rate {
            self.calibr_speed = DITHER_CALIBR_SPEED;
        } else if guid_rate_supported {
            self.calibr_speed = self.indi.mount_get_guide_rate(&self.mount_device)?.0;
        } else {
            self.calibr_speed = 1.0;
        }
        self.attempt_num = 0;
        self.state = DitherCalibrState::WaitForImage;
        self.axis = axis;
        self.attempts.clear();
        Ok(())
    }

    fn process_axis_results(&mut self) -> anyhow::Result<()> {
        struct AttemptRes {move_x: f64, move_y: f64, dist: f64}
        let mut result = Vec::new();
        for (prev, cur) in self.attempts.iter().tuple_windows() {
            let prev_points: Vec<_> = prev.stars
                .iter()
                .map(|s| Point { x: s.x, y: s.y })
                .collect();
            let points: Vec<_> = cur.stars
                .iter()
                .map(|s| Point { x: s.x, y: s.y })
                .collect();
            let offset = Offset::calculate(
                &prev_points,
                &points,
                self.image_width as f64,
                self.image_height as f64
            );
            if let Some(offset) = offset {
                result.push(AttemptRes{
                    move_x: offset.x,
                    move_y: offset.y,
                    dist: f64::sqrt(offset.x * offset.x + offset.y * offset.y),
                })
            }
        }

        // TODO: check result is not empty

        let dist_max = result.iter().map(|r|r.dist).max_by(cmp_f64).unwrap_or(0.0);
        let min_dist = 0.5 * dist_max;

        result.retain(|r| r.dist > min_dist);
        if self.axis == DitherCalibrAxis::Dec && result.len() >= 2 {
            result.remove(0);
        }

        let x_sum: f64 = result.iter().map(|r| r.move_x).sum();
        let y_sum: f64 = result.iter().map(|r| r.move_y).sum();
        let cnt = result.len() as f64;
        let move_x = x_sum / cnt;
        let move_x = move_x / self.move_period;

        let move_y = y_sum / cnt;
        let move_y = move_y / self.move_period;

        match self.axis {
            DitherCalibrAxis::Ra => {
                self.result.move_x_ra = move_x;
                self.result.move_y_ra = move_y;
                self.start_for_axis(DitherCalibrAxis::Dec)?;
            }
            DitherCalibrAxis::Dec => {
                self.result.move_x_dec = move_x;
                self.result.move_y_dec = move_y;
                if let Some(next_mode) = &mut self.next_mode {
                    next_mode.set_or_correct_value(&mut self.result);
                }
                self.restore_orig_coords()?;
                self.state = DitherCalibrState::WaitForOrigCoords;
            }
            _ => unreachable!()
        }
        Ok(())
    }

    fn restore_orig_coords(&self) -> anyhow::Result<()> {
        self.indi.mount_set_eq_coord(
            &self.mount_device,
            self.start_ra,
            self.start_dec,
            true, None
        )?;
        Ok(())
    }

    fn process_light_frame_info(
        &mut self,
        info:         &LightFrameInfo,
        _subscribers: &Arc<RwLock<Subscribers>>,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if info.stars.fwhm_is_ok && info.stars.ovality_is_ok {
            if self.image_width == 0 || self.image_height == 0 {
                self.image_width = info.width;
                self.image_height = info.height;
                let cam_ccd = indi::CamCcd::from_ccd_prop_name(&self.camera.prop);
                if let Ok((pix_size_x, pix_size_y))
                = self.indi.camera_get_pixel_size_um(&self.camera.name, cam_ccd) {
                    let min_size = f64::min(info.width as f64, info.height as f64);
                    let min_pix_size = f64::min(pix_size_x, pix_size_y);
                    let cam_size_mm = min_size * min_pix_size / 1000.0;
                    let camera_angle = f64::atan2(cam_size_mm, self.telescope.real_focal_length());
                    let sky_angle_is_second = 2.0 * PI / (60.0 * 60.0 * 24.0);
                    // time when point went all camera matrix on sky rotation speed = DITHER_CALIBR_SPEED
                    let cam_time = camera_angle / (sky_angle_is_second * self.calibr_speed);
                    let total_time = cam_time * 0.5; // half of matrix
                    self.move_period = total_time / (DITHER_CALIBR_ATTEMPTS_CNT - 1) as f64;
                    if self.move_period > 3.0 {
                        self.move_period = 3.0;
                    }
                } else {
                    self.move_period = 1.0;
                }
            }
            self.attempts.push(DitherCalibrAtempt {
                stars: info.stars.items.clone(),
            });
            self.attempt_num += 1;
            result = NotifyResult::ProgressChanges;
            if self.attempt_num >= DITHER_CALIBR_ATTEMPTS_CNT {
                result = NotifyResult::ModeChanged;
                self.process_axis_results()?;
            } else {
                let (ns, we) = match self.axis {
                    DitherCalibrAxis::Ra => (0.0, 1000.0 * self.move_period),
                    DitherCalibrAxis::Dec => (1000.0 * self.move_period, 0.0),
                    _ => unreachable!()
                };
                if self.can_change_g_rate {
                    self.indi.mount_set_guide_rate(
                        &self.mount_device,
                        DITHER_CALIBR_SPEED,
                        DITHER_CALIBR_SPEED,
                        true,
                        INDI_SET_PROP_TIMEOUT
                    )?;
                }
                self.indi.mount_timed_guide(&self.mount_device, ns, we)?;
                self.state = DitherCalibrState::WaitForSlew;
            }
        } else {
            start_taking_shots(
                &self.indi,
                &self.frame,
                &self.camera,
                &self.img_proc_stop_flag,
                false
            )?;
        }
        Ok(result)
    }
}

impl Mode for MountCalibrMode {
    fn get_type(&self) -> ModeType {
        ModeType::DitherCalibr
    }

    fn progress_string(&self) -> String {
        match self.axis {
            DitherCalibrAxis::Undefined =>
                "Mount calibration".to_string(),
            DitherCalibrAxis::Ra =>
                "Mount calibration (RA)".to_string(),
            DitherCalibrAxis::Dec =>
                "Mount calibration (DEC)".to_string(),
        }
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        self.restore_orig_coords()?;
        Ok(())
    }

    fn take_next_mode(&mut self) -> Option<ModeBox> {
        self.next_mode.take()
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.frame.exposure())
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.attempt_num,
            total: DITHER_CALIBR_ATTEMPTS_CNT
        })
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.start_dec = self.indi.mount_get_eq_dec(&self.mount_device)?;
        self.start_ra = self.indi.mount_get_eq_ra(&self.mount_device)?;
        self.start_for_axis(DitherCalibrAxis::Ra)?;
        Ok(())
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
                Ok(NotifyResult::Empty),
        }
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;

        if *prop_change.device_name != self.mount_device {
            return Ok(result);
        }
        match self.state {
            DitherCalibrState::WaitForSlew => {
                if let ("TELESCOPE_TIMED_GUIDE_NS"|"TELESCOPE_TIMED_GUIDE_WE",
                        indi::PropChange::Change { value, .. })
                = (prop_change.prop_name.as_str(), &prop_change.change) {
                    match value.elem_name.as_str() {
                        "TIMED_GUIDE_N" => self.cur_timed_guide_n = value.prop_value.to_f64()?,
                        "TIMED_GUIDE_S" => self.cur_timed_guide_s = value.prop_value.to_f64()?,
                        "TIMED_GUIDE_W" => self.cur_timed_guide_w = value.prop_value.to_f64()?,
                        "TIMED_GUIDE_E" => self.cur_timed_guide_e = value.prop_value.to_f64()?,
                        _ => {},
                    }
                    if self.cur_timed_guide_n == 0.0 && self.cur_timed_guide_s == 0.0
                    && self.cur_timed_guide_w == 0.0 && self.cur_timed_guide_e == 0.0 {
                        start_taking_shots(
                            &self.indi,
                            &self.frame,
                            &self.camera,
                            &self.img_proc_stop_flag,
                            false
                        )?;
                        self.state = DitherCalibrState::WaitForImage;
                    }
                }
            }

            DitherCalibrState::WaitForOrigCoords => {
                if let ("EQUATORIAL_EOD_COORD", indi::PropChange::Change { value, new_state, .. })
                = (prop_change.prop_name.as_str(), &prop_change.change) {
                    match value.elem_name.as_str() {
                        "RA" => self.cur_ra = value.prop_value.to_f64()?,
                        "DEC" => self.cur_dec = value.prop_value.to_f64()?,
                        _ => {},
                    }
                    let state_is_ok = *new_state == indi::PropState::Ok;
                    let coord_is_near =
                        f64::abs(self.cur_ra-self.start_ra) < 0.001
                        && f64::abs(self.cur_dec-self.start_dec) < 0.001;
                    if state_is_ok || coord_is_near {
                        // TODO: add delay before got next mode
                        result = NotifyResult::Finished {
                            next_mode: self.next_mode.take()
                        };
                    }
                }
            }

            _ => {},
        }
        Ok(result)
    }

}
