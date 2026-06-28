use std::{
    any::Any, fmt::Display, path::Path,
    sync::{Arc, Mutex, RwLock, RwLockReadGuard, atomic::AtomicBool},
};

use crate::{
    core::{cam_ctrl::*, cur_devices::CurDevices},
    guiding::external_guider::*,
    hal::{events::HalEvent, *},
    image::io::FromFileCameraShot,
    options::*,
    sky_math::math::EqCoord, utils::timer::*,
};

use super::{
    events::*,
    frame_processing::*,
    mode_platesolve::*,
    mode_darks_lib::*,
    mode_focusing::*,
    mode_goto::*,
    mode_mnt_calib::*,
    mode_polar_align::PolarAlignMode,
    mode_camera::*,
    mode_waiting::*
};

#[derive(PartialEq, Copy, Clone, Debug)]
pub enum ModeType {
    Waiting,
    OpeningImgFile,
    SingleShot,
    LiveView,
    SavingRawFrames,
    MasterDark,
    MasterBias,
    DefectPixels,
    LiveStacking,
    Focusing,
    DitherCalibr,
    CreatingDefectPixels,
    CreatingMasterDarks,
    CreatingMasterBiases,
    Goto,
    CapturePlatesolve,
    PolarAlignment,
}

pub type ModeBox = Box<dyn Mode + Send + Sync>;

pub trait Mode {
    fn get_type(&self) -> ModeType;
    fn progress_string(&self) -> String;
    fn camera_id(&self) -> Option<&str> { None }
    fn progress(&self) -> Option<Progress> { None }
    fn get_cur_exposure(&self) -> Option<f64> { None }
    fn can_be_stopped(&self) -> bool { true }
    fn can_be_continued_after_stop(&self) -> bool { false }
    fn start(&mut self) -> eyre::Result<()> { Ok(()) }
    fn abort(&mut self) -> eyre::Result<()> { Ok(()) }
    fn continue_work(&mut self) -> eyre::Result<()> { Ok(()) }
    fn frame_options_to_restart_exposure(&self) -> Option<&FrameOptions> { None }
    fn restart_cam_exposure(&mut self) -> eyre::Result<bool> { Ok(false) }
    fn take_next_mode(&mut self) -> Option<ModeBox> { None }
    fn set_or_correct_value(&mut self, _value: &mut dyn Any) {}
    fn complete_img_process_params(&self, _cmd: &mut FrameProcessCommandData) {}
    fn notify_camera_download_started(&mut self, _camera_id: &str) -> eyre::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_before_frame_processing_start(&mut self, _camera_shot: &Arc<dyn CameraShot + Send + Sync>, _should_be_processed: &mut bool) -> eyre::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_about_frame_processing_result(&mut self, _fp_result: &FrameProcessResult) -> eyre::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_guider_event(&mut self, _event: ExtGuiderEvent) -> eyre::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_periodical_timer_tick(&mut self, _timer_period_ms: usize) -> eyre::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn custom_command(&mut self, _args: &dyn Any) -> eyre::Result<Option<Box<dyn Any>>> { Ok(None) }
    fn notify_processing_queue_overflow(&mut self) -> eyre::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn stop_live_view_before_this_mode(&self) -> bool { true }
}

pub enum NotifyResult {
    Empty,
    ProgressChanges,
    Finished { next_mode: Option<ModeBox> },
    Exec(Box<dyn FnOnce(&Arc<Core>, &mut ModeData)-> eyre::Result<()> + 'static + Send + Sync>),
}

pub struct ModeData {
    pub active:   ModeBox,
    pub finished: Option<ModeBox>,
    pub aborted:  Option<ModeBox>,
    previous:     Option<ModeBox>,
}

impl ModeData {
    fn new() -> Self {
        Self {
            active:    Box::new(WaitingMode),
            finished:  None,
            aborted:   None,
            previous:  None,
        }
    }
}

pub struct Core {
    pub cur_devices:    Arc<CurDevices>,
    pub hal:            Arc<Hal>,
    pub events:         Arc<EventHandlers>,
    pub options:        Arc<RwLock<Options>>,
    pub cur_frame:      Arc<ResultImage>,
    pub live_stacking:  Arc<LiveStackingData>,
    pub ext_guider:     Arc<ExternalGuiderCtrl>,

    mode:               RwLock<ModeData>,
    calibr_data:        Arc<Mutex<CalibrData>>,
    timer:              Arc<Timer>,
    img_proc_stop_flag: Mutex<Arc<AtomicBool>>, // stop flag for last command
    frame_processing:   Arc<FrameProcessing>,

}

impl Drop for Core {
    fn drop(&mut self) {
        log::info!("Core dropped");
    }
}

impl Core {
    pub fn new() -> Arc<Self> {
        let hal = Hal::new();
        let events = Arc::new(EventHandlers::new());
        let options = Arc::new(RwLock::new(Options::default()));

        let frame_processing = FrameProcessing::new();

        let cur_devices = CurDevices::new(&options, &hal, &events);

        let this = Arc::new(Self {
            options:            Arc::clone(&options),
            mode:               RwLock::new(ModeData::new()),
            cur_frame:          Arc::new(ResultImage::new()),
            calibr_data:        Arc::new(Mutex::new(CalibrData::default())),
            live_stacking:      Arc::new(LiveStackingData::new()),
            timer:              Arc::new(Timer::new()),
            img_proc_stop_flag: Mutex::new(Arc::new(AtomicBool::new(false))),
            ext_guider:         ExternalGuiderCtrl::new(),
            cur_devices,
            hal,
            frame_processing,
            events,
        });

        this.set_ext_guider_events_handler();
        this.connect_events();
        this.start_timer();
        this
    }

    pub fn stop(self: &Arc<Self>) {
        self.timer.clear();
        self.ext_guider.disconnect_events_handler();
        self.ext_guider.phd2_conn().discnnect_all_event_handlers();

        self.abort_active_mode();
        *self.mode.write().unwrap() = ModeData::new();

        log::info!("Unsubscribing all...");
        self.events.disconnect_all();

        log::info!("Done");

        log::info!("Disconnecting from INDI...");
        let indi_hal = self.hal.indi_impl();
        indi_hal.indi().disconnect_all_event_handlers(); // TODO: move into hal
        _ = indi_hal.indi().disconnect_and_wait(); // TODO: move into hal
        log::info!("Done!");

        log::info!("Stopping HAL...");
        self.hal.disconnect_all_subscribers();
        log::info!("Done!");
    }

    fn set_ext_guider_events_handler(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.ext_guider.set_events_handler(Box::new(move |event| {
            log::info!("External guider event = {:?}", event);
            let result = || -> eyre::Result<()> {
                let mut mode = self_.mode.write().unwrap();
                let res = mode.active.notify_guider_event(event.clone())?;
                self_.apply_notify_result(res, &mut mode)?;
                Ok(())
            } ();
            self_.events.send(Event::Guider(event));
            self_.process_error(result, "Core::connect_ext_guider_events");
        }));
    }

    pub fn stop_img_process_thread(&self) -> eyre::Result<()> {
        self.frame_processing.add_to_queue(FrameProcessCommand::Stop)?;
        Ok(())
    }

    pub fn mode(&self) -> RwLockReadGuard<'_, ModeData> {
        self.mode.read().unwrap()
    }

    fn process_error(
        self:    &Arc<Self>,
        result:  eyre::Result<()>,
        context: &str,
    ) {
        let Err(err) = result else { return; };
        if cfg!(debug_assertions) {
            self.process_error_str(&format!("{:?}", err), context);
        } else {
            self.process_error_str(&err, context);
        }
    }

    fn process_error_str(self: &Arc<Self>, error_text: &impl Display, context: &str) {
        log::error!("Error {}, context: {}", error_text, context);

        log::info!("Aborting active mode...");
        self.abort_active_mode();
        log::info!("Active mode aborted!");

        log::info!("Inform about error...");
        self.events.send(Event::Error(error_text.to_string()));
        log::info!("Error has informed!");
    }

    const TIMER_PERIOD_MS: usize = 250;

    fn start_timer(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.timer.exec(Self::TIMER_PERIOD_MS as _, true, move || {
            let result = self_.timer_event_handler();
            self_.process_error(result, "Core::connect_events (timer closure)");
        });
    }

    fn timer_event_handler(self: &Arc<Self>) -> eyre::Result<()> {
        let mut mode = self.mode.write().unwrap();
        let result = mode.active.notify_periodical_timer_tick(Self::TIMER_PERIOD_MS)?;
        self.apply_notify_result(result, &mut mode)?;
        drop(mode);
        self.hal.notify_periodical_timer_tick(Self::TIMER_PERIOD_MS)?;
        Ok(())
    }

    fn hal_event_handler(self: &Arc<Self>, event: HalEvent) -> eyre::Result<()> {
        match &event {
            HalEvent::Error(err) => {
                self.process_error_str(&err.as_str(), "HAL error");
            }
            HalEvent::CameraShotResult { device_id, shot } => {
                let result = self.process_camera_shot_result(device_id, shot);
                self.process_error(result, "process_camera_shot_result");
            }
            HalEvent::CameraIsReadyForCooling(device_id) |
            HalEvent::CameraIsReadyForCtrlFan(device_id) |
            HalEvent::CameraIsReadyForCtrlHeater(device_id) => {
                let options = self.options.read().unwrap();
                if options.cam.device_id == **device_id {
                    let Ok(camera) = self.hal.camera(&options.cam.device_id) else { return Ok(()); };
                    match &event {
                        HalEvent::CameraIsReadyForCooling(_) =>
                            control_camera_cooling(&camera, &options.cam.ctrl)?,
                        HalEvent::CameraIsReadyForCtrlFan(_) =>
                            control_camera_fan(&camera, &options.cam.ctrl)?,
                        HalEvent::CameraIsReadyForCtrlHeater(_) =>
                            control_camera_heater(&camera, &options.cam.ctrl)?,
                        _ => unreachable!()
                    };
                }
            }
            HalEvent::CameraBeginDownloadData(camera_id) => {
                let mut mode = self.mode.write().unwrap();
                let res = mode.active.notify_camera_download_started(camera_id)?;
                self.apply_notify_result(res, &mut mode)?;
            }
            HalEvent::CameraNeedRestartExposure(camera_id) => {
                let options = self.options.read().unwrap();
                if options.cam.device_id == **camera_id {
                    let Ok(camera) = self.hal.camera(&options.cam.device_id) else { return Ok(()); };
                    let mut mode = self.mode.write().unwrap();
                    restart_camera_exposure(
                        &camera,
                        &mut mode,
                        &options.cam.frame,
                        &options.cam.ctrl,
                    )?;
                }
            }
            HalEvent::CameraNeedInitTelescopeFocalLen(_camera_id) => {
                self.init_focal_len_for_cameras();
            }
            _ => {}
        }
        Ok(())
    }

    fn process_camera_shot_result(
        self:        &Arc<Self>,
        camera_id:   &str,
        camera_shot: &Arc<dyn CameraShot + Send + Sync>
    ) -> eyre::Result<()> {
        let mut mode = self.mode.write().unwrap();

        if Some(camera_id) != mode.active.camera_id() {
            return Ok(());
        }

        let mut should_be_processed = true;
        let res = mode.active.notify_before_frame_processing_start(
            camera_shot,
            &mut should_be_processed
        )?;
        self.apply_notify_result(res, &mut mode)?;
        if !should_be_processed {
            return Ok(());
        }

        let mut command_data = {
            let options = self.options.read().unwrap();

            let ccd_temp = if options.cam.ctrl.enable_cooler {
                Some(options.cam.ctrl.temperature)
            } else {
                None
            };

            let calibr_params = Some(CalibrParams {
                extract_dark:  options.calibr.dark_frame_en,
                dark_lib_path: options.calibr.dark_library_path.clone(),
                flat_fname:    None,
                sar_hot_pixs:  options.calibr.hot_pixels,
                ccd_temp
            });

            let new_stop_flag = Arc::new(AtomicBool::new(false));
            *self.img_proc_stop_flag.lock().unwrap() = Arc::clone(&new_stop_flag);

            FrameProcessCommandData {
                mode_type:       mode.active.get_type(),
                camera_id:       camera_id.to_string(),
                img_source:      Arc::clone(camera_shot),
                flags:           FrameProcessCommandFlags::empty(),
                frame:           Arc::clone(&self.cur_frame),
                stop_flag:       new_stop_flag,
                ref_stars:       None,
                calibr_data:     Arc::clone(&self.calibr_data),
                view_options:    options.preview.preview_params(),
                frame_options:   options.cam.frame.clone(),
                quality_options: Some(options.quality.clone()),
                cam_ctrl_opts:   None,
                live_stacking:   None,
                calibr_params,
            }
        };

        mode.active.complete_img_process_params(&mut command_data);

        self.frame_processing.add_to_queue(
            FrameProcessCommand::ProcessImage(command_data)
        )?;

        Ok(())
    }

    fn connect_events(self: &Arc<Self>) {
        // HAL events
        let self_ = Arc::clone(self);
        self.hal.connect_event_handler(move |event| {
            let res = self_.hal_event_handler(event);
            self_.process_error(res, "hal_event_handler");
        });

        // Main events
        let self_ = Arc::clone(self);
        self.events.connect(move |event| {
            self_.event_handler(event);
        });

        // Frame processing events
        let self_ = Arc::clone(self);
        self.frame_processing.connect_result_fun(
            move |res| self_.frame_process_result_handler(res)
        );
    }

    fn event_handler(self: &Arc<Self>, event: Event) {
        match &event {
            Event::CameraDeviceChanged {..} => {
                self.process_camera_changed();
            }
            Event::TelescopeFocalLenChanged(_)|
            Event::TelescopeBarlowChanged|
            Event::GuiderFocalLenChanged(_) => {
                self.init_focal_len_for_cameras();
            }
            Event::CameraCoolingOptionsChanged |
            Event::CameraFanOptionsChanged |
            Event::CameraHeaterOptionsChanged => {
                let options = self.options.read().unwrap();
                let Ok(camera) = self.hal.camera(&options.cam.device_id) else { return; };
                let res = match &event {
                    Event::CameraCoolingOptionsChanged =>
                        control_camera_cooling(&camera, &options.cam.ctrl),
                    Event::CameraFanOptionsChanged =>
                        control_camera_fan(&camera, &options.cam.ctrl),
                    Event::CameraHeaterOptionsChanged =>
                        control_camera_heater(&camera, &options.cam.ctrl),
                    _ => unreachable!(),
                };
                self.process_error(res, "event_handler, camera control");
            }
            _ => {},
        }
    }

    fn process_camera_changed(self: &Arc<Self>) {
        let options = self.options.read().unwrap();

        let Some(camera) = self.cur_devices.camera() else { return; };

        let res = control_camera_cooling(&camera, &options.cam.ctrl);
        self.process_error(res, "control_camera_cooling");

        let res = control_camera_fan(&camera, &options.cam.ctrl);
        self.process_error(res, "control_camera_fan");

        let res = control_camera_heater(&camera, &options.cam.ctrl);
        self.process_error(res, "control_camera_heater");
    }

    fn init_focal_len_for_cameras(self: &Arc<Self>) {
        let options = self.options.read().unwrap();
        let res = set_focal_len_for_cameras(&self.hal, &options);
        self.process_error(res, "set_focal_len_for_cameras");
    }

    fn frame_process_result_handler(self: &Arc<Self>, res: CommandResult) {
        match res {
            CommandResult::Result(res) => {
                if res.mode_type != ModeType::OpeningImgFile  {
                    let mut mode = self.mode.write().unwrap();
                    if Some(res.camera_id.as_str()) != mode.active.camera_id() {
                        return;
                    }
                    if mode.active.get_type() != res.mode_type {
                        return;
                    }
                    let result = || -> eyre::Result<()> {
                        let res = mode.active.notify_about_frame_processing_result(&res)?;
                        self.apply_notify_result(res, &mut mode)?;
                        Ok(())
                    } ();
                    drop(mode);
                    self.process_error(result, "Core::apply_change_result");
                }
                self.events.send(
                    Event::FrameProcessing(res.clone())
                );
            }

            CommandResult::QueueOverflow => {
                let mut mode = self.mode.write().unwrap();
                let result = || -> eyre::Result<()> {
                    let res = mode.active.notify_processing_queue_overflow()?;
                    self.apply_notify_result(res, &mut mode)?;
                    Ok(())
                } ();
                drop(mode);
                self.process_error(result, "Core::apply_change_result");

            }
            CommandResult::Error(error_str) => {
                self.abort_active_mode();
                self.events.send(Event::Error(error_str));
            }
        }
    }

    pub fn exec_mode_custom_command(
        self: &Arc<Self>,
        args: &dyn std::any::Any
    ) -> eyre::Result<Option<Box<dyn Any>>> {
        let mut mode = self.mode.write().unwrap();
        mode.active.custom_command(args)
    }

    fn start_new_mode(
        &self,
        new_mode:            impl Mode + Send + Sync + 'static,
        reset_aborted_mode:  bool,
        reset_finished_mode: bool,
    ) -> eyre::Result<()> {
        let mut mode = self.mode.write().unwrap();

        let have_to_abort_mode =
            new_mode.stop_live_view_before_this_mode() ||
            mode.active.get_type() != ModeType::LiveView;

        // abort previous mode
        if have_to_abort_mode {
            mode.active.abort()?;
        }

        // move mode.active to mode.previous
        mode.previous = Some(std::mem::replace(
            &mut mode.active,
            Box::new(WaitingMode)
        ));

        mode.active = Box::new(new_mode);
        if reset_aborted_mode {
            mode.aborted = None;
        }
        if reset_finished_mode {
            mode.finished = None;
        }
        // Start new mode
        mode.active.start()?;

        let progress = mode.active.progress();
        let mode_type = mode.active.get_type();

        drop(mode);

        // Inform about progress and and mode change
        self.events.send(Event::Progress(progress, mode_type));
        self.events.send(Event::ModeChanged);

        Ok(())
    }

    pub fn open_image_from_file(self: &Arc<Self>, file_name: &Path) -> eyre::Result<()> {
        let new_stop_flag = Arc::new(AtomicBool::new(false));
        *self.img_proc_stop_flag.lock().unwrap() = Arc::clone(&new_stop_flag);

        let options = self.options.read().unwrap();

        let calibr_params = Some(CalibrParams {
            extract_dark:  options.calibr.dark_frame_en,
            dark_lib_path: options.calibr.dark_library_path.clone(),
            flat_fname:    None,
            sar_hot_pixs:  options.calibr.hot_pixels,
            ccd_temp:      None,
        });

        let command = FrameProcessCommandData {
            mode_type:       ModeType::OpeningImgFile,
            camera_id:       String::new(),
            img_source:      Arc::new(FromFileCameraShot::new(file_name)),
            flags:           FrameProcessCommandFlags::empty(),
            frame:           Arc::clone(&self.cur_frame),
            stop_flag:       new_stop_flag,
            ref_stars:       None,
            calibr_data:     Arc::clone(&self.calibr_data),
            view_options:    options.preview.preview_params(),
            frame_options:   options.cam.frame.clone(),
            quality_options: None,
            cam_ctrl_opts:   None,
            live_stacking:   None,
            calibr_params,
        };

        self.frame_processing.add_to_queue(
            FrameProcessCommand::ProcessImage(command)
        )?;

        Ok(())
    }

    pub fn check_before_saving_raw_or_live_stacking(&self) -> eyre::Result<()> {
        let options = self.options.read().unwrap();
        if options.cam.frame.frame_type == FrameType::Lights {
            match options.guiding.mode {
                GuidingMode::MainCamera => {
                    if self.cur_devices.telescope().is_none() {
                        eyre::bail!(
                            "Guiding by main camera is selected but \
                            mound device is not selected or connected!"
                        );
                    }
                }
                GuidingMode::External => {
                    if !self.ext_guider.is_connected() {
                        eyre::bail!(
                            "Guiding by external software is selected but \
                            no external software is connected!"
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn start_single_shot(&self) -> eyre::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::SingleShot, self)?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_live_view(&self) -> eyre::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::LiveView, self)?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_saving_raw_frames(&self) -> eyre::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::SavingRawFrames, self)?;
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_live_stacking(&self) -> eyre::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::LiveStacking, self)?;
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_focusing(&self) -> eyre::Result<()> {
        let mode = FocusingMode::new(
            self,
            None,
            true,
            FocusingErrorReaction::Fail
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_mount_calibr(&self) -> eyre::Result<()> {
        let mode = MountCalibrMode::new(self, None)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_creating_dark_library(
        &self,
        dark_lib_mode: DarkLibMode,
        program: &[MasterFileCreationProgramItem]
    ) -> eyre::Result<()> {
        let mode = DarkCreationMode::new(
            self,
            dark_lib_mode,
            &self.calibr_data,
            program
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_goto_coord(
        self:     &Arc<Self>,
        eq_coord: &EqCoord,
        config:   GotoConfig,
    ) -> eyre::Result<()> {
        let mode = GotoMode::new(self, GotoDestination::Coord(*eq_coord), config)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_goto_image(self: &Arc<Self>) -> eyre::Result<()> {
        let image = self.cur_frame.image.read().unwrap();
        if image.is_empty() {
            eyre::bail!("Image is empty");
        }
        drop(image);
        let image_info = self.cur_frame.info.read().unwrap();
        let ResultImageInfo::LightInfo(light_frame_info) = &*image_info else {
            eyre::bail!("Image is not light frame");
        };
        self.mode.write().unwrap().active.abort()?;
        let mode = GotoMode::new(
            self,
            GotoDestination::Image{
                image: Arc::clone(&self.cur_frame.image),
                info: Arc::clone(&light_frame_info.image),
                stars: Arc::clone(&light_frame_info.stars)
            },
            GotoConfig::GotoPlateSolveAndCorrect,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_capture_and_platesolve(self: &Arc<Self>) -> eyre::Result<()> {
        let mode = PlatesolveMode::new(self)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_polar_alignment(self: &Arc<Self>) -> eyre::Result<()> {
        let mode = PolarAlignMode::new(self)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn abort_active_mode(&self) {
        let mut mode = self.mode.write().unwrap();

        if mode.active.get_type() == ModeType::Waiting {
            return;
        }

        _ = mode.active.abort();

        self.img_proc_stop_flag.lock().unwrap().store(true, std::sync::atomic::Ordering::Relaxed);

        let mut prev_mode = std::mem::replace(&mut mode.active, Box::new(WaitingMode));
        loop {
            if prev_mode.can_be_continued_after_stop() {
                mode.aborted = Some(prev_mode);
                break;
            }
            if let Some(next_mode) = prev_mode.take_next_mode() {
                prev_mode = next_mode;
            } else {
                break;
            }
        }
        mode.finished = None;
        drop(mode);
        self.events.send(Event::ModeChanged);
    }

    pub fn continue_prev_mode(&self) -> eyre::Result<()> {
        let mut mode = self.mode.write().unwrap();
        let Some(perv_mode) = mode.aborted.take() else {
            eyre::bail!("Aborted state is empty");
        };
        mode.active = perv_mode;
        mode.active.continue_work()?;
        let progress = mode.active.progress();
        let mode_type = mode.active.get_type();
        drop(mode);
        self.events.send(Event::ModeContinued);
        self.events.send(Event::Progress(progress, mode_type));
        self.events.send(Event::ModeChanged);
        Ok(())
    }

    fn apply_notify_result(
        self:   &Arc<Self>,
        result: NotifyResult,
        mode:   &mut ModeData,
    ) -> eyre::Result<()> {
        let mut mode_changed = false;
        let mut finished_progress_and_type = None;
        match result {
            NotifyResult::Empty => {
                return Ok(());
            }
            NotifyResult::ProgressChanges => {
            }
            NotifyResult::Finished { next_mode } => {
                let next_is_none = next_mode.is_none();
                if next_is_none {
                    finished_progress_and_type = Some((
                        mode.active.progress(),
                        mode.active.get_type()
                    ));
                }
                if let Some(next_mode) = next_mode {
                    _ = std::mem::replace(
                        &mut mode.active,
                        next_mode
                    );
                } else if let Some(prev_mode) = mode.previous.take() {
                    mode.finished = Some(std::mem::replace(
                        &mut mode.active,
                        prev_mode
                    ));
                } else {
                    mode.finished = Some(std::mem::replace(
                        &mut mode.active,
                        Box::new(WaitingMode)
                    ));
                }

                mode.active.continue_work()?;
                mode_changed = true;
            }
            NotifyResult::Exec(fun) => {
                fun(self, mode)?;
                mode_changed = true;
            }
        }

        if mode_changed {
            self.events.send(Event::ModeChanged);
        }
        if let Some((finished_progress, finished_mode_type)) = finished_progress_and_type {
            self.events.send(Event::Progress(
                finished_progress,
                finished_mode_type,
            ));
        } else {
            self.events.send(Event::Progress(
                mode.active.progress(),
                mode.active.get_type(),
            ));
        }

        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
