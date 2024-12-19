use std::sync::{Arc, RwLock};

use crate::{core::{consts::INDI_SET_PROP_TIMEOUT, core::*, frame_processing::*}, image::{image::*, stars::Stars}, indi, options::*, plate_solve::*, ui::sky_map::math::*};

use super::{events::*, utils::gain_to_value};

enum State {
    None,
    Capturing,
    PlateSolve,
    Finished,
}

pub struct CapturePlatesolveMode {
    state:        State,
    indi:         Arc<indi::Connection>,
    subscribers:  Arc<EventSubscriptions>,
    camera:       DeviceAndProp,
    mount:        String,
    cam_opts:     CamOptions,
    ps_opts:      PlateSolverOptions,
    plate_solver: PlateSolver,
}

impl CapturePlatesolveMode {
    pub fn new(
        options:     &Arc<RwLock<Options>>,
        indi:        &Arc<indi::Connection>,
        subscribers: &Arc<EventSubscriptions>,
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(camera) = opts.cam.device.clone() else {
            anyhow::bail!("Camera is not selected");
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
        Ok(Self {
            state:        State::None,
            indi:         Arc::clone(indi),
            subscribers:  Arc::clone(subscribers),
            mount:        opts.mount.device.clone(),
            ps_opts:      opts.plate_solver.clone(),
            plate_solver,
            camera,
            cam_opts,
        })
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
        stars:      &Stars,
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

    fn try_process_plate_solving_result(&mut self) -> anyhow::Result<bool> {
        let result = match self.plate_solver.get_result()? {
            PlateSolveResult::Waiting => return Ok(false),
            PlateSolveResult::Done(result) => result,
            PlateSolveResult::Failed => anyhow::bail!("Can't platesolve image")
        };

        result.print_to_log();

        let event = PlateSolverEvent {
            cam_name: self.camera.name.clone(),
            result: result.clone(),
        };
        self.subscribers.notify(
            Event::PlateSolve(event)
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

impl Mode for CapturePlatesolveMode {
    fn get_type(&self) -> ModeType {
        ModeType::CapturePlatesolve
    }

    fn progress_string(&self) -> String {
        match self.state {
            State::Capturing =>
                "Capturing image".to_string(),
            State::PlateSolve =>
                "Platesolving...".to_string(),
            State::None|State::Finished =>
                "Capture, platesolve & sync".to_string(),
        }
    }

    fn progress(&self) -> Option<Progress> {
        let stage = match self.state {
            State::None       => 0,
            State::Capturing  => 0,
            State::PlateSolve => 1,
            State::Finished   => 2,
        };
        Some(Progress { cur: stage, total: 2 })
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exp_main)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        log::debug!("Tacking picture for plate solve with {:?}", &self.cam_opts.frame);
        apply_camera_options_and_take_shot(&self.indi, &self.camera, &self.cam_opts.frame)?;
        self.state = State::Capturing;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        _ = abort_camera_exposure(&self.indi, &self.camera);
        _ = self.indi.mount_abort_motion(&self.mount);
        self.state = State::None;
        Ok(())
    }

    fn notify_about_frame_processing_result(
        &mut self,
        fp_result: &FrameProcessResult
    ) -> anyhow::Result<NotifyResult> {
        let xy_supported = self.plate_solver.support_stars_as_input();
        match (&self.state, &fp_result.data, xy_supported) {
            (State::Capturing, FrameProcessResultData::Image(image), false) => {
                self.plate_solve_image(image)?;
                self.state = State::PlateSolve;
                return Ok(NotifyResult::ProgressChanges);
            }
            (State::Capturing, FrameProcessResultData::LightFrameInfo(info), true) => {
                self.plate_solve_stars(&info.stars.items, info.width, info.height)?;
                self.state = State::PlateSolve;
                return Ok(NotifyResult::ProgressChanges);
            }
            _ => {},
        }

        Ok(NotifyResult::Empty)
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        match self.state {
            State::PlateSolve => {
                let ok = self.try_process_plate_solving_result()?;
                if ok {
                    self.state = State::Finished;
                    return Ok(NotifyResult::Finished { next_mode: None });
                }
            }
            _ => {},
        }
        Ok(NotifyResult::Empty)
    }
}