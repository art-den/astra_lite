use std::{sync::{Arc, RwLock}, f64::consts::PI};
use itertools::Itertools;
use crate::{
    image::{stars::*, stars_offset::*}, indi, options::*, utils::math::*
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
    indi:              Arc<indi::Connection>,
    state:             State,
    axis:              Axis,
    cam_opts:          CamOptions,
    telescope:         TelescopeOptions,
    start_dec:         f64,
    start_ra:          f64,
    mount_device:      String,
    camera:            DeviceAndProp,
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
    WaitForSlew(usize /* ok_time */),
    WaitForOrigCoords(usize /* ok time */),
}

struct CalibrAtempt {
    stars: StarItems,
}

impl MountCalibrMode {
    pub fn new(
        indi:      &Arc<indi::Connection>,
        options:   &Arc<RwLock<Options>>,
        next_mode: Option<Box<dyn Mode + Sync + Send>>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };
        let mut cam_opts = opts.cam.clone();
        cam_opts.frame.frame_type = crate::image::raw::FrameType::Lights;
        cam_opts.frame.exp_main = opts.guiding.main_cam.calibr_exposure;
        cam_opts.frame.gain = gain_to_value(
            opts.guiding.main_cam.calibr_gain,
            opts.cam.frame.gain,
            cam_device,
            indi
        )?;
        Ok(Self {
            indi:              Arc::clone(indi),
            state:             State::Undefined,
            axis:              Axis::Undefined,
            telescope:         opts.telescope.clone(),
            start_dec:         0.0,
            start_ra:          0.0,
            mount_device:      opts.mount.device.clone(),
            camera:            cam_device.clone(),
            attempt_num:       0,
            attempts:          Vec::new(),
            image_width:       0,
            image_height:      0,
            move_period:       0.0,
            result:            MountMoveCalibrRes::default(),
            can_change_g_rate: false,
            calibr_speed:      0.0,
            cam_opts,
            next_mode,
        })
    }

    fn start_for_axis(&mut self, axis: Axis) -> anyhow::Result<()> {
        apply_camera_options_and_take_shot(
            &self.indi,
            &self.camera,
            &self.cam_opts.frame,
            &self.cam_opts.ctrl
        )?;

        let guid_rate_supported = self.indi.mount_is_guide_rate_supported(&self.mount_device)?;
        self.can_change_g_rate =
            guid_rate_supported &&
            self.indi.mount_get_guide_rate_prop_data(&self.mount_device)?.permition == indi::PropPermition::RW;

        if self.can_change_g_rate {
            self.calibr_speed = MOUNT_CALIBR_SPEED;
        } else if guid_rate_supported {
            self.calibr_speed = self.indi.mount_get_guide_rate(&self.mount_device)?.0;
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
                self.restore_orig_coords()?;
                self.state = State::WaitForOrigCoords(0);
            }
            _ => unreachable!()
        }
        Ok(())
    }

    fn restore_orig_coords(&self) -> anyhow::Result<()> {
        self.indi.mount_set_after_coord_action(
            &self.mount_device,
            indi::AfterCoordSetAction::Track,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;

        self.indi.mount_set_eq_coord(
            &self.mount_device,
            self.start_ra,
            self.start_dec,
            true,
            None
        )?;
        Ok(())
    }

    fn process_light_frame_info(
        &mut self,
        info: &LightFrameInfoData,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if info.stars.info.fwhm_is_ok && info.stars.info.ovality_is_ok {
            if self.image_width == 0 || self.image_height == 0 {
                self.image_width = info.image.width;
                self.image_height = info.image.height;
                let cam_ccd = indi::CamCcd::from_ccd_prop_name(&self.camera.prop);
                if let Ok((pix_size_x, pix_size_y))
                = self.indi.camera_get_pixel_size_um(&self.camera.name, cam_ccd) {
                    let min_size = f64::min(info.image.width as f64, info.image.height as f64);
                    let min_pix_size = f64::min(pix_size_x, pix_size_y);
                    let cam_size_mm = min_size * min_pix_size / 1000.0;
                    let camera_angle = f64::atan2(cam_size_mm, self.telescope.real_focal_length());
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
                    self.indi.mount_set_guide_rate(
                        &self.mount_device,
                        MOUNT_CALIBR_SPEED,
                        MOUNT_CALIBR_SPEED,
                        true,
                        INDI_SET_PROP_TIMEOUT
                    )?;
                }
                self.indi.mount_timed_guide(&self.mount_device, ns, we)?;
                self.state = State::WaitForSlew(0);
            }
        } else {
            apply_camera_options_and_take_shot(
                &self.indi,
                &self.camera,
                &self.cam_opts.frame,
                &self.cam_opts.ctrl
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
            Axis::Undefined =>
                "Mount calibration".to_string(),
            Axis::Ra =>
                "Mount calibration (RA)".to_string(),
            Axis::Dec =>
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
        Some(self.cam_opts.frame.exposure())
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

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        match &mut self.state {
            State::WaitForSlew(ok_time) => {
                let guide_pulse_finished = self.indi.mount_is_timed_guide_finished(&self.mount_device)?;
                if guide_pulse_finished {
                    *ok_time += 1;
                    if *ok_time == AFTER_MOUNT_MOVE_WAIT_TIME {
                        self.indi.mount_abort_motion(&self.mount_device)?;
                        apply_camera_options_and_take_shot(
                            &self.indi,
                            &self.camera,
                            &self.cam_opts.frame,
                            &self.cam_opts.ctrl
                        )?;
                        self.state = State::WaitForImage;
                        result = NotifyResult::ProgressChanges;
                    }
                }
            }
            State::WaitForOrigCoords(ok_time) => {
                let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.mount_device)?;
                if matches!(crd_prop_state, indi::PropState::Ok|indi::PropState::Idle) {
                    *ok_time += 1;
                    if *ok_time >= AFTER_GOTO_WAIT_TIME {
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
