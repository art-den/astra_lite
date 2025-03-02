use std::{f64::consts::PI, sync::{Arc, RwLock}};


use chrono::{NaiveDateTime, Utc};

use crate::{core::{core::*, frame_processing::*}, image::{image::*, stars::StarItems}, indi, options::*, plate_solve::*, ui::sky_map::math::*};

use super::{consts::*, events::*, utils::{check_telescope_is_at_desired_position, gain_to_value}};

///////////////////////////////////////////////////////////////////////////////

struct PolarAlignmentMeasure {
    coord:    EqCoord,
    utc_time: NaiveDateTime,
}

struct PolarAlignment {
    measurements: Vec<PolarAlignmentMeasure>,
    pole:         Option<HorizCoord>,
    mount_pole:   Option<HorizCoord>,
}

impl PolarAlignment {
    fn new() -> Self {
        Self {
            measurements: Vec::new(),
            pole:         None,
            mount_pole:   None,
        }
    }

    fn add_measurement(&mut self, measurement: PolarAlignmentMeasure) {
        self.measurements.push(measurement);
    }

    fn calc_mount_pole(&mut self, latitude: f64, longitude: f64) {
        assert!(self.measurements.len() == 3);

        let latitude = degree_to_radian(latitude);
        let longitude = degree_to_radian(longitude);

        let horiz_crd = |m: &PolarAlignmentMeasure| -> HorizCoord {
            let cvt = EqToSphereCvt::new(longitude, latitude, &m.utc_time);
            HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&m.coord))
        };

        let pt1 = horiz_crd(&self.measurements[0]).to_sphere_pt();
        let pt2 = horiz_crd(&self.measurements[1]).to_sphere_pt();
        let pt3 = horiz_crd(&self.measurements[2]).to_sphere_pt();

        let vec1 = &pt2 - &pt1;
        let vec2 = &pt3 - &pt2;

        let mut mount_pole_crd = &vec1 * &vec2;
        mount_pole_crd.normalize();
        let mount_pole = HorizCoord::from_sphere_pt(&mount_pole_crd);

        let cvt = EqToSphereCvt::new(longitude, latitude, &Utc::now().naive_utc());
        let celestial_pole = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&EqCoord { ra: 0.0, dec: 0.5 * PI }));

        self.pole = Some(celestial_pole);
        self.mount_pole = Some(mount_pole);
    }

    fn pole_error(&self) -> Option<HorizCoord> {
        let (Some(pole), Some(mnt_pole)) = (&self.pole, &self.mount_pole) else { return None; };

        Some(HorizCoord {
            alt: mnt_pole.alt - pole.alt,
            az: mnt_pole.az - pole.az,
        })
    }
}

///////////////////////////////////////////////////////////////////////////////

const AFTER_GOTO_WAIT_TIME: usize = 3; // seconds

enum State {
    Undefined,
    Goto,
    Capture,
    PlateSolve,
}

enum Step {
    Undefined,
    First,
    Second,
    Third,
    Corr
}

#[derive(Clone)]
pub enum PolarAlignmentEvent {
    Error(HorizCoord),
}

pub struct PolarAlignMode {
    state:        State,
    step:         Step,
    camera:       DeviceAndProp,
    mount:        String,
    cam_opts:     CamOptions,
    pa_opts:      PloarAlignOptions,
    s_opts:       SiteOptions,
    options:      Arc<RwLock<Options>>,
    indi:         Arc<indi::Connection>,
    subscribers:  Arc<EventSubscriptions>,
    ps_opts:      PlateSolverOptions,
    plate_solver: PlateSolver,
    goto_time:    usize,
    goto_ok_cnt:  usize,
    goto_pos:     EqCoord,
    alignment:    PolarAlignment,
}

impl PolarAlignMode {
    pub fn new(
        indi:        &Arc<indi::Connection>,
        options:     &Arc<RwLock<Options>>,
        subscribers: &Arc<EventSubscriptions>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };

        let mut cam_opts = opts.cam.clone();
        cam_opts.frame.frame_type = crate::image::raw::FrameType::Lights;
        cam_opts.frame.exp_main = opts.plate_solver.exposure;
        cam_opts.frame.binning = opts.plate_solver.bin;
        cam_opts.frame.gain = gain_to_value(
            opts.plate_solver.gain,
            opts.cam.frame.gain,
            &cam_device,
            indi
        )?;

        let plate_solver = PlateSolver::new(opts.plate_solver.solver);

        Ok(Self{
            state:       State::Undefined,
            step:        Step::Undefined,
            camera:      cam_device.clone(),
            mount:       opts.mount.device.clone(),
            pa_opts:     opts.polar_align.clone(),
            s_opts:      opts.site.clone(),
            options:     Arc::clone(options),
            indi:        Arc::clone(indi),
            subscribers: Arc::clone(subscribers),
            ps_opts:     opts.plate_solver.clone(),
            alignment:   PolarAlignment::new(),
            goto_time:   0,
            goto_ok_cnt: 0,
            goto_pos:    Default::default(),
            cam_opts,
            plate_solver
        })
    }

    fn start_capture(&self) -> anyhow::Result<()> {
        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
        Ok(())
    }

    fn plate_solve_image(&mut self, image: &Arc<RwLock<Image>>) -> anyhow::Result<()> {
        let image = image.read().unwrap();
        let mut config = PlateSolveConfig::default();
        config.time_out = self.ps_opts.timeout;
        config.blind_time_out = self.ps_opts.blind_timeout;
        self.plate_solver.start(&PlateSolverInData::Image(&image), &config)?;
        drop(image);
        Ok(())
    }

    fn plate_solve_stars(
        &mut self,
        stars:      &StarItems,
        img_width:  usize,
        img_height: usize
    ) -> anyhow::Result<()> {
        let mut config = PlateSolveConfig::default();
        config.time_out = self.ps_opts.timeout;
        config.blind_time_out = self.ps_opts.blind_timeout;
        let stars_arg = PlateSolverInData::Stars{
            stars,
            img_width,
            img_height,
        };
        self.plate_solver.start(&stars_arg, &config)?;
        Ok(())
    }

    fn try_process_plate_solving_result(&mut self) -> anyhow::Result<NotifyResult> {
        let result = match self.plate_solver.get_result()? {
            PlateSolveResult::Waiting => return Ok(NotifyResult::Empty),
            PlateSolveResult::Done(result) => result,
            PlateSolveResult::Failed => anyhow::bail!("Can't platesolve image")
        };

        // Add polar alignment error in debug mode
        let result = if cfg!(debug_assertions) {
            let cvt = EqToSphereCvt::new(self.s_opts.latitude, self.s_opts.longitude, &Utc::now().naive_utc());

            let mut horiz = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&result.crd_now));
            let options = self.options.read().unwrap();
            horiz.alt += degree_to_radian(options.polar_align.sim_alt_err);
            horiz.az += degree_to_radian(options.polar_align.sim_az_err);
            drop(options);

            let mut result = result;
            result.crd_now = cvt.sphere_to_eq(&horiz.to_sphere_pt());
            result
        } else {
            result
        };

        result.print_to_log();

        let event = PlateSolverEvent {
            cam_name: self.camera.name.clone(),
            result: result.clone(),
        };
        self.subscribers.notify(Event::PlateSolve(event));

        match self.step {
            Step::First | Step::Second => {
                self.alignment.add_measurement(PolarAlignmentMeasure {
                    coord:    result.crd_now,
                    utc_time: Utc::now().naive_utc(),
                });
                self.move_next_pos()?;
                self.state = State::Goto;
            }
            Step::Third => {
                self.alignment.add_measurement(PolarAlignmentMeasure {
                    coord:    result.crd_now,
                    utc_time: Utc::now().naive_utc(),
                });
                self.alignment.calc_mount_pole(self.s_opts.latitude, self.s_opts.longitude);
                self.notify_error()?;
                self.start_capture()?;
                self.step = Step::Corr;
                self.state = State::Capture;
            }
            Step::Corr => {
                self.start_capture()?;
                self.state = State::Capture;
            }

            _ => unreachable!(),
        }

        Ok(NotifyResult::ProgressChanges)
    }

    fn move_next_pos(&mut self) -> anyhow::Result<()> {
        let (cur_ra, cur_dec) = self.indi.mount_get_eq_ra_and_dec(&self.mount)?;
        let cur_ra = hour_to_radian(cur_ra);
        let cur_dec = degree_to_radian(cur_dec);
        let angle = match self.pa_opts.direction {
            PloarAlignDir::East => self.pa_opts.angle,
            PloarAlignDir::West => -self.pa_opts.angle,
        };
        let mut new_ra = cur_ra + degree_to_radian(0.5 * angle);
        while new_ra < 0.0 { new_ra += 2.0 * PI; }
        while new_ra >= 2.0 * PI { new_ra -= 2.0 * PI; }
        self.goto_pos = EqCoord { ra: new_ra, dec: cur_dec };
        self.indi.set_after_coord_set_action(
            &self.mount,
            indi::AfterCoordSetAction::Track,
            true,
            Some(1000)
        )?;
        self.indi.mount_set_eq_coord(
            &self.mount,
            radian_to_hour(self.goto_pos.ra),
            radian_to_degree(self.goto_pos.dec),
            true,
            None
        )?;
        self.goto_time = 0;
        self.goto_ok_cnt = 0;
        Ok(())
    }

    fn notify_error(&mut self) -> anyhow::Result<()> {
        let Some(error) = self.alignment.pole_error() else {
            anyhow::bail!("Mount pole is not calculated!");
        };
        self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Error(
            error
        )));
        Ok(())
    }
}

impl Mode for PolarAlignMode {
    fn get_type(&self) -> ModeType {
        ModeType::PolarAlignment
    }

    fn progress(&self) -> Option<Progress> {
        let step = match (&self.step, &self.state) {
            (Step::First,  State::Capture   ) => 0,
            (Step::First,  State::PlateSolve) => 1,
            (Step::First,  State::Goto      ) => 2,
            (Step::Second, State::Capture   ) => 3,
            (Step::Second, State::PlateSolve) => 4,
            (Step::Second, State::Goto      ) => 5,
            (Step::Third,  State::Capture   ) => 6,
            (Step::Third,  State::PlateSolve) => 7,
            (Step::Corr,   _                ) => 8,
            _                                 => 0,
        };

        Some(Progress{ cur: step, total: 8 })
    }

    fn progress_string(&self) -> String {
        match (&self.step, &self.state) {
            (Step::First,  State::Capture   ) => "1st capture",
            (Step::First,  State::PlateSolve) => "1st platesolve",
            (Step::First,  State::Goto      ) => "1st goto",
            (Step::Second, State::Capture   ) => "2nd capture",
            (Step::Second, State::PlateSolve) => "2nd platesolve",
            (Step::Second, State::Goto      ) => "2nd goto",
            (Step::Third,  State::Capture   ) => "3rd capture",
            (Step::Third,  State::PlateSolve) => "3rd platesolve",
            (Step::Corr,   State::Capture   ) => "Capture",
            (Step::Corr,   State::PlateSolve) => "Platesolve",

            _ => "",
        }.to_string()
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exp_main)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.start_capture()?;
        self.state = State::Capture;
        self.step = Step::First;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        _ = abort_camera_exposure(&self.indi, &self.camera);
        _ = self.indi.mount_abort_motion(&self.mount);
        self.state = State::Undefined;
        Ok(())
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        let stars_supported = self.plate_solver.support_stars_as_input();
        match (&self.state, &fp_result.data, stars_supported) {
            (State::Capture, FrameProcessResultData::Image(image), false) => {
                self.plate_solve_image(image)?;
                self.state = State::PlateSolve;
                return Ok(NotifyResult::ProgressChanges);
            }
            (State::Capture, FrameProcessResultData::LightFrameInfo(info), true) => {
                self.plate_solve_stars(&info.stars.items, info.image.width, info.image.height)?;
                self.state = State::PlateSolve;
                return Ok(NotifyResult::ProgressChanges);
            }
            _ => {},
        };
        Ok(NotifyResult::Empty)
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        match self.state {
            State::PlateSolve => {
                return self.try_process_plate_solving_result();
            }

            State::Goto => {
                if self.indi.mount_get_eq_coord_prop_state(&self.mount)? == indi::PropState::Ok {
                    self.goto_ok_cnt += 1;
                    if self.goto_ok_cnt >= AFTER_GOTO_WAIT_TIME {
                        check_telescope_is_at_desired_position(
                            &self.indi,
                            &self.mount,
                            &self.goto_pos,
                            0.5
                        )?;

                        self.start_capture()?;
                        self.state = State::Capture;

                        match self.step {
                            Step::First  => self.step = Step::Second,
                            Step::Second => self.step = Step::Third,
                            _            => unreachable!(),
                        }

                        return Ok(NotifyResult::ProgressChanges);
                    }
                }

                self.goto_time += 1;
                if self.goto_time > MAX_GOTO_TIME {
                    anyhow::bail!("Telescope is moving too long time (> {}s)", MAX_GOTO_TIME);
                }
            }

            _ => {},
        }
        Ok(NotifyResult::Empty)
    }
}
