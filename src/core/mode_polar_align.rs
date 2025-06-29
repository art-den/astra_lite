use std::{any::Any, f64::consts::PI, sync::{Arc, RwLock}};

use chrono::{NaiveDateTime, Utc};

use crate::{
    core::{core::*, frame_processing::*},
    image::{image::*, stars::StarItems},
    indi::{self, degree_to_str},
    options::*,
    plate_solve::*,
    sky_math::{math::*, solar_system::calc_atmospheric_refraction},
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
    result:       Option<Result>,
}

impl PolarAlignment {
    fn new() -> Self {
        Self {
            measurements: Vec::new(),
            result:       None,
        }
    }

    fn add_measurement(&mut self, measurement: PolarAlignmentMeasure) {
        self.measurements.push(measurement);
    }

    fn calc_mount_pole(&mut self, longitude: f64, latitude: f64) -> anyhow::Result<()> {
        assert!(self.measurements.len() >= 3);

        log::debug!(
            "Calculating PA error for latitude={}, longitude={}",
            degree_to_str(radian_to_degree(latitude)),
            degree_to_str(radian_to_degree(longitude)),
        );

        // Earth pole (axis)

        let cvt = EqToSphereCvt::new(longitude, latitude, &self.measurements[1].utc_time);
        let earth_pole_3d = cvt.eq_to_sphere(&EqCoord { ra: 0.0, dec: 0.5 * PI });
        let earth_pole = HorizCoord::from_sphere_pt(&earth_pole_3d);

        log::debug!(
            "Earth pole alt={}, az={}",
            degree_to_str(radian_to_degree(earth_pole.alt)),
            degree_to_str(radian_to_degree(earth_pole.az)),
        );

        // Calc mount pole (axis)

        let pt1 = &self.measurements[0];
        let pt2 = &self.measurements[1];
        let pt3 = &self.measurements[2];

        let mut mount_pole_3d = Point3D::normal(&pt1.coord, &pt2.coord, &pt3.coord)
            .normalized()
            .ok_or_else(|| anyhow::anyhow!("Can't calculate mount pole!"))?;

        if mount_pole_3d.x.is_sign_positive() != earth_pole_3d.x.is_sign_positive() {
            mount_pole_3d.x = -mount_pole_3d.x;
            mount_pole_3d.y = -mount_pole_3d.y;
            mount_pole_3d.z = -mount_pole_3d.z;
        }

        let mount_pole = HorizCoord::from_sphere_pt(&mount_pole_3d);

        log::debug!(
            "Mount pole alt={}, az={}",
            degree_to_str(radian_to_degree(mount_pole.alt)),
            degree_to_str(radian_to_degree(mount_pole.az)),
        );

        // Polar alignment initial error

        let alt_error = mount_pole.alt - earth_pole.alt;
        let az_error = mount_pole.az - earth_pole.az;

        log::debug!(
            "PA error alt={}, az={}",
            degree_to_str(radian_to_degree(alt_error)),
            degree_to_str(radian_to_degree(az_error)),
        );

        // Find target point (3rd point with -error adjust)

        let mut target_pt = pt3.coord.clone();
        target_pt.rotate_over_x(-az_error);
        target_pt.rotate_over_y(-alt_error);
        let target_point = HorizCoord::from_sphere_pt(&target_pt);

        log::debug!(
            "Target point alt={}, az={}",
            degree_to_str(radian_to_degree(target_point.alt)),
            degree_to_str(radian_to_degree(target_point.az)),
        );

        self.result = Some(Result {
            earth_pole,
            target_point
        });

        Ok(())
    }

    fn pole_error(&self) -> Option<(HorizCoord, f64)> {
        assert!(self.measurements.len() >= 3);

        let Some(result) = &self.result else {
            return None;
        };

        let third = &self.measurements[2];
        let last = self.measurements.last().unwrap();
        let time_from_third_point = (last.utc_time - third.utc_time).num_milliseconds() as f64 / 1000.0;

        let target = Self::rotate_point_around_pole_with_earth_speed(
            &result.target_point.to_sphere_pt(),
            &result.earth_pole,
            time_from_third_point
        );

        // How we do rotate mount to go from target point to last point
        let changes = coordinate_descent(
            vec![0.0, 0.0],
            PI / (360.0 * 60.0 * 60.0),
            1_000_000,
            |changes| {
                let mut crd = last.coord.clone();
                crd.rotate_over_x(changes[0]);
                crd.rotate_over_y(changes[1]);
                Point3D::angle(&crd, &target).unwrap_or(0.0)
            },
        );
        let horiz_error = HorizCoord {alt: -changes[1], az: -changes[0]};

        // Calculate total error
        let alt_err_pt = HorizCoord { alt: horiz_error.alt, az: 0.0 }.to_sphere_pt();
        let az_err_pt = HorizCoord { alt: 0.0, az: horiz_error.az }.to_sphere_pt();
        let total_error = Point3D::angle(&alt_err_pt, &az_err_pt).unwrap_or_default();

        log::debug!(
            "New PA error alt={}, az={}. Total error={}, time from 3rd point={:.0}s",
            degree_to_str(radian_to_degree(horiz_error.alt)),
            degree_to_str(radian_to_degree(horiz_error.az)),
            degree_to_str(radian_to_degree(total_error)),
            time_from_third_point,
        );

        Some((horiz_error, total_error))
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
        let earth_rotations = time_in_seconds / 86164.09054;
        target.rotate_over_z(2.0 * PI * earth_rotations);
        target.rotate_over_y(pole.alt);
        target.rotate_over_x(pole.az);
        target
    }

}

///////////////////////////////////////////////////////////////////////////////

const AFTER_GOTO_WAIT_TIME: usize = 3; // seconds

pub enum CustomCommand {
    Restart,
    ManualRefresh,
    GetState,
}

#[derive(Clone)]
pub enum State {
    Undefined,
    Goto {
        time:   usize,
        ok_cnt: usize,
        target: EqCoord,
    },
    Capture,
    PlateSolve,
    WaitForManualRefresh,
}

#[derive(PartialEq)]
enum Step {
    Undefined,
    GotoInitialPos,
    First,
    Second,
    Third,
    Corr,
}

#[derive(Clone)]
pub enum PolarAlignmentEvent {
    Empty,
    Error{
        horiz: HorizCoord,
        total: f64,
        step:  usize,
    },
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
    subscribers:  Arc<Events>,
    ps_opts:      PlateSolverOptions,
    plate_solver: PlateSolver,
    alignment:    PolarAlignment,
    image_time:   Option<NaiveDateTime>,
    initial_crd:  Option<EqCoord>,
    step_cnt:     usize,
}

impl PolarAlignMode {
    pub fn check_before_start(
        indi:    &Arc<indi::Connection>,
        options: &Arc<RwLock<Options>>,
    ) -> anyhow::Result<String> {
        const MIN_ALT: f64 = 10.0 ; // in degrees

        let opts = options.read().unwrap();
        let pa_opts = opts.polar_align.clone();
        let mount_device = opts.mount.device.clone();
        let site_opts = opts.site.clone();
        drop(opts);

        let mut warnings = Vec::<String>::new();

        let now = Utc::now().naive_utc();
        let eq_to_sphere_cvt = EqToSphereCvt::new(
            degree_to_radian(site_opts.longitude),
            degree_to_radian(site_opts.latitude),
            &now
        );

        let (init_ra, init_dec) = indi.mount_get_eq_ra_and_dec(&mount_device)?;

        let angle = match pa_opts.direction {
            PloarAlignDir::East => pa_opts.angle,
            PloarAlignDir::West => -pa_opts.angle,
        };

        // 1st point

        let init_eq_crd = EqCoord {
            dec: degree_to_radian(init_dec),
            ra: hour_to_radian(init_ra),
        };

        let init_horiz_crd = HorizCoord::from_sphere_pt(
            &eq_to_sphere_cvt.eq_to_sphere(&init_eq_crd)
        );

        let init_alt_degree = radian_to_degree(init_horiz_crd.alt);

        if init_alt_degree < 0.0 {
            anyhow::bail!(
                "Telescope is pointed under the horizon (altitude = {:.1})°",
                init_alt_degree
            );
        }

        if init_alt_degree < MIN_ALT {
            warnings.push(format!(
                "Altitude is less then {}°. \
                Atmospheric refraction can increase the error",
                MIN_ALT
            ));
        }

        // 2nd point

        let mut mid_ra = init_eq_crd.ra + degree_to_radian(0.5 * angle);
        while mid_ra < 0.0 { mid_ra += 2.0 * PI; }
        while mid_ra >= 2.0 * PI { mid_ra -= 2.0 * PI; }

        let mid_eq_crd = EqCoord {
            dec: init_eq_crd.dec,
            ra: mid_ra,
        };

        let mid_horiz_crd = HorizCoord::from_sphere_pt(
            &eq_to_sphere_cvt.eq_to_sphere(&mid_eq_crd)
        );

        let mid_alt_degree = radian_to_degree(mid_horiz_crd.alt);

        // 3rd point

        let mut final_ra = init_eq_crd.ra + degree_to_radian(angle);
        while final_ra < 0.0 { final_ra += 2.0 * PI; }
        while final_ra >= 2.0 * PI { final_ra -= 2.0 * PI; }

        let final_eq_crd = EqCoord {
            dec: init_eq_crd.dec,
            ra: final_ra,
        };

        let final_horiz_crd = HorizCoord::from_sphere_pt(
            &eq_to_sphere_cvt.eq_to_sphere(&final_eq_crd)
        );

        let final_alt_degree = radian_to_degree(final_horiz_crd.alt);

        if final_alt_degree < 0.0 || mid_alt_degree < 0.0 {
            anyhow::bail!("Telescope will cross the horizon!");
        }

        if final_alt_degree < MIN_ALT {
            warnings.push(format!(
                "Final altitude is less then {}°. \
                Atmospheric refraction can increase the error",
                MIN_ALT
            ));
        }

        if init_horiz_crd.az.is_sign_positive() != final_horiz_crd.az.is_sign_positive() {
            anyhow::bail!("Telescope will cross meridian!");
        }

        Ok(warnings.join("\n"))
    }

    pub fn new(
        indi:        &Arc<indi::Connection>,
        cur_frame:   &Arc<ResultImage>,
        options:     &Arc<RwLock<Options>>,
        subscribers: &Arc<Events>,
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
            cam_device,
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
            image_time:  None,
            initial_crd: None,
            step_cnt:    0,
            cam_opts,
            plate_solver
        })
    }

    fn start_capture(&mut self) -> anyhow::Result<()> {
        apply_camera_options_and_take_shot(
            &self.indi,
            &self.camera,
            &self.cam_opts.frame,
            &self.cam_opts.ctrl
        )?;
        self.image_time = Some(Utc::now().naive_utc());
        Ok(())
    }

    fn get_platesolver_config(&self) -> anyhow::Result<PlateSolveConfig> {
        let (ra, dec) = self.indi.mount_get_eq_ra_and_dec(&self.mount)?;
        let eq_coord = EqCoord {
            dec: degree_to_radian(dec),
            ra:  hour_to_radian(ra),
        };
        let mut config = PlateSolveConfig {
            time_out:       self.ps_opts.timeout,
            blind_time_out: self.ps_opts.blind_timeout,
            eq_coord:       Some(eq_coord),
            .. PlateSolveConfig::default()
        };

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
            Ok(())
        } else {
            anyhow::bail!("{}", err_str);
        }
}

    fn try_process_plate_solving_result(&mut self) -> anyhow::Result<NotifyResult> {
        assert!(matches!(self.state, State::PlateSolve));

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

        let coord = cvt.eq_to_sphere(&ps_result.crd_now);

        // correct atmospheric refraction
        let mut horiz = HorizCoord::from_sphere_pt(&coord);
        horiz.alt += calc_atmospheric_refraction(horiz.alt);
        let mut coord = horiz.to_sphere_pt();

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
            preview: preview.map(Arc::new),
        };
        self.subscribers.notify(Event::PlateSolve(event));

        self.alignment.add_measurement(PolarAlignmentMeasure {
            coord:    coord.clone(),
            utc_time: image_time,
        });

        match self.step {
            Step::First | Step::Second => {
                self.goto_next_pos()?;
                Ok(NotifyResult::ProgressChanges)
            }
            Step::Third => {
                self.alignment.calc_mount_pole(
                    degree_to_radian(self.s_opts.longitude),
                    degree_to_radian(self.s_opts.latitude),
                )?;
                self.step_cnt += 1;
                self.notify_error()?;

                self.step = Step::Corr;
                if self.pa_opts.auto_refresh {
                    self.start_capture()?;
                    self.state = State::Capture;
                } else {
                    self.state = State::WaitForManualRefresh;
                }
                Ok(NotifyResult::ProgressChanges)
            }
            Step::Corr => {
                self.step_cnt += 1;
                self.notify_error()?;
                if self.pa_opts.auto_refresh {
                    self.start_capture()?;
                    self.state = State::Capture;
                } else {
                    self.state = State::WaitForManualRefresh;
                }
                Ok(NotifyResult::ProgressChanges)
            }

            _ => unreachable!(),
        }
    }

    fn goto_next_pos(&mut self) -> anyhow::Result<()> {
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
        self.goto_impl(new_ra, cur_dec)?;
        Ok(())
    }

    fn goto_impl(&mut self, ra: f64, dec: f64) -> anyhow::Result<()> {
        if let Some(slew_speed) = &self.pa_opts.speed {
            self.indi.mount_set_slew_speed(
                &self.mount,
                slew_speed,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }
        self.indi.mount_set_after_coord_action(
            &self.mount,
            indi::AfterCoordSetAction::Track,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
        self.indi.mount_set_eq_coord(
            &self.mount,
            radian_to_hour(ra),
            radian_to_degree(dec),
            true,
            None
        )?;
        self.state = State::Goto {
            time: 0,
            ok_cnt: 0,
            target: EqCoord { dec, ra },
        };
        Ok(())
    }

    fn notify_error(&self) -> anyhow::Result<()> {
        let Some((horiz, total)) = self.alignment.pole_error() else {
            anyhow::bail!("Mount pole is not calculated!");
        };
        self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Error {
            horiz, total, step: self.step_cnt
        }));
        Ok(())
    }

    fn restart(&mut self) -> anyhow::Result<()> {
        if let Some(initial_crd) = self.initial_crd {
            self.abort()?;
            self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Empty));
            self.goto_impl(initial_crd.ra, initial_crd.dec)?;
            self.alignment.clear();
            self.step = Step::GotoInitialPos;
        }
        Ok(())
    }

    fn manual_refresh(&mut self) -> anyhow::Result<()> {
        if !matches!(self.state, State::WaitForManualRefresh) {
            return Ok(());
        }
        self.start_capture()?;
        self.state = State::Capture;
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
            (Step::First,          State::Goto{..}  ) => 2,
            (Step::Second,         State::Capture   ) => 3,
            (Step::Second,         State::PlateSolve) => 4,
            (Step::Second,         State::Goto{..}  ) => 5,
            (Step::Third,          State::Capture   ) => 6,
            (Step::Third,          State::PlateSolve) => 7,
            (Step::Corr,           _                ) => 8,
            (_, State::WaitForManualRefresh         ) => 8,
            _                                         => 0,
        };

        Some(Progress{ cur: step, total: 8 })
    }

    fn progress_string(&self) -> String {
        match (&self.step, &self.state) {
            (Step::GotoInitialPos, _        ) => "Goto initial position",
            (Step::First,  State::Capture   ) => "1st capture",
            (Step::First,  State::PlateSolve) => "1st platesolve",
            (Step::First,  State::Goto{..}  ) => "1st goto",
            (Step::Second, State::Capture   ) => "2nd capture",
            (Step::Second, State::PlateSolve) => "2nd platesolve",
            (Step::Second, State::Goto{..}  ) => "2nd goto",
            (Step::Third,  State::Capture   ) => "3rd capture",
            (Step::Third,  State::PlateSolve) => "3rd platesolve",
            (Step::Corr,   State::Capture   ) => "Capture",
            (Step::Corr,   State::PlateSolve) => "Platesolve",
            (_, State::WaitForManualRefresh ) => "Wait for manual refresh",

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

        self.step_cnt = 0;

        self.initial_crd = Some(EqCoord {
            ra: hour_to_radian(ra),
            dec: degree_to_radian(dec),
        });

        self.subscribers.notify(Event::PolarAlignment(PolarAlignmentEvent::Empty));

        self.start_capture()?;
        self.state = State::Capture;
        self.step = Step::First;
        Ok(())
    }

    fn custom_command(&mut self, args: &dyn Any) -> anyhow::Result<Option<Box<dyn Any>>> {
        let Some(command) = args.downcast_ref::<CustomCommand>() else {
            return Ok(None);
        };

        match command {
            CustomCommand::Restart => {
                self.restart()?;
                self.subscribers.notify(Event::Progress(self.progress(), self.get_type()));
                Ok(None)
            }

            CustomCommand::ManualRefresh => {
                self.manual_refresh()?;
                self.subscribers.notify(Event::Progress(self.progress(), self.get_type()));
                Ok(None)
            }

            CustomCommand::GetState => {
                Ok(Some(Box::new(self.state.clone())))
            }
        }
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
        match &mut self.state {
            State::PlateSolve => {
                return self.try_process_plate_solving_result();
            }

            State::Goto {ok_cnt: goto_ok_cnt, time: goto_time, target: goto_pos} => {
                let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.mount)?;
                if matches!(crd_prop_state, indi::PropState::Ok|indi::PropState::Idle) {
                    *goto_ok_cnt += 1;
                    if *goto_ok_cnt >= AFTER_GOTO_WAIT_TIME {
                        check_telescope_is_at_desired_position(
                            &self.indi,
                            &self.mount,
                            goto_pos,
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

                *goto_time += 1;
                if *goto_time > MAX_GOTO_TIME {
                    anyhow::bail!("Telescope is moving too long time (> {}s)", MAX_GOTO_TIME);
                }
            }

            _ => {},
        }
        Ok(NotifyResult::Empty)
    }
}
