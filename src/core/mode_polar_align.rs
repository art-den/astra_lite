use std::{f64::consts::PI, sync::{Arc, RwLock}};

use chrono::{NaiveDateTime, Utc};

use crate::{
    core::{core::*, frame_processing::*},
    image::{image::*, stars::StarItems},
    indi,
    options::*,
    plate_solve::*,
    sky_math::math::*,
};

use super::{consts::*, events::*, utils::{check_telescope_is_at_desired_position, gain_to_value}};

///////////////////////////////////////////////////////////////////////////////

struct PolarAlignmentMeasure {
    coord:    Point3D,
    utc_time: NaiveDateTime,
}

struct Result {
    earth_pole:   HorizCoord,
    target_point: HorizCoord,
}

struct PolarAlignment {
    measurements: Vec<PolarAlignmentMeasure>,
    result:  Option<Result>,
}

impl PolarAlignment {
    fn new() -> Self {
        Self {
            measurements: Vec::new(),
            result:  None,
        }
    }

    fn add_measurement(&mut self, measurement: PolarAlignmentMeasure) {
        self.measurements.push(measurement);
    }

    fn calc_mount_pole(&mut self, longitude: f64, latitude: f64) -> anyhow::Result<()> {
        assert!(self.measurements.len() >= 3);

        let pt1 = &self.measurements[0];
        let pt2 = &self.measurements[1];
        let pt3 = &self.measurements[2];

        // Calc mount pole (axis)

        let mount_pole_crd = Point3D::normal(&pt1.coord, &pt2.coord, &pt3.coord)
            .normalized()
            .ok_or_else(|| anyhow::anyhow!("Can't calculate mount pole!"))?;

        let mount_pole = HorizCoord::from_sphere_pt(&mount_pole_crd);

        // Correct points by pole (rotate around pole) and calc pole again.
        // It is not nessesary but better to do it for slow mounts

        let time_diff1 = (pt2.utc_time - pt1.utc_time).num_milliseconds() as f64 / 1000.0;
        let time_diff2 = (pt3.utc_time - pt2.utc_time).num_milliseconds() as f64 / 1000.0;

        let pt1_corrected_crd = Self::rotate_point_around_pole_with_earth_speed(
            &pt1.coord,
            &mount_pole,
            time_diff1 + time_diff2
        );

        let pt2_corrected_crd = Self::rotate_point_around_pole_with_earth_speed(
            &pt2.coord,
            &mount_pole,
            time_diff2
        );

        let mount_pole_crd = Point3D::normal(&pt1_corrected_crd, &pt2_corrected_crd, &pt3.coord)
            .normalized()
            .ok_or_else(|| anyhow::anyhow!("Can't calculate corrected mount pole!"))?;

        let mount_pole = HorizCoord::from_sphere_pt(&mount_pole_crd);

        // Earth pole (axis)

        let cvt = EqToSphereCvt::new(longitude, latitude, &self.measurements[1].utc_time);
        let earth_pole = HorizCoord::from_sphere_pt(
            &cvt.eq_to_sphere(&EqCoord { ra: 0.0, dec: 0.5 * PI })
        );

        // Find target point (3rd point with error adjust)

        let alt_error = mount_pole.alt - earth_pole.alt;
        let az_error = mount_pole.az - earth_pole.az;
        let mut target_pt = pt3.coord.clone();
        target_pt.rotate_over_x(-az_error);
        target_pt.rotate_over_y(-alt_error);
        let target_point = HorizCoord::from_sphere_pt(&target_pt);

        self.result = Some(Result {
            earth_pole,
            target_point
        });

        Ok(())
    }

    fn pole_error(&self) -> Option<HorizCoord> {
        assert!(self.measurements.len() >= 3);

        let Some(result) = &self.result else {
            return None;
        };

        let third = &self.measurements[2];
        let last = self.measurements.last().unwrap();

        let target = Self::rotate_point_around_pole_with_earth_speed(
            &result.target_point.to_sphere_pt(),
            &result.earth_pole,
            (last.utc_time - third.utc_time).num_milliseconds() as f64 / 1000.0
        );

        // How we do rotate mount to go from target point to last point
        let changes = coordinate_descent(
            vec![0.0, 0.0],
            PI / (360.0 * 60.0 * 60.0 * 5.0),
            1_000_000,
            |changes| {
                let mut crd = last.coord.clone();
                crd.rotate_over_x(changes[0]);
                crd.rotate_over_y(changes[1]);
                Point3D::angle(&crd, &target).unwrap_or(0.0)
            },
        );

        Some(HorizCoord {
            alt: -changes[1],
            az: -changes[0],
        })
    }

    fn clear(&mut self) {
        self.measurements.clear();
        self.result = None;
    }

    fn rotate_point_around_pole_with_earth_speed(
        point:           &Point3D,
        pole:            &HorizCoord,
        time_in_seconds: f64
    ) -> Point3D {
        let mut target = point.clone();
        target.rotate_over_x(-pole.az);
        target.rotate_over_y(-pole.alt);
        let earth_rotations = time_in_seconds as f64 / 86164.09054;
        target.rotate_over_z(2.0 * PI * earth_rotations);
        target.rotate_over_y(pole.alt);
        target.rotate_over_x(pole.az);
        target
    }

}

///////////////////////////////////////////////////////////////////////////////

const AFTER_GOTO_WAIT_TIME: usize = 3; // seconds

#[derive(PartialEq)]
enum State {
    Undefined,
    Goto,
    Capture,
    PlateSolve,
}

#[derive(PartialEq)]
enum Step {
    Undefined,
    GotoInitialPos,
    First,
    Second,
    Third,
    Corr
}

#[derive(Clone)]
pub enum PolarAlignmentEvent {
    Error(Option<HorizCoord>),
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
    cur_frame:    Arc<ResultImage>,
    subscribers:  Arc<EventSubscriptions>,
    ps_opts:      PlateSolverOptions,
    plate_solver: PlateSolver,
    goto_time:    usize,
    goto_ok_cnt:  usize,
    goto_pos:     EqCoord,
    alignment:    PolarAlignment,
    image_time:   Option<NaiveDateTime>,
    initial_crd:  Option<EqCoord>,
}

impl PolarAlignMode {
    pub fn new(
        indi:        &Arc<indi::Connection>,
        cur_frame:   &Arc<ResultImage>,
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
            cur_frame:   Arc::clone(cur_frame),
            subscribers: Arc::clone(subscribers),
            ps_opts:     opts.plate_solver.clone(),
            alignment:   PolarAlignment::new(),
            goto_time:   0,
            goto_ok_cnt: 0,
            goto_pos:    Default::default(),
            image_time:  None,
            initial_crd: None,
            cam_opts,
            plate_solver
        })
    }

    fn start_capture(&mut self) -> anyhow::Result<()> {
        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
        self.image_time = Some(Utc::now().naive_utc());
        Ok(())
    }

    fn get_platesolver_config(&self) -> anyhow::Result<PlateSolveConfig> {
        let (ra, dec) = self.indi.mount_get_eq_ra_and_dec(&self.mount)?;
        let mut config = PlateSolveConfig::default();
        config.time_out = self.ps_opts.timeout;
        config.blind_time_out = self.ps_opts.blind_timeout;
        config.eq_coord = Some(EqCoord {
            dec: degree_to_radian(dec),
            ra:  hour_to_radian(ra),
        });
        if self.step == Step::Corr {
            config.allow_blind = false;
        }
        Ok(config)
    }

    // Returns Ok(true) on silent error
    fn plate_solve_image(&mut self, image: &Arc<RwLock<Image>>) -> anyhow::Result<bool> {
        let image = image.read().unwrap();
        let config = self.get_platesolver_config()?;
        let start_result = self.plate_solver.start(&PlateSolverInData::Image(&image), &config);
        if let Err(err) = start_result {
            self.process_platesolver_fail(err.to_string().as_str())?;
            return Ok(false);
        }
        Ok(true)
    }

    fn plate_solve_stars(
        &mut self,
        stars:      &StarItems,
        img_width:  usize,
        img_height: usize
    ) -> anyhow::Result<bool> {
        let config = self.get_platesolver_config()?;
        let stars_arg = PlateSolverInData::Stars{ stars, img_width, img_height };
        let start_result = self.plate_solver.start(&stars_arg, &config);
        if let Err(err) = start_result {
            self.process_platesolver_fail(err.to_string().as_str())?;
            return Ok(false);
        }
        Ok(true)
    }

    fn process_platesolver_fail(&mut self, err_str: &str) -> anyhow::Result<()> {
        if self.step == Step::Corr {
            self.start_capture()?;
            self.state = State::Capture;
            return Ok(());
        } else {
            anyhow::bail!("{}", err_str);
        }
}

    fn try_process_plate_solving_result(&mut self) -> anyhow::Result<NotifyResult> {
        assert!(self.state == State::PlateSolve);

        let ps_result = match self.plate_solver.get_result()? {
            PlateSolveResult::Waiting => return Ok(NotifyResult::Empty),
            PlateSolveResult::Done(result) => result,
            PlateSolveResult::Failed => {
                self.process_platesolver_fail("Can't platesolve image")?;
                return Ok(NotifyResult::Empty);
            }
        };

        ps_result.print_to_log();

        let image_time = self.image_time
            .ok_or_else(|| anyhow::anyhow!("Image time is not stored"))?;

        let cvt = EqToSphereCvt::new(
            degree_to_radian(self.s_opts.longitude),
            degree_to_radian(self.s_opts.latitude),
            &image_time
        );

        let mut coord = cvt.eq_to_sphere(&ps_result.crd_now);

        // Add polar alignment error only in debug mode
        if cfg!(debug_assertions) {
            let options = self.options.read().unwrap();
            let az_err = degree_to_radian(options.polar_align.sim_az_err);
            let alt_err = degree_to_radian(options.polar_align.sim_alt_err);
            coord.rotate_over_y(alt_err);
            coord.rotate_over_x(az_err);
        }

        // Image for preview in map

        let options = self.options.read().unwrap();
        let preview = self.cur_frame.create_preview_for_platesolve_image(&options.preview);
        drop(options);

        let event = PlateSolverEvent {
            cam_name: self.camera.name.clone(),
            result: ps_result.clone(),
            preview: preview.map(|p| Arc::new(p)),
        };
        self.subscribers.notify(Event::PlateSolve(event));

        self.alignment.add_measurement(PolarAlignmentMeasure {
            coord:    coord.clone(),
            utc_time: image_time,
        });

        match self.step {
            Step::First | Step::Second => {
                self.move_next_pos()?;
                self.state = State::Goto;
            }
            Step::Third => {
                self.alignment.calc_mount_pole(
                    degree_to_radian(self.s_opts.longitude),
                    degree_to_radian(self.s_opts.latitude),
                )?;
                self.notify_error()?;
                self.start_capture()?;
                self.step = Step::Corr;
                self.state = State::Capture;
            }
            Step::Corr => {
                self.notify_error()?;
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

    fn notify_error(&self) -> anyhow::Result<()> {
        let Some(error) = self.alignment.pole_error() else {
            anyhow::bail!("Mount pole is not calculated!");
        };
        self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Error(
            Some(error)
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
            (Step::GotoInitialPos, _                ) => 0,
            (Step::First,          State::Capture   ) => 0,
            (Step::First,          State::PlateSolve) => 1,
            (Step::First,          State::Goto      ) => 2,
            (Step::Second,         State::Capture   ) => 3,
            (Step::Second,         State::PlateSolve) => 4,
            (Step::Second,         State::Goto      ) => 5,
            (Step::Third,          State::Capture   ) => 6,
            (Step::Third,          State::PlateSolve) => 7,
            (Step::Corr,           _                ) => 8,
            _                                         => 0,
        };

        Some(Progress{ cur: step, total: 8 })
    }

    fn progress_string(&self) -> String {
        match (&self.step, &self.state) {
            (Step::GotoInitialPos, _        ) => "Goto initial position",
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
        let (ra, dec) = self.indi.mount_get_eq_ra_and_dec(&self.mount)?;
        self.initial_crd = Some(EqCoord {
            ra: hour_to_radian(ra),
            dec: degree_to_radian(dec),
        });

        self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Error(
            None
        )));

        self.start_capture()?;
        self.state = State::Capture;
        self.step = Step::First;
        Ok(())
    }

    fn restart(&mut self) -> anyhow::Result<()> {
        if let Some(initial_crd) = self.initial_crd.clone() {
            self.abort()?;

            self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Error(
                None
            )));

            self.indi.set_after_coord_set_action(
                &self.mount,
                indi::AfterCoordSetAction::Track,
                true,
                Some(1000)
            )?;
            self.indi.mount_set_eq_coord(
                &self.mount,
                radian_to_hour(initial_crd.ra),
                radian_to_degree(initial_crd.dec),
                true,
                None
            )?;
            self.goto_pos = initial_crd;
            self.goto_time = 0;
            self.goto_ok_cnt = 0;

            self.alignment.clear();
            self.state = State::Goto;
            self.step = Step::GotoInitialPos;
        }

        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        _ = abort_camera_exposure(&self.indi, &self.camera);
        _ = self.indi.mount_abort_motion(&self.mount);
        self.plate_solver.abort();
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
                let ok = self.plate_solve_image(image)?;
                if !ok { return Ok(NotifyResult::Empty); }
                self.state = State::PlateSolve;
                return Ok(NotifyResult::ProgressChanges);
            }
            (State::Capture, FrameProcessResultData::LightFrameInfo(info), true) => {
                let ok = self.plate_solve_stars(&info.stars.items, info.image.width, info.image.height)?;
                if !ok { return Ok(NotifyResult::Empty); }
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
                let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.mount)?;
                if matches!(crd_prop_state, indi::PropState::Ok|indi::PropState::Idle) {
                    self.goto_ok_cnt += 1;
                    if self.goto_ok_cnt >= AFTER_GOTO_WAIT_TIME {
                        check_telescope_is_at_desired_position(
                            &self.indi,
                            &self.mount,
                            &self.goto_pos,
                            0.5
                        )?;

                        match self.step {
                            Step::GotoInitialPos => {
                                self.start_capture()?;
                                self.state = State::Capture;
                                self.step = Step::First;
                            }
                            Step::First => {
                                self.start_capture()?;
                                self.state = State::Capture;
                                self.step = Step::Second;
                            }
                            Step::Second => {
                                self.start_capture()?;
                                self.state = State::Capture;
                                self.step = Step::Third;
                            }
                            _ =>
                                unreachable!(),
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
