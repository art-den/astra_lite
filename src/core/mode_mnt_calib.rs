use std::{sync::{Arc, RwLock}, f64::consts::PI};
use itertools::Itertools;
use crate::{
    core::cam_ctrl::take_shot,
    hal::{Camera, FrameType, Hal, Telescope},
    image::{stars::*, stars_offset::*},
    options::*,
    utils::math::*,
};
use super::{consts::*, core::*, events::*, frame_processing::*, utils::*};

const DITHER_CALIBR_ATTEMPTS_CNT: usize = 11;

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
    camera:            Arc<dyn Camera + Send + Sync>,
    telescope:         Arc<dyn Telescope + Send + Sync>,
    state:             State,
    axis:              Axis,
    cam_opts:          CamOptions,
    telescope_opts:    TelescopeOptions,
    start_dec:         f64,
    start_ra:          f64,
    camera_dev:        DeviceAndProp,
    attempt_num:       usize,
    attempts:          Vec<CalibrAtempt>,
    image_width:       usize,
    image_height:      usize,
    move_period:       f64,
    result:            MountMoveCalibrRes,
    next_mode:         Option<Box<dyn Mode + Sync + Send>>,
    can_change_g_rate: bool,
    calibr_speed:      f64,
}

#[derive(PartialEq)]
enum Axis {
    Undefined,
    Ra,
    Dec,
}

#[derive(PartialEq)]
enum State {
    Undefined,
    WaitForImage,
    WaitForSlew(usize /* ok_time in ms */),
    WaitForOrigCoords(usize /* ok time in ms */),
}

struct CalibrAtempt {
    stars: StarItems,
}

impl MountCalibrMode {
    pub fn new(
        hal:       &Hal,
        options:   &Arc<RwLock<Options>>,
        next_mode: Option<Box<dyn Mode + Sync + Send>>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();

        let camera = hal.camera(&opts.cam.device_id)?;
        let telescope = hal.telescope(&opts.mount.device)?;

        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };
        let mut cam_opts = opts.cam.clone();
        cam_opts.frame.frame_type = FrameType::Lights;
        cam_opts.frame.exp_main = opts.guiding.main_cam.calibr_exposure;
        cam_opts.frame.gain = gain_to_value(
            opts.guiding.main_cam.calibr_gain,
            opts.cam.frame.gain,
            camera.gain_range()?
        );
        Ok(Self {
            state:             State::Undefined,
            axis:              Axis::Undefined,
            telescope_opts:    opts.telescope.clone(),
            start_dec:         0.0,
            start_ra:          0.0,
            camera_dev:        cam_device.clone(),
            attempt_num:       0,
            attempts:          Vec::new(),
            image_width:       0,
            image_height:      0,
            move_period:       0.0,
            result:            MountMoveCalibrRes::default(),
            can_change_g_rate: false,
            calibr_speed:      0.0,
            camera,
            telescope,
            cam_opts,
            next_mode,
        })
    }

    fn start_for_axis(&mut self, axis: Axis) -> anyhow::Result<()> {
        take_shot(
            &self.camera,
            &self.cam_opts.frame,
            &self.cam_opts.ctrl
        )?;

        let guid_rate_supported = self.telescope.is_guide_rate_supported()?;
        self.can_change_g_rate =
            guid_rate_supported && self.telescope.can_set_guide_rate()?;

        if self.can_change_g_rate {
            self.calibr_speed = MOUNT_CALIBR_SPEED;
        } else if guid_rate_supported {
            self.calibr_speed = self.telescope.guide_rate()?.0;
        } else {
            self.calibr_speed = 1.0;
        }
        self.attempt_num = 0;
        self.state = State::WaitForImage;
        self.axis = axis;
        self.attempts.clear();
        Ok(())
    }

    fn process_axis_results(&mut self) -> anyhow::Result<()> {
        #[derive(Debug)]
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
        if self.axis == Axis::Dec && result.len() >= 2 {
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
            Axis::Ra => {
                self.result.move_x_ra = move_x;
                self.result.move_y_ra = move_y;
                self.start_for_axis(Axis::Dec)?;
            }
            Axis::Dec => {
                self.result.move_x_dec = move_x;
                self.result.move_y_dec = move_y;

                if let Some(next_mode) = &mut self.next_mode {
                    next_mode.set_or_correct_value(&mut self.result);
                }
                self.telescope.goto_and_track(self.start_ra, self.start_dec)?;
                self.state = State::WaitForOrigCoords(0);
            }
            _ => unreachable!()
        }
        Ok(())
    }

    fn process_light_frame_info(
        &mut self,
        info: &LightFrameInfoData,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if info.quality.fwhm_is_ok && info.quality.ovality_is_ok {
            if self.image_width == 0 || self.image_height == 0 {
                self.image_width = info.image.width;
                self.image_height = info.image.height;
                if let Ok((pix_size_x, pix_size_y)) = self.camera.pixel_size_um() {
                    let min_size = f64::min(info.image.width as f64, info.image.height as f64);
                    let min_pix_size = f64::min(pix_size_x, pix_size_y);
                    let cam_size_mm = min_size * min_pix_size / 1000.0;
                    let camera_angle = f64::atan2(cam_size_mm, self.telescope_opts.real_focal_length());
                    let sky_angle_in_seconds = 2.0 * PI / (60.0 * 60.0 * 24.0);
                    // time when point went all camera matrix on sky rotation speed = DITHER_CALIBR_SPEED
                    let cam_time = camera_angle / (sky_angle_in_seconds * self.calibr_speed);
                    let total_time = cam_time * 0.333; // 1/3 of matrix
                    self.move_period = total_time / (DITHER_CALIBR_ATTEMPTS_CNT - 1) as f64;
                    if self.move_period > MAX_TIMED_GUIDE_TIME {
                        self.move_period = MAX_TIMED_GUIDE_TIME;
                    }
                } else {
                    self.move_period = 1.0;
                }
            }
            self.attempts.push(CalibrAtempt {
                stars: Vec::clone(&info.stars.items),
            });
            self.attempt_num += 1;
            result = NotifyResult::ProgressChanges;
            if self.attempt_num >= DITHER_CALIBR_ATTEMPTS_CNT {
                result = NotifyResult::ProgressChanges;
                self.process_axis_results()?;
            } else {
                let (ns, we) = match self.axis {
                    Axis::Ra => (0.0, 1000.0 * self.move_period),
                    Axis::Dec => (1000.0 * self.move_period, 0.0),
                    _ => unreachable!()
                };
                if self.can_change_g_rate {
                    self.telescope.set_guide_rate(MOUNT_CALIBR_SPEED, MOUNT_CALIBR_SPEED)?;
                }
                self.telescope.pulse_guide(ns, we)?;
                self.state = State::WaitForSlew(0);
            }
        } else {
            take_shot(&self.camera, &self.cam_opts.frame, &self.cam_opts.ctrl)?;
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
            Axis::Undefined =>
                "Mount calibration".to_string(),
            Axis::Ra =>
                "Mount calibration (RA)".to_string(),
            Axis::Dec =>
                "Mount calibration (DEC)".to_string(),
        }
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        self.telescope.goto_and_track(self.start_ra, self.start_dec)?;
        Ok(())
    }

    fn frame_options_to_restart_exposure(&self) -> Option<&FrameOptions> {
        Some(&self.cam_opts.frame)
    }

    fn take_next_mode(&mut self) -> Option<ModeBox> {
        self.next_mode.take()
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera_dev)
    }

    fn camera_id(&self) -> Option<&str> {
        Some(&self.camera.id())
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exposure())
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.attempt_num,
            total: DITHER_CALIBR_ATTEMPTS_CNT
        })
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let (ra, dec) = self.telescope.eq_coord()?;
        self.start_dec = ra;
        self.start_ra = dec;
        self.start_for_axis(Axis::Ra)?;
        Ok(())
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        match &fp_result.data {
            FrameProcessResultData::LightFrameInfo(info) =>
                self.process_light_frame_info(info),

            _ =>
                Ok(NotifyResult::Empty),
        }
    }

    fn notify_periodical_timer_tick(&mut self, timer_period_ms: usize) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        match &mut self.state {
            State::WaitForSlew(ok_time_ms) => {
                let guide_pulse_finished = !self.telescope.is_pulse_guiding()?;
                if guide_pulse_finished {
                    *ok_time_ms += timer_period_ms;
                    if *ok_time_ms >= AFTER_MOUNT_MOVE_WAIT_TIME * 1000 {
                        if self.telescope.is_abort_motion_supported() {
                            self.telescope.abort_motion()?
                        }

                        take_shot(
                            &self.camera,
                            &self.cam_opts.frame,
                            &self.cam_opts.ctrl
                        )?;
                        self.state = State::WaitForImage;
                        result = NotifyResult::ProgressChanges;
                    }
                }
            }
            State::WaitForOrigCoords(ok_time_ms) => {
                if !self.telescope.is_slewing()? {
                    *ok_time_ms += timer_period_ms;
                    if *ok_time_ms >= AFTER_GOTO_WAIT_TIME {
                        result = NotifyResult::Finished {
                            next_mode: self.next_mode.take()
                        };
                    }
                }
            }
            _ => {}
        }
        Ok(result)
    }
}
