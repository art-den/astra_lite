use std::sync::{Arc, RwLock};
use crate::{core::{consts::*, frame_processing::*}, image::image::Image, indi::{self, value_to_sexagesimal}, options::*, plate_solve::*, ui::sky_map::math::*};
use super::{core::*, utils::*};

const MAX_MOUNT_UNPARK_TIME: usize = 20; // seconds
const MAX_GOTO_TIME: usize = 120; // seconds
const AFTER_GOTO_WAIT_TIME: usize = 3; // seconds

#[derive(PartialEq)]
enum State {
    None,
    WaitingUnparking,
    WaitingGoto,
    WaitingPicture,
    PlateSolving,
    CorrectMount,
    WaitingFinalPicture,
    FinalPlateSolving,
}

pub struct GotoMode {
    state:           State,
    eq_coord:        EqCoord,
    camera:          DeviceAndProp,
    cam_opts:        CamOptions,
    ps_opts:         PlateSolverOptions,
    mount:           String,
    indi:            Arc<indi::Connection>,
    subscribers:     Arc<RwLock<Subscribers>>,
    plate_solver:    Option<PlateSolver>,
    unpark_seconds:  usize,
    goto_seconds:    usize,
    goto_ok_seconds: usize,
}

impl GotoMode {
    pub fn new(
        eq_coord:    &EqCoord,
        options:     &Arc<RwLock<Options>>,
        indi:        &Arc<indi::Connection>,
        subscribers: &Arc<RwLock<Subscribers>>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(camera) = opts.cam.device.clone() else {
            anyhow::bail!("Camera is not selected");
        };
        let mut cam_opts = opts.cam.clone();
        cam_opts.frame.frame_type = crate::image::raw::FrameType::Lights;
        cam_opts.frame.exp_main = opts.plate_solver.exposure;
        cam_opts.frame.gain = gain_to_value(
            opts.plate_solver.gain,
            opts.cam.frame.gain,
            &camera,
            indi
        )?;

        Ok(Self {
            state: State::None,
            eq_coord: eq_coord.clone(),
            camera,
            cam_opts,
            ps_opts: opts.plate_solver.clone(),
            mount:   opts.mount.device.clone(),
            indi:    Arc::clone(indi),
            subscribers: Arc::clone(subscribers),
            plate_solver: None,
            unpark_seconds: 0,
            goto_seconds: 0,
            goto_ok_seconds: 0,
        })
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
        self.state = State::WaitingUnparking;
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
        log::debug!("Tacking picture for plate solve with {:?}", &self.cam_opts.frame);
        init_cam_continuous_mode(&self.indi, &self.camera, &self.cam_opts.frame, false)?;
        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
        Ok(())
    }

    fn plate_solve_image(&mut self, image: &Arc<RwLock<Image>>) -> anyhow::Result<()> {
        let image = image.read().unwrap();
        let mut plate_solver = PlateSolver::new(self.ps_opts.solver);
        let mut config = PlateSolveConfig::default();
        config.eq_coord = Some(self.eq_coord.clone());
        config.time_out = self.ps_opts.timeout;
        plate_solver.start(&image, &config)?;
        drop(image);
        self.plate_solver = Some(plate_solver);
        Ok(())
    }

    fn try_process_plate_solving_result(&mut self) -> anyhow::Result<bool> {
        let Some(plate_solver) = &mut self.plate_solver else {
            return Ok(false);
        };
        let result = match plate_solver.get_result() {
            Some(result) => result?,
            None => return Ok(false),
        };

        log::debug!(
            "plate solver j2000 = (ra: {}, dec: {}), now = (ra: {}, dec: {}), image size = {:.6} x {:.6}",
            value_to_sexagesimal(radian_to_hour(result.crd_j2000.ra), true, 9),
            value_to_sexagesimal(radian_to_degree(result.crd_j2000.dec), true, 8),
            value_to_sexagesimal(radian_to_hour(result.crd_now.ra), true, 9),
            value_to_sexagesimal(radian_to_degree(result.crd_now.dec), true, 8),
            radian_to_hour(result.width),
            radian_to_degree(result.height),
        );

        let event = PlateSolverEvent {
            cam_name: self.camera.name.clone(),
            result: result.clone(),
        };
        self.subscribers.read().unwrap().inform_event(
            CoreEvent::PlateSolve(event)
        );

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

        Ok(true)
    }
}

impl Mode for GotoMode {
    fn get_type(&self) -> ModeType {
        ModeType::Goto
    }

    fn progress_string(&self) -> String {
        match self.state {
            State::None => "Goto...".to_string(),
            State::WaitingUnparking => "Unpark mount".to_string(),
            State::WaitingGoto => "Goto coordinate".to_string(),
            State::WaitingPicture => "Tacking picture".to_string(),
            State::PlateSolving => "Plate solving".to_string(),
            State::CorrectMount => "Mount correction".to_string(),
            State::WaitingFinalPicture => "Tacking final picture".to_string(),
            State::FinalPlateSolving => "Final plate solving".to_string(),
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn cam_opts(&self) -> Option<&CamOptions> {
        Some(&self.cam_opts)
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exposure())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.indi.mount_get_parked(&self.mount)? {
            self.start_unpark_telescope()?;
        } else {
            self.start_goto_coord()?;
            self.state = State::WaitingGoto;
        }
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        abort_camera_exposure(&self.indi, &self.camera)?;
        self.state = State::None;
        Ok(())
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        match self.state {
            State::WaitingUnparking => {
                if !self.indi.mount_get_parked(&self.mount)? {
                    self.start_goto_coord()?;
                    self.state = State::WaitingGoto;
                    return Ok(NotifyResult::ModeStrChanged);
                }
                self.unpark_seconds += 1;
                if self.unpark_seconds > MAX_MOUNT_UNPARK_TIME {
                    anyhow::bail!(
                        "Mount unpark time out (> {} seconds)!",
                        MAX_MOUNT_UNPARK_TIME
                    );
                }
            }

            State::WaitingGoto | State::CorrectMount => {
                let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.mount)?;
                if crd_prop_state == indi::PropState::Ok {
                    self.goto_ok_seconds += 1;
                    if self.goto_ok_seconds >= AFTER_GOTO_WAIT_TIME {
                        self.start_take_picture()?;
                        if self.state == State::WaitingGoto {
                            self.state = State::WaitingPicture;
                        } else {
                            self.state = State::WaitingFinalPicture;
                        }
                        return Ok(NotifyResult::ModeStrChanged);
                    }
                } else {
                    self.goto_seconds += 1;
                    if self.goto_seconds > MAX_GOTO_TIME {
                        anyhow::bail!(
                            "Mount goto time out (> {} seconds)!",
                            MAX_GOTO_TIME
                        );
                    }
                }
            }

            State::PlateSolving => {
                let ok = self.try_process_plate_solving_result()?;
                if ok {
                    self.start_goto_coord()?;
                    self.state = State::CorrectMount;
                    return Ok(NotifyResult::ModeStrChanged)
                }
            }

            State::FinalPlateSolving => {
                let ok = self.try_process_plate_solving_result()?;
                if ok {
                    self.start_take_picture()?;
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

        if let FrameProcessResultData::Image(image) = &fp_result.data {
            match self.state {
                State::WaitingPicture => {
                    self.plate_solve_image(image)?;
                    self.state = State::PlateSolving;
                    return Ok(NotifyResult::ModeStrChanged);
                }

                State::WaitingFinalPicture => {
                    self.plate_solve_image(image)?;
                    self.state = State::FinalPlateSolving;
                    return Ok(NotifyResult::ModeStrChanged);
                }

                _ => {},
            }
        }

        Ok(NotifyResult::Empty)
    }

}