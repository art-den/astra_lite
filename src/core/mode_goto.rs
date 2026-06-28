use std::sync::{Arc, RwLock};
use crate::{
    core::{cam_ctrl::take_shot, consts::*, events::*, frame_processing::*},
    hal::{Camera, FrameType, Telescope, indi::value_to_sexagesimal},
    image::{image::Image, info::LightFrameInfo, stars::StarItems, stars_offset::Point},
    options::*,
    plate_solve::*,
    sky_math::math::*,
};
use super::{core::*, events::EventHandlers, utils::*};

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
    Checking,
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

enum ProcessPlateSolverResultAction {
    Sync,
    SetEqCoord,
}

pub struct GotoMode {
    camera:       Option<Arc<dyn Camera + Send + Sync>>,
    telescope:    Arc<dyn Telescope + Send + Sync>,
    state:        State,
    destination:  GotoDestination,
    config:       GotoConfig,
    eq_coord:     EqCoord,
    cam_opts:     Option<CamOptions>,
    ps_opts:      PlateSolverOptions,
    cur_frame:    Arc<ResultImage>,
    options:      Arc<RwLock<Options>>,
    subscribers:  Arc<EventHandlers>,
    plate_solver: Option<PlateSolver>,
    unpark_ms:    usize,
    goto_ms:      usize,
    goto_ok_ms:   usize,
    extra_stages: usize,
}

impl GotoMode {
    pub fn new(
        core:        &Core,
        destination: GotoDestination,
        config:      GotoConfig
    ) -> eyre::Result<Self> {
        let opts = core.options().read().unwrap();
        let telescope = core.cur_devices.telescope_or_err()?;
        let (camera, cam_opts, plate_solver) = if config == GotoConfig::GotoPlateSolveAndCorrect {
            let camera = core.cur_devices.camera_or_err()?;
            let mut cam_opts = opts.cam.clone();
            cam_opts.frame.frame_type = FrameType::Lights;
            cam_opts.frame.exp_main = opts.plate_solver.exposure;
            cam_opts.frame.binning = opts.plate_solver.bin;
            cam_opts.frame.gain = gain_to_value(
                opts.plate_solver.gain,
                opts.cam.frame.gain,
                camera.gain_range()?
            );
            let plate_solver = PlateSolver::new(opts.plate_solver.solver);

            (Some(camera), Some(cam_opts), Some(plate_solver))
        } else {
            (None, None, None)
        };

        Ok(Self {
            state:        State::None,
            eq_coord:     EqCoord::default(),
            ps_opts:      opts.plate_solver.clone(),
            cur_frame:    Arc::clone(core.cur_frame()),
            options:      Arc::clone(core.options()),
            subscribers:  Arc::clone(core.events()),
            unpark_ms:    0,
            goto_ms:      0,
            goto_ok_ms:   0,
            extra_stages: 0,
            config,
            plate_solver,
            destination,
            camera,
            telescope,
            cam_opts,
        })
    }

    fn start_goto(&mut self) -> eyre::Result<()> {
        if self.telescope.is_parked()? {
            self.start_unpark_telescope()?;
        } else {
            self.start_goto_coord()?;
            self.state = State::Goto;
        }
        Ok(())
    }

    fn start_unpark_telescope(&mut self) -> eyre::Result<()> {
        log::debug!("Unparking mount...");
        self.telescope.unpark()?;
        self.unpark_ms = 0;
        self.state = State::Unparking;
        Ok(())
    }

    fn start_goto_coord(&mut self) -> eyre::Result<()> {
        log::debug!(
            "Goto {}, {} ...",
            value_to_sexagesimal(self.eq_coord.ra, true, 9),
            value_to_sexagesimal(self.eq_coord.dec, true, 8)
        );
        self.telescope.goto_and_track(
            radian_to_hour(self.eq_coord.ra),
            radian_to_degree(self.eq_coord.dec)
        )?;
        self.goto_ms = 0;
        self.goto_ok_ms = 0;
        Ok(())
    }

    fn start_take_picture(&mut self) -> eyre::Result<()> {
        let cam_opts = self.cam_opts.as_ref().unwrap();
        let camera = self.camera.as_ref().unwrap();

        log::debug!("Tacking picture for plate solve with {:?}", &cam_opts.frame);
        take_shot(camera, &cam_opts.frame, &cam_opts.ctrl)?;
        Ok(())
    }

    fn plate_solve_image(&mut self, image: &Arc<RwLock<Image>>) -> eyre::Result<()> {
        let plate_solver = self.plate_solver.as_mut().unwrap();
        let image = image.read().unwrap();
        let config = PlateSolveConfig {
            eq_coord:       Some(self.eq_coord),
            time_out:       self.ps_opts.timeout,
            blind_time_out: self.ps_opts.blind_timeout,
            .. PlateSolveConfig::default()
        };

        plate_solver.start(&PlateSolverInData::Image(&image), &config)?;
        drop(image);
        Ok(())
    }

    fn plate_solve_stars(
        &mut self,
        stars:      &StarItems,
        img_width:  usize,
        img_height: usize
    ) -> eyre::Result<()> {
        let plate_solver = self.plate_solver.as_mut().unwrap();
        let config = PlateSolveConfig {
            eq_coord:       Some(self.eq_coord),
            time_out:       self.ps_opts.timeout,
            blind_time_out: self.ps_opts.blind_timeout,
            .. PlateSolveConfig::default()
        };
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
    ) -> eyre::Result<bool> {
        let Some(plate_solver) = self.plate_solver.as_mut() else {
            eyre::bail!("self.plate_solver is None");
        };
        let Some(camera) = self.camera.as_mut() else {
            eyre::bail!("self.camera is None");
        };

        let result = match plate_solver.get_result()? {
            PlateSolveResult::Waiting => return Ok(false),
            PlateSolveResult::Done(result) => result,
            PlateSolveResult::Failed => eyre::bail!("Can't platesolve image")
        };

        result.print_to_log();

        // Image for preview in map

        let options = self.options.read().unwrap();
        let preview = self.cur_frame.create_preview_for_platesolve_image(&options.preview);
        drop(options);

        let event = PlateSolverEvent {
            cam_name: camera.name().to_string(),
            result:   result.clone(),
            preview:  preview.map(Arc::new),
        };
        self.subscribers.send(
            Event::PlateSolve(event)
        );

        match action {
            ProcessPlateSolverResultAction::Sync => {
                self.telescope.sync(
                    radian_to_hour(result.crd_now.ra),
                    radian_to_degree(result.crd_now.dec)
                )?;
            }
            ProcessPlateSolverResultAction::SetEqCoord => {
                self.eq_coord = result.crd_now;
            }
        }
        Ok(true)
    }

    fn show_overlay_message(&self, info: &LightFrameInfoData) {
        let message = if let Some(offset) = &info.offset {
            format!(
                "Offset x={:.1}, y={:.1}\nRotation = {:.2}°",
                offset.x,
                offset.y,
                radian_to_degree(offset.angle),
            )
        } else {
            "???".to_string()
        };

        self.subscribers.send(Event::OverlayMessage {
            pos: OverlayMessgagePos::Top,
            text: Arc::new(message)
        });
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
            State::Checking =>
                "Checking position".to_string(),
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
            State::Finished|State::Checking => 6,
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

    fn camera_id(&self) -> Option<&str> {
        self.camera.as_ref().map(|c| c.id())
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        self.cam_opts
            .as_ref()
            .map(|cam_opts| cam_opts.frame.exposure())
    }

    fn start(&mut self) -> eyre::Result<()> {
        match &self.destination {
            GotoDestination::Coord(coord) => {
                self.extra_stages = 0;
                self.eq_coord = *coord;
                self.start_goto()?;
            }
            GotoDestination::Image{image, info, stars} => {
                let plate_solver = self.plate_solver.as_mut().unwrap();
                self.extra_stages = 1;
                let mut config = PlateSolveConfig {
                    time_out:       self.ps_opts.timeout,
                    blind_time_out: self.ps_opts.blind_timeout,
                    .. PlateSolveConfig::default()
                };
                let image = image.read().unwrap();
                if let Some(raw_info) = &image.raw_info
                && let (Some(dec), Some(ra)) = (raw_info.dec, raw_info.ra) {
                    config.eq_coord_j2000 = Some(EqCoord {
                        dec: degree_to_radian(dec),
                        ra: degree_to_radian(ra),
                    });
                }
                let plate_solver_input = if plate_solver.support_stars_as_input() {
                    &PlateSolverInData::Stars{
                        stars: &stars.items,
                        img_width: info.width,
                        img_height: info.height,
                    }
                } else {
                    &PlateSolverInData::Image(&image)
                };
                plate_solver.start(plate_solver_input, &config)?;
                self.state = State::ImagePlateSolving;
            }
        }

        Ok(())
    }

    fn abort(&mut self) -> eyre::Result<()> {
        if let Some(camera) = &self.camera {
            _ = camera.abort_exposure();
        }
        _ = self.telescope.abort_motion();

        self.state = State::None;

        self.subscribers.send(Event::OverlayMessage {
            pos: OverlayMessgagePos::Top,
            text: Arc::new(String::new())
        });

        Ok(())
    }

    fn frame_options_to_restart_exposure(&self) -> Option<&FrameOptions> {
        self.cam_opts.as_ref().map(|cam_opts| &cam_opts.frame)
    }

    fn notify_periodical_timer_tick(&mut self, timer_period_ms: usize) -> eyre::Result<NotifyResult> {
        match self.state {
            State::Unparking => {
                if !self.telescope.is_parked()? {
                    self.start_goto_coord()?;
                    self.state = State::Goto;
                    return Ok(NotifyResult::ProgressChanges);
                }
                self.unpark_ms += timer_period_ms;
                if self.unpark_ms > MAX_MOUNT_UNPARK_TIME * 1000 {
                    eyre::bail!(
                        "Mount unpark time out (> {} seconds)!",
                        MAX_MOUNT_UNPARK_TIME
                    );
                }
            }

            State::Goto | State::CorrectMount => {
                if !self.telescope.is_slewing()? {
                    self.goto_ok_ms += timer_period_ms;
                    if self.goto_ok_ms >= AFTER_GOTO_WAIT_TIME * 1000 {
                        let (cur_ra, cur_dec) = self.telescope.eq_coord()?;
                        check_telescope_is_at_desired_position(
                            cur_ra,
                            cur_dec,
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
                    self.goto_ms += timer_period_ms;
                    if self.goto_ms > MAX_GOTO_TIME * 1000 {
                        eyre::bail!("Telescope is moving too long time (> {}s)", MAX_GOTO_TIME);
                    }
                }
            }

            State::ImagePlateSolving => {
                let ok = self.try_process_plate_solving_result(
                    ProcessPlateSolverResultAction::SetEqCoord
                )?;
                if ok {
                    self.plate_solver.as_mut().unwrap().reset(); // reset optimization gotten from image
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
                    if matches!(&self.destination, GotoDestination::Image {.. }) {
                        self.start_take_picture()?;
                        self.state = State::Checking;
                    } else {
                        self.state = State::Finished;
                        return Ok(NotifyResult::Finished { next_mode: None })
                    }
                }
            }

            _ => {},
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result:  &FrameProcessResult
    ) -> eyre::Result<NotifyResult> {
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
            (State::Checking, FrameProcessResultData::LightFrameInfo(info), true) => {
                self.show_overlay_message(info);
                self.start_take_picture()?;
                return Ok(NotifyResult::ProgressChanges);
            }

            _ => {},
        }

        Ok(NotifyResult::Empty)
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        if let GotoDestination::Image { stars, .. } = &self.destination {
            let ref_stars = stars.items.iter().map(|s| Point { x: s.x, y: s.y }).collect();
            cmd.ref_stars = Some(ref_stars);
        }
    }
}
