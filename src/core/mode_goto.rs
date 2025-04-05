use std::sync::{Arc, RwLock};
use crate::{
    core::{consts::*, events::*, frame_processing::*},
    image::{image::Image, info::LightFrameInfo, stars::StarItems},
    indi,
    options::*,
    plate_solve::*,
    ui::sky_map::math::*
};
use super::{core::*, events::EventSubscriptions, utils::*};

const MAX_MOUNT_UNPARK_TIME: usize = 20; // seconds

#[derive(PartialEq)]
enum State {
    None,
    ImagePlateSolving,
    Unparking,
    Goto,
    TackingPicture,
    PlateSolving,
    CorrectMount,
    TackingFinalPicture,
    FinalPlateSolving,
    Finished,
}

pub enum GotoDestination {
    Image{
        image: Arc<RwLock<Image>>,
        info:  Arc<LightFrameInfo>,
        stars: Arc<StarsInfoData>,
    },
    Coord(EqCoord)
}

#[derive(PartialEq)]
pub enum GotoConfig {
    OnlyGoto,
    GotoPlateSolveAndCorrect,
}

pub struct GotoMode {
    state:           State,
    destination:     GotoDestination,
    config:          GotoConfig,
    eq_coord:        EqCoord,
    camera:          Option<DeviceAndProp>,
    cam_opts:        Option<CamOptions>,
    ps_opts:         PlateSolverOptions,
    mount:           String,
    indi:            Arc<indi::Connection>,
    cur_frame:       Arc<ResultImage>,
    options:         Arc<RwLock<Options>>,
    subscribers:     Arc<EventSubscriptions>,
    plate_solver:    Option<PlateSolver>,
    unpark_seconds:  usize,
    goto_seconds:    usize,
    goto_ok_seconds: usize,
    extra_stages:    usize,
}

impl GotoMode {
    pub fn new(
        destination: GotoDestination,
        config:      GotoConfig,
        options:     &Arc<RwLock<Options>>,
        indi:        &Arc<indi::Connection>,
        cur_frame:   &Arc<ResultImage>,
        subscribers: &Arc<EventSubscriptions>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let (camera, cam_opts, plate_solver) = if config == GotoConfig::GotoPlateSolveAndCorrect {
            let Some(camera) = opts.cam.device.clone() else {
                anyhow::bail!("Camera is not selected!");
            };
            let mut cam_opts = opts.cam.clone();
            cam_opts.frame.frame_type = crate::image::raw::FrameType::Lights;
            cam_opts.frame.exp_main = opts.plate_solver.exposure;
            cam_opts.frame.binning = opts.plate_solver.bin;
            cam_opts.frame.gain = gain_to_value(
                opts.plate_solver.gain,
                opts.cam.frame.gain,
                &camera,
                indi
            )?;
            let plate_solver = PlateSolver::new(opts.plate_solver.solver);

            (Some(camera), Some(cam_opts), Some(plate_solver))
        } else {
            (None, None, None)
        };

        Ok(Self {
            state:           State::None,
            config,
            eq_coord:        EqCoord::default(),
            ps_opts:         opts.plate_solver.clone(),
            mount:           opts.mount.device.clone(),
            indi:            Arc::clone(indi),
            cur_frame:       Arc::clone(cur_frame),
            options:         Arc::clone(options),
            subscribers:     Arc::clone(subscribers),
            unpark_seconds:  0,
            goto_seconds:    0,
            goto_ok_seconds: 0,
            extra_stages:    0,
            plate_solver,
            destination,
            camera,
            cam_opts,
        })
    }

    fn start_goto(&mut self) -> anyhow::Result<()> {
        if self.indi.mount_get_parked(&self.mount)? {
            self.start_unpark_telescope()?;
        } else {
            self.start_goto_coord()?;
            self.state = State::Goto;
        }
        Ok(())
    }

    fn start_unpark_telescope(&mut self) -> anyhow::Result<()> {
        log::debug!("Unparking mount...");
        self.indi.mount_set_parked(
            &self.mount,
            false,
            true,
            None
        )?;
        self.unpark_seconds = 0;
        self.state = State::Unparking;
        Ok(())
    }

    fn start_goto_coord(&mut self) -> anyhow::Result<()> {
        log::debug!(
            "Goto {}, {} ...",
            indi::value_to_sexagesimal(self.eq_coord.ra, true, 9),
            indi::value_to_sexagesimal(self.eq_coord.dec, true, 8)
        );
        self.indi.set_after_coord_set_action(
            &self.mount,
            indi::AfterCoordSetAction::Track,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;

        self.indi.mount_set_eq_coord(
            &self.mount,
            radian_to_hour(self.eq_coord.ra),
            radian_to_degree(self.eq_coord.dec),
            true,
            None
        )?;
        self.goto_seconds = 0;
        self.goto_ok_seconds = 0;
        Ok(())
    }

    fn start_take_picture(&mut self) -> anyhow::Result<()> {
        let cam_opts = self.cam_opts.as_ref().unwrap();
        let camera = self.camera.as_ref().unwrap();

        log::debug!("Tacking picture for plate solve with {:?}", &cam_opts.frame);
        apply_camera_options_and_take_shot(&self.indi, camera, &cam_opts.frame)?;
        Ok(())
    }

    fn plate_solve_image(&mut self, image: &Arc<RwLock<Image>>) -> anyhow::Result<()> {
        let plate_solver = self.plate_solver.as_mut().unwrap();
        let image = image.read().unwrap();
        let mut config = PlateSolveConfig::default();
        config.eq_coord = Some(self.eq_coord.clone());
        config.time_out = self.ps_opts.timeout;
        config.blind_time_out = self.ps_opts.blind_timeout;
        plate_solver.start(&PlateSolverInData::Image(&image), &config)?;
        drop(image);
        Ok(())
    }

    fn plate_solve_stars(
        &mut self,
        stars:      &StarItems,
        img_width:  usize,
        img_height: usize
    ) -> anyhow::Result<()> {
        let plate_solver = self.plate_solver.as_mut().unwrap();
        let mut config = PlateSolveConfig::default();
        config.eq_coord = Some(self.eq_coord.clone());
        config.time_out = self.ps_opts.timeout;
        config.blind_time_out = self.ps_opts.blind_timeout;
        let stars_arg = PlateSolverInData::Stars{
            stars,
            img_width,
            img_height,
        };
        plate_solver.start(&stars_arg, &config)?;
        Ok(())
    }

    fn try_process_plate_solving_result(
        &mut self,
        action: ProcessPlateSolverResultAction,
    ) -> anyhow::Result<bool> {
        let plate_solver = self.plate_solver.as_mut().unwrap();
        let camera = self.camera.as_ref().unwrap();

        let result = match plate_solver.get_result()? {
            PlateSolveResult::Waiting => return Ok(false),
            PlateSolveResult::Done(result) => result,
            PlateSolveResult::Failed => anyhow::bail!("Can't platesolve image")
        };

        result.print_to_log();

        // Image for preview in map

        let options = self.options.read().unwrap();
        let preview = self.cur_frame.create_preview_for_platesolve_image(&options.preview);
        drop(options);

        let event = PlateSolverEvent {
            cam_name: camera.name.clone(),
            result: result.clone(),
            preview: preview.map(|p| Arc::new(p)),
        };
        self.subscribers.notify(
            Event::PlateSolve(event)
        );

        match action {
            ProcessPlateSolverResultAction::Sync => {
                self.indi.set_after_coord_set_action(
                    &self.mount,
                    indi::AfterCoordSetAction::Sync,
                    true,
                    INDI_SET_PROP_TIMEOUT
                )?;

                self.indi.mount_set_eq_coord(
                    &self.mount,
                    radian_to_hour(result.crd_now.ra),
                    radian_to_degree(result.crd_now.dec),
                    true,
                    INDI_SET_PROP_TIMEOUT
                )?;
            }
            ProcessPlateSolverResultAction::SetEqCoord => {
                self.eq_coord = result.crd_now.clone();
            }
        }
        Ok(true)

    }
}

impl Mode for GotoMode {
    fn get_type(&self) -> ModeType {
        ModeType::Goto
    }

    fn progress_string(&self) -> String {
        match self.state {
            State::ImagePlateSolving =>
                "Custom image plate solving".to_string(),
            State::Unparking =>
                "Unpark mount".to_string(),
            State::Goto =>
                "Goto coordinate".to_string(),
            State::TackingPicture =>
                "Tacking picture".to_string(),
            State::PlateSolving =>
                "Plate solving".to_string(),
            State::CorrectMount =>
                "Mount correction".to_string(),
            State::TackingFinalPicture =>
                "Tacking final picture".to_string(),
            State::FinalPlateSolving =>
                "Final plate solving".to_string(),
            State::None|State::Finished =>
                "Goto and platesolve".to_string(),
        }
    }

    fn progress(&self) -> Option<Progress> {
        if self.config == GotoConfig::OnlyGoto {
            return None;
        }

        let mut stage = match self.state {
            State::None => return None,
            State::ImagePlateSolving => -1,
            State::Unparking => 0,
            State::Goto => 0,
            State::TackingPicture => 1,
            State::PlateSolving => 2,
            State::CorrectMount => 3,
            State::TackingFinalPicture => 4,
            State::FinalPlateSolving => 5,
            State::Finished => 6,
        };

        stage += self.extra_stages as i32;
        if stage >= 0 {
            Some(Progress {
                cur: stage as usize,
                total: self.extra_stages + 6
            })
        } else {
            None
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        match self.config {
            GotoConfig::GotoPlateSolveAndCorrect =>
                self.camera.as_ref(),
            GotoConfig::OnlyGoto =>
                None,
        }
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        self.cam_opts.as_ref().map(|cam_opts| cam_opts.frame.exposure())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        match &self.destination {
            GotoDestination::Coord(coord) => {
                self.extra_stages = 0;
                self.eq_coord = coord.clone();
                self.start_goto()?;
            }
            GotoDestination::Image{image, info, stars} => {
                let plate_solver = self.plate_solver.as_mut().unwrap();

                self.extra_stages = 1;
                let mut config = PlateSolveConfig::default();
                config.time_out = self.ps_opts.timeout;
                config.blind_time_out = self.ps_opts.blind_timeout;
                if plate_solver.support_stars_as_input() {
                    plate_solver.start(
                        &PlateSolverInData::Stars{
                            stars: &stars.items,
                            img_width: info.width,
                            img_height: info.height,
                        },
                        &config
                    )?;
                } else {
                    let image = image.read().unwrap();
                    plate_solver.start(
                        &PlateSolverInData::Image(&image),
                        &config
                    )?;
                };
                self.state = State::ImagePlateSolving;
            }
        }

        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        if let Some(camera) = &self.camera {
            _ = abort_camera_exposure(&self.indi, camera);
        }
        _ = self.indi.mount_abort_motion(&self.mount);
        self.state = State::None;
        Ok(())
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        match self.state {
            State::Unparking => {
                if !self.indi.mount_get_parked(&self.mount)? {
                    self.start_goto_coord()?;
                    self.state = State::Goto;
                    return Ok(NotifyResult::ProgressChanges);
                }
                self.unpark_seconds += 1;
                if self.unpark_seconds > MAX_MOUNT_UNPARK_TIME {
                    anyhow::bail!(
                        "Mount unpark time out (> {} seconds)!",
                        MAX_MOUNT_UNPARK_TIME
                    );
                }
            }

            State::Goto | State::CorrectMount => {
                let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.mount)?;
                if crd_prop_state == indi::PropState::Ok {
                    self.goto_ok_seconds += 1;
                    if self.goto_ok_seconds >= AFTER_GOTO_WAIT_TIME {
                        check_telescope_is_at_desired_position(
                            &self.indi,
                            &self.mount,
                            &self.eq_coord,
                            0.5
                        )?;
                        if self.state == State::Goto {
                            if self.config == GotoConfig::OnlyGoto {
                                return Ok(NotifyResult::Finished { next_mode: None });
                            }
                            self.start_take_picture()?;
                            self.state = State::TackingPicture;
                        } else {
                            self.start_take_picture()?;
                            self.state = State::TackingFinalPicture;
                        }
                        return Ok(NotifyResult::ProgressChanges);
                    }
                } else {
                    self.goto_seconds += 1;
                    if self.goto_seconds > MAX_GOTO_TIME {
                        anyhow::bail!("Telescope is moving too long time (> {}s)", MAX_GOTO_TIME);
                    }
                }
            }

            State::ImagePlateSolving => {
                let ok = self.try_process_plate_solving_result(
                    ProcessPlateSolverResultAction::SetEqCoord
                )?;
                if ok {
                    self.start_goto()?;
                    return Ok(NotifyResult::ProgressChanges)
                }
            }

            State::PlateSolving => {
                let ok = self.try_process_plate_solving_result(
                    ProcessPlateSolverResultAction::Sync
                )?;
                if ok {
                    self.start_goto_coord()?;
                    self.state = State::CorrectMount;
                    return Ok(NotifyResult::ProgressChanges)
                }
            }

            State::FinalPlateSolving => {
                let ok = self.try_process_plate_solving_result(
                    ProcessPlateSolverResultAction::Sync
                )?;
                if ok {
                    self.state = State::Finished;
                    return Ok(NotifyResult::Finished { next_mode: None })
                }
            }

            _ => {},
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result:  &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        let plate_solver = self.plate_solver.as_mut().unwrap();
        let xy_supported = plate_solver.support_stars_as_input();
        match (&self.state, &fp_result.data, xy_supported) {
            (State::TackingPicture, FrameProcessResultData::Image(image), false) => {
                self.plate_solve_image(image)?;
                self.state = State::PlateSolving;
                return Ok(NotifyResult::ProgressChanges);
            }
            (State::TackingPicture, FrameProcessResultData::LightFrameInfo(info), true) => {
                self.plate_solve_stars(&info.stars.items, info.image.width, info.image.height)?;
                self.state = State::PlateSolving;
                return Ok(NotifyResult::ProgressChanges);
            }
            (State::TackingFinalPicture, FrameProcessResultData::Image(image), false) => {
                self.plate_solve_image(image)?;
                self.state = State::FinalPlateSolving;
                return Ok(NotifyResult::ProgressChanges);
            }
            (State::TackingFinalPicture, FrameProcessResultData::LightFrameInfo(info), true) => {
                self.plate_solve_stars(&info.stars.items, info.image.width, info.image.height)?;
                self.state = State::FinalPlateSolving;
                return Ok(NotifyResult::ProgressChanges);
            }
            _ => {},
        }

        Ok(NotifyResult::Empty)
    }

}

enum ProcessPlateSolverResultAction {
    Sync,
    SetEqCoord,
}
