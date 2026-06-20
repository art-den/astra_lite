use std::sync::{Arc, RwLock};

use crate::{
    core::{
        cam_ctrl::take_shot,
        core::*,
        frame_processing::*,
    },
    hal::{Camera, CcdPurpose, FrameType, Hal, Telescope},
    image::{image::*, stars::StarItems},
    options::*,
    plate_solve::*,
    sky_math::math::*,
};

use super::{events::*, utils::gain_to_value};

#[derive(PartialEq)]
enum State {
    None,
    Capturing,
    PlateSolve,
    Finished,
}

pub struct PlatesolveMode {
    state:        State,
    camera:       Arc<dyn Camera + Send + Sync>,
    mount:        Arc<dyn Telescope + Send + Sync>,
    hal:          Arc<Hal>,
    events:       Arc<EventHandlers>,
    cur_frame:    Arc<ResultImage>,
    options:      Arc<RwLock<Options>>,
    subscribers:  Arc<EventHandlers>,
    cam_opts:     CamOptions,
    ps_opts:      PlateSolverOptions,
    plate_solver: PlateSolver,
}

impl PlatesolveMode {
    pub fn new(core: &Core) -> anyhow::Result<Self> {
        let camera = core.camera_or_err()?;
        let mount = core.telescope_or_err()?;
        let opts = core.options().read().unwrap();
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
        Ok(Self {
            state:        State::None,
            hal:          Arc::clone(core.hal()),
            events:       Arc::clone(core.events()),
            cur_frame:    Arc::clone(core.cur_frame()),
            options:      Arc::clone(core.options()),
            subscribers:  Arc::clone(core.events()),
            ps_opts:      opts.plate_solver.clone(),
            camera,
            mount,
            plate_solver,
            cam_opts,
        })
    }

    fn get_platesolver_config(&self) -> anyhow::Result<PlateSolveConfig> {
        let (ra, dec) = self.mount.eq_coord()?;
        let eq_coord = EqCoord {
            dec: degree_to_radian(dec),
            ra:  hour_to_radian(ra),
        };
        let config = PlateSolveConfig {
            time_out:       self.ps_opts.timeout,
            blind_time_out: self.ps_opts.blind_timeout,
            eq_coord:       Some(eq_coord),
            .. PlateSolveConfig::default()
        };
        Ok(config)
    }

    fn plate_solve_image(&mut self, image: &Arc<RwLock<Image>>) -> anyhow::Result<()> {
        let image = image.read().unwrap();
        let config = self.get_platesolver_config()?;
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
        let config = self.get_platesolver_config()?;
        let stars_arg = PlateSolverInData::Stars{ stars, img_width, img_height };
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

        // Image for preview in map
        let options = self.options.read().unwrap();
        let preview = self.cur_frame.create_preview_for_platesolve_image(&options.preview);
        drop(options);

        // Calculate and correct focal length in options
        self.calc_focal_len(&result)?;

        // Send event about platesolve result
        let event = PlateSolverEvent {
            cam_name:  self.camera.id().to_string(),
            result:    result.clone(),
            preview:   preview.map(Arc::new),
        };
        self.subscribers.send(
            Event::PlateSolve(event)
        );

        // Sync coordinates
        self.mount.sync(
            radian_to_hour(result.crd_now.ra),
            radian_to_degree(result.crd_now.dec),
        )?;

        Ok(true)
    }

    fn calc_focal_len(&self, ps_result: &PlateSolveOkResult) -> anyhow::Result<()> {
        let mut options = self.options.write().unwrap();
        if !options.telescope.from_platesolve { return Ok(()); }

        let cameras = self.hal.cameras()?;
        let cam_purpose = cameras.iter()
            .find(|cam| cam.id == self.camera.id())
            .map(|cam| cam.ccd)
            .unwrap_or(CcdPurpose::Unknown);

        if cam_purpose == CcdPurpose::Unknown {
            return Ok(());
        }

        let (pixel_size_x, pixel_size_y) = self.camera.pixel_size_um()?;
        let (sensor_width, sensor_height) = self.camera.ccd_size()?;
        let (frame_width, _) = options.cam.frame.active_sensor_size(sensor_width, sensor_height);
        let bin_ratio = options.cam.frame.binning.get_ratio();

        if pixel_size_x == pixel_size_y {
            let frame_horiz_size = (frame_width * bin_ratio) as f64 * pixel_size_x * 0.000_001;
            let is_telescope_ccd = matches!(cam_purpose, CcdPurpose::MainTelescopeCcd|CcdPurpose::SecodnaryTelescopeCcd);
            let mut focal_len = 1000.0/*mm*/ * frame_horiz_size / (2.0 * f64::tan(0.5 * ps_result.width));
            if is_telescope_ccd && options.telescope.barlow > 0.0 {
                focal_len /= options.telescope.barlow;
            }

            log::debug!("cam_purpose={:?}, frame_horiz_size={:.1}mm", cam_purpose, frame_horiz_size * 1000.0);
            log::info!("Calculated telescope focal len = {focal_len}");

            let cur_len =
                if is_telescope_ccd {
                    &mut options.telescope.focal_len
                } else {
                    &mut options.guiding.foc_len
                };

            let ok_to_set_new_value = f64::abs(*cur_len - focal_len) >= 2.0;
            if ok_to_set_new_value {
                log::info!("Correcting options focal len from {:.1} to {:.1}", *cur_len, focal_len);
                *cur_len = focal_len;
            }
            drop(options);
            if ok_to_set_new_value {
                if is_telescope_ccd {
                    self.events.send(Event::TelescopeFocalLenChanged(focal_len));
                } else {
                    self.events.send(Event::GuiderFocalLenChanged(focal_len));
                }
            }
        }
        Ok(())
    }
}

impl Mode for PlatesolveMode {
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

    fn camera_id(&self) -> Option<&str> {
        Some(self.camera.id())
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cam_opts.frame.exp_main)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        log::debug!("Tacking picture for plate solve with {:?}", &self.cam_opts.frame);
        take_shot(&self.camera, &self.cam_opts.frame, &self.cam_opts.ctrl)?;
        self.state = State::Capturing;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        _ = self.camera.abort_exposure();
        _ = self.mount.abort_motion();
        self.state = State::None;
        Ok(())
    }

    fn frame_options_to_restart_exposure(&self) -> Option<&FrameOptions> {
        Some(&self.cam_opts.frame)
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
                self.plate_solve_stars(&info.stars.items, info.image.width, info.image.height)?;
                self.state = State::PlateSolve;
                return Ok(NotifyResult::ProgressChanges);
            }
            _ => {},
        }

        Ok(NotifyResult::Empty)
    }

    fn notify_periodical_timer_tick(&mut self, _timer_period_ms: usize) -> anyhow::Result<NotifyResult> {
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
