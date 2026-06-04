use std::{
    any::Any, path::Path, sync::{
        atomic::AtomicBool, Arc, Mutex, RwLock, RwLockReadGuard
    },
};
use gtk::glib::PropertySet;
use itertools::Itertools;

use crate::{
    core::cam_ctrl::*,
    guiding::external_guider::*,
    hal::{FrameType, Hal, events::HalEvent, indi},
    options::*,
    sky_math::math::EqCoord,
    utils::timer::*,
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
    fn cam_device(&self) -> Option<&DeviceAndProp> { None }
    fn progress(&self) -> Option<Progress> { None }
    fn get_cur_exposure(&self) -> Option<f64> { None }
    fn can_be_stopped(&self) -> bool { true }
    fn can_be_continued_after_stop(&self) -> bool { false }
    fn start(&mut self) -> anyhow::Result<()> { Ok(()) }
    fn abort(&mut self) -> anyhow::Result<()> { Ok(()) }
    fn continue_work(&mut self) -> anyhow::Result<()> { Ok(()) }
    fn frame_options_to_restart_exposure(&self) -> Option<&FrameOptions> { None }
    fn restart_cam_exposure(&mut self) -> anyhow::Result<bool> { Ok(false) }
    fn take_next_mode(&mut self) -> Option<ModeBox> { None }
    fn set_or_correct_value(&mut self, _value: &mut dyn Any) {}
    fn complete_img_process_params(&self, _cmd: &mut FrameProcessCommandData) {}
    fn notify_indi_prop_change(&mut self, _prop_change: &indi::PropChangeEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_blob_start_event(&mut self, _event: &indi::BlobStartEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_before_frame_processing_start(&mut self, _blob: &Arc<indi::BlobPropValue>, _should_be_processed: &mut bool) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_about_frame_processing_result(&mut self, _fp_result: &FrameProcessResult) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_guider_event(&mut self, _event: ExtGuiderEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_periodical_timer_tick(&mut self, _timer_period_ms: usize) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn custom_command(&mut self, _args: &dyn Any) -> anyhow::Result<Option<Box<dyn Any>>> { Ok(None) }
    fn notify_processing_queue_overflow(&mut self) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn stop_live_view_before_this_mode(&self) -> bool { return true; }
}

pub enum NotifyResult {
    Empty,
    ProgressChanges,
    Finished { next_mode: Option<ModeBox> },
    Exec(Box<dyn FnOnce(&Arc<Core>, &mut ModeData)-> anyhow::Result<()> + 'static + Send + Sync>),
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
    hal:                Arc<Hal>,
    indi:               Arc<indi::Connection>,
    options:            Arc<RwLock<Options>>,
    mode:               RwLock<ModeData>,
    events:             Arc<Events>,
    cur_frame:          Arc<ResultImage>,
    calibr_data:        Arc<Mutex<CalibrData>>,
    live_stacking:      Arc<LiveStackingData>,
    timer:              Arc<Timer>,
    img_proc_stop_flag: Mutex<Arc<AtomicBool>>, // stop flag for last command
    frame_processing:   Arc<FrameProcessing>,
    ext_guider:         Arc<ExternalGuiderCtrl>,
}

impl Drop for Core {
    fn drop(&mut self) {
        log::info!("Core dropped");
    }
}

impl Core {
    pub fn new() -> Arc<Self> {
        let hal = Hal::new();
        let events = Arc::new(Events::new());
        let options = Arc::new(RwLock::new(Options::default()));
        let indi = Arc::new(indi::Connection::new());

        let hal_impl = hal.create_indy_impl(&indi);
        hal.set_impl(hal_impl);

        let frame_processing = FrameProcessing::new();

        let this = Arc::new(Self {
            indi:               Arc::clone(&indi),
            options:            Arc::clone(&options),
            mode:               RwLock::new(ModeData::new()),
            cur_frame:          Arc::new(ResultImage::new()),
            calibr_data:        Arc::new(Mutex::new(CalibrData::default())),
            live_stacking:      Arc::new(LiveStackingData::new()),
            timer:              Arc::new(Timer::new()),
            img_proc_stop_flag: Mutex::new(Arc::new(AtomicBool::new(false))),
            ext_guider:         ExternalGuiderCtrl::new(),
            hal,
            frame_processing,
            events,
        });

        this.set_ext_guider_events_handler();
        this.connect_indi_events();
        this.connect_events();
        this.start_timer();
        this
    }

    pub fn hal(&self) -> &Arc<Hal> {
        &self.hal
    }

    pub fn indi(&self) -> &Arc<indi::Connection> {
        &self.indi
    }

    pub fn options(&self) -> &Arc<RwLock<Options>> {
        &self.options
    }

    pub fn events(&self) -> &Arc<Events> {
        &self.events
    }

    pub fn stop(self: &Arc<Self>) {
        self.timer.clear();
        self.ext_guider.disconnect_events_handler();
        self.ext_guider.phd2_conn().discnnect_all_event_handlers();

        self.abort_active_mode();
        *self.mode.write().unwrap() = ModeData::new();

        log::info!("Unsubscribing all...");
        self.events.unsubscribe_all();
        self.indi.unsubscribe_all();
        log::info!("Done");

        log::info!("Disconnecting from INDI...");
        _ = self.indi.disconnect_and_wait();
        log::info!("Done!");

        log::info!("Stopping HAL...");
        self.hal.disconnect_all_subscribers();
        self.hal.reset_impl();
        log::info!("Done!");
    }

    pub fn ext_giuder(&self) -> &Arc<ExternalGuiderCtrl> {
        &self.ext_guider
    }

    fn set_ext_guider_events_handler(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.ext_guider.set_events_handler(Box::new(move |event| {
            log::info!("External guider event = {:?}", event);
            let result = || -> anyhow::Result<()> {
                let mut mode = self_.mode.write().unwrap();
                let res = mode.active.notify_guider_event(event.clone())?;
                self_.apply_notify_result(res, &mut mode)?;
                Ok(())
            } ();
            self_.events.notify(Event::Guider(event));
            self_.process_error(result, "Core::connect_ext_guider_events");
        }));
    }

    pub fn stop_img_process_thread(&self) -> anyhow::Result<()> {
        self.frame_processing.add_to_queue(FrameProcessCommand::Stop)?;
        Ok(())
    }

    pub fn mode(&self) -> RwLockReadGuard<'_, ModeData> {
        self.mode.read().unwrap()
    }

    pub fn cur_frame(&self) -> &Arc<ResultImage> {
        &self.cur_frame
    }

    pub fn live_stacking(&self) -> &Arc<LiveStackingData> {
        &self.live_stacking
    }

    fn process_error(
        self:    &Arc<Self>,
        result:  anyhow::Result<()>,
        context: &str,
    ) {
        let Err(err) = result else { return; };
        log::error!("Error {}, context: {}", err.to_string(), context);

        log::info!("Aborting active mode...");
        self.abort_active_mode();
        log::info!("Active mode aborted!");

        log::info!("Inform about error...");
        self.events.notify(Event::Error(err.to_string()));
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

    fn timer_event_handler(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut mode = self.mode.write().unwrap();
        let result = mode.active.notify_periodical_timer_tick(Self::TIMER_PERIOD_MS)?;
        self.apply_notify_result(result, &mut mode)?;
        drop(mode);

        self.hal.notify_periodical_timer_tick(Self::TIMER_PERIOD_MS)?;

        Ok(())
    }

    fn connect_indi_events(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.indi.subscribe_events(move |event| {
            let result = || -> anyhow::Result<()> {
                match event {
                    indi::Event::BlobStart(event) => {
                        let mut mode = self_.mode.write().unwrap();
                        let result = mode.active.notify_blob_start_event(&event)?;
                        self_.apply_notify_result(result, &mut mode)?;
                    }
                    indi::Event::PropChange(prop_change) => {
                        if let indi::PropChange::Change {
                            value: indi::PropValue::Blob(blob),
                            prop_name,
                            ..
                        } = &prop_change.change {
                            self_.process_indi_blob_event(
                                blob,
                                &prop_change.device_name,
                                prop_name,
                            )?;
                        } else {
                            self_.process_indi_prop_change_event(&prop_change)?;
                        }
                    },
                    _ => {}
                }
                Ok(())
            } ();
            self_.process_error(result, "Core::connect_indi_events");
        });
    }

    fn hal_event_handler(self: &Arc<Self>, event: HalEvent) {
        match &event {
            HalEvent::CameraIsReadyForCooling(device_id) |
            HalEvent::CameraIsReadyForCtrlFan(device_id) |
            HalEvent::CameraIsReadyForCtrlHeater(device_id) => {
                let options = self.options().read().unwrap();
                if options.cam.device_id == **device_id {
                    let Ok(camera) = self.hal.camera(&options.cam.device_id) else { return; };
                    let ctrl_result = match &event {
                        HalEvent::CameraIsReadyForCooling(_) =>
                            control_camera_cooling(&camera, &options.cam.ctrl),
                        HalEvent::CameraIsReadyForCtrlFan(_) =>
                            control_camera_fan(&camera, &options.cam.ctrl),
                        HalEvent::CameraIsReadyForCtrlHeater(_) =>
                            control_camera_heater(&camera, &options.cam.ctrl),
                        _ => unreachable!()
                    };
                    self.process_error(ctrl_result, "hal_event_handler");
                }
            }
            HalEvent::NeedRestartCameraExposure(camera_id) => {
                let options = self.options().read().unwrap();
                if options.cam.device_id == **camera_id {
                    let Ok(camera) = self.hal.camera(&options.cam.device_id) else { return; };
                    let mut mode = self.mode.write().unwrap();
                    let restart_result = restart_camera_exposure(
                        &camera,
                        &mut mode,
                        &options.cam.frame,
                        &options.cam.ctrl,
                    );
                    self.process_error(restart_result, "restart_camera_exposure");
                }
            }
            HalEvent::NeedInitTelescopeFocalLenForCamera(_camera_id) => {

            }
            _ => {}
        }
    }

    fn connect_events(self: &Arc<Self>) {
        // HAL events
        let self_ = Arc::clone(self);
        self.hal.connect_event_handler(move |event| {
            self_.hal_event_handler(event);
        });

        // Main events
        let self_ = Arc::clone(self);
        self.events.subscribe(move |event| {
            self_.event_handler(event);
        });

        // Frame processing events
        let self_ = Arc::clone(&self);
        self.frame_processing.connect_result_fun(
            move |res| self_.frame_process_result_handler(res)
        );
    }

    fn event_handler(self: &Arc<Self>, event: Event) {
        match &event {
            Event::CameraDeviceChanged { new_camera_id, .. } => {
                self.process_camera_changed(new_camera_id);
            },
            Event::TelescopeFocalLenChanged(_)|
            Event::TelescopeBarlowChanged|
            Event::GuiderFocalLenChanged(_) => {
                self.process_focal_len_changed();
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

    pub fn connect_indi(
        self:         &Arc<Self>,
        indi_drivers: &indi::Drivers
    ) -> anyhow::Result<()> {
        let options = self.options.read().unwrap();
        let drivers = if !options.indi.remote {
            let telescopes    = indi_drivers.get_group_by_name("Telescopes")?;
            let cameras       = indi_drivers.get_group_by_name("CCDs")?;
            let focusers      = indi_drivers.get_group_by_name("Focusers")?;
            let filter_wheels = indi_drivers.get_group_by_name("Filter Wheels")?;
            let aux           = indi_drivers.get_group_by_name("Auxiliary")?;

            fn get_driver<'a>(
                device_name: &Option<String>,
                group:       &'a indi::DriverGroup
            ) -> Option<&'a String> {
                device_name
                    .as_ref()
                    .and_then(|name| group.get_item_by_device_name(name))
                    .map(|d| &d.driver)
            }

            [ get_driver(&options.indi.mount,     &telescopes),
              get_driver(&options.indi.camera,    &cameras),
              get_driver(&options.indi.guid_cam,  &cameras),
              get_driver(&options.indi.focuser,   &focusers),
              get_driver(&options.indi.flt_wheel, &filter_wheels),
              get_driver(&options.indi.aux1,      &aux),
              get_driver(&options.indi.aux1,      &aux),
            ].iter()
                .filter_map(|v| *v)
                .cloned()
                .unique()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        if !options.indi.remote && drivers.is_empty() {
            anyhow::bail!("No devices selected");
        }

        log::info!(
            "Connecting to INDI, remote={}, address={}, drivers='{}' ...",
            options.indi.remote,
            options.indi.address,
            drivers.iter().join(",")
        );

        let conn_settings = indi::ConnSettings {
            drivers,
            remote: options.indi.remote,
            host:   options.indi.address.clone(),
            .. Default::default()
        };

        drop(options);
        self.indi.connect(&conn_settings)?;
        Ok(())
    }

    fn process_indi_prop_change_event(
        self:        &Arc<Self>,
        prop_change: &indi::PropChangeEvent,
    ) -> anyhow::Result<()> {
        let mut mode = self.mode.write().unwrap();
        let result = mode.active.notify_indi_prop_change(prop_change)?;
        self.apply_notify_result(result, &mut mode)?;
        drop(mode);

        Ok(())
    }

    fn process_indi_blob_event(
        self:        &Arc<Self>,
        blob:        &Arc<indi::BlobPropValue>,
        device_name: &str,
        device_prop: &str,
    ) -> anyhow::Result<()> {
        if blob.data.is_empty() { return Ok(()); }
        log::debug!(
            "process_blob_event, device_name = {}, device_prop = {}, dl_time = {:.2}s",
            device_name, device_prop, blob.dl_time
        );

        let mut mode = self.mode.write().unwrap();
        let Some(mode_cam) = mode.active.cam_device() else {
            return Ok(());
        };

        if device_name != mode_cam.name {
            log::debug!("device_name({}) != mode_cam.name({}). Exiting...", device_name, mode_cam.name);
            return Ok(());
        }

        if device_prop != mode_cam.prop {
            log::debug!("device_prop({}) != mode_cam.prop({}). Exiting...", device_prop, mode_cam.prop);
            return Ok(());
        }

        let mut should_be_processed = true;
        let res = mode.active.notify_before_frame_processing_start(blob, &mut should_be_processed)?;
        self.apply_notify_result(res, &mut mode)?;
        if !should_be_processed {
            return Ok(());
        }

        let mut command_data = {
            let options = self.options.read().unwrap();
            let device = DeviceAndProp {
                name: device_name.to_string(),
                prop: device_prop.to_string(),
            };

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
                camera:          device,
                img_source:      ImageSource::Blob(Arc::clone(blob)),
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

    fn process_camera_changed(self: &Arc<Self>, new_camera_id: &str) {
        let options = self.options.read().unwrap();
        if options.cam.device_id != new_camera_id {
            return;
        }

        let Ok(camera) = self.hal.camera(new_camera_id) else { return; };

        let res = control_camera_cooling(&camera, &options.cam.ctrl);
        self.process_error(res, "control_camera_cooling");

        let res = control_camera_fan(&camera, &options.cam.ctrl);
        self.process_error(res, "control_camera_fan");

        let res = control_camera_heater(&camera, &options.cam.ctrl);
        self.process_error(res, "control_camera_heater");

        //let res = CameraCtrl::set_focal_len_for_indi_devices(&self.indi, &options);
        //self.process_error(res, "CameraWatchdog::set_telescope_focal_len");
    }

    fn process_focal_len_changed(self: &Arc<Self>) {
        //let options = self.options.read().unwrap();
        //let res = CameraCtrl::set_focal_len_for_indi_devices(&self.indi, &options);
        //self.process_error(res, "CameraWatchdog::set_telescope_focal_len");
    }

    fn frame_process_result_handler(self: &Arc<Self>, res: CommandResult) {
        match res {
            CommandResult::Result(res) => {
                let is_opening_file = res.mode_type == ModeType::OpeningImgFile;

                let mut mode = self.mode.write().unwrap();
                if Some(&res.camera) != mode.active.cam_device() && !is_opening_file {
                    return;
                }

                if mode.active.get_type() != res.mode_type && !is_opening_file {
                    return;
                }

                self.events.notify(
                    Event::FrameProcessing(res.clone())
                );

                let result = || -> anyhow::Result<()> {
                    let res = mode.active.notify_about_frame_processing_result(&res)?;
                    self.apply_notify_result(res, &mut mode)?;
                    Ok(())
                } ();
                drop(mode);
                self.process_error(result, "Core::apply_change_result");
            }

            CommandResult::QueueOverflow => {
                let mut mode = self.mode.write().unwrap();
                let result = || -> anyhow::Result<()> {
                    let res = mode.active.notify_processing_queue_overflow()?;
                    self.apply_notify_result(res, &mut mode)?;
                    Ok(())
                } ();
                drop(mode);
                self.process_error(result, "Core::apply_change_result");

            }
            CommandResult::Error(error_str) => {
                self.abort_active_mode();
                self.events.notify(Event::Error(error_str));
            }
        }
    }

    pub fn exec_mode_custom_command(
        self: &Arc<Self>,
        args: &dyn std::any::Any
    ) -> anyhow::Result<Option<Box<dyn Any>>> {
        let mut mode = self.mode.write().unwrap();
        mode.active.custom_command(args)
    }

    fn start_new_mode(
        &self,
        new_mode:            impl Mode + Send + Sync + 'static,
        reset_aborted_mode:  bool,
        reset_finished_mode: bool,
    ) -> anyhow::Result<()> {
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

        // init camera for mode
        if have_to_abort_mode {
            self.init_cam_for_mode(&new_mode)?;
        }

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
        self.events.notify(Event::Progress(progress, mode_type));
        self.events.notify(Event::ModeChanged);

        Ok(())
    }

    pub fn open_image_from_file(self: &Arc<Self>, file_name: &Path) -> anyhow::Result<()> {
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
            camera:          DeviceAndProp::default(),
            img_source:      ImageSource::FileName(file_name.to_path_buf()),
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

    pub fn check_before_saving_raw_or_live_stacking(&self) -> anyhow::Result<()> {
        let options = self.options.read().unwrap();
        if options.cam.frame.frame_type == FrameType::Lights {
            match options.guiding.mode {
                GuidingMode::MainCamera => {
                    if !self.indi.is_device_enabled(&options.mount.device).unwrap_or(false) {
                        anyhow::bail!(
                            "Guiding by main camera is selected but \
                            mound device is not selected or connected!"
                        );
                    }
                }
                GuidingMode::External => {
                    if !self.ext_guider.is_connected() {
                        anyhow::bail!(
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

    pub fn start_single_shot(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::SingleShot, &self)?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_live_view(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::LiveView, &self)?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_saving_raw_frames(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::SavingRawFrames, &self)?;
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_live_stacking(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(CameraMode::LiveStacking, &self)?;
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_focusing(&self) -> anyhow::Result<()> {
        let mode = FocusingMode::new(
            &self.hal,
            &self.options,
            &self.events,
            None,
            true,
            FocusingErrorReaction::Fail
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_mount_calibr(&self) -> anyhow::Result<()> {
        let mode = MountCalibrMode::new(
            &self.hal,
            &self.options,
            None
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_creating_dark_library(
        &self,
        dark_lib_mode: DarkLibMode,
        program: &[MasterFileCreationProgramItem]
    ) -> anyhow::Result<()> {
        let mode = DarkCreationMode::new(
            &self.hal,
            dark_lib_mode,
            &self.calibr_data,
            &self.options,
            program
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_goto_coord(
        &self,
        eq_coord: &EqCoord,
        config:   GotoConfig,
    ) -> anyhow::Result<()> {
        let mode = GotoMode::new(
            &self.hal,
            GotoDestination::Coord(*eq_coord),
            config,
            &self.options,
            &self.cur_frame,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_goto_image(&self) -> anyhow::Result<()> {
        let image = self.cur_frame.image.read().unwrap();
        if image.is_empty() {
            anyhow::bail!("Image is empty");
        }
        drop(image);
        let image_info = self.cur_frame.info.read().unwrap();
        let ResultImageInfo::LightInfo(light_frame_info) = &*image_info else {
            anyhow::bail!("Image is not light frame");
        };
        self.mode.write().unwrap().active.abort()?;
        let mode = GotoMode::new(
            &self.hal,
            GotoDestination::Image{
                image: Arc::clone(&self.cur_frame.image),
                info: Arc::clone(&light_frame_info.image),
                stars: Arc::clone(&light_frame_info.stars)
            },
            GotoConfig::GotoPlateSolveAndCorrect,
            &self.options,
            &self.cur_frame,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_capture_and_platesolve(&self) -> anyhow::Result<()> {
        let mode = PlatesolveMode::new(
            &self.hal,
            &self.events,
            &self.options,
            &self.cur_frame,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_polar_alignment(&self) -> anyhow::Result<()> {
        let mode = PolarAlignMode::new(
            &self.hal,
            &self.cur_frame,
            &self.options,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn init_cam_for_mode(&self, mode: &dyn Mode) -> anyhow::Result<()> {
        let Some(cam_device) = &mode.cam_device() else {
            return Ok(());
        };

        // Enable blob

        self.indi.command_enable_blob(
            &cam_device.name,
            None,
            indi::BlobEnable::Also,
        )?;

        Ok(())
    }

    pub fn abort_active_mode(&self) {
        let mut mode = self.mode.write().unwrap();

        if mode.active.get_type() == ModeType::Waiting {
            return;
        }

        _ = mode.active.abort();

        self.img_proc_stop_flag.lock().unwrap().set(true);

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
        self.events.notify(Event::ModeChanged);
    }

    pub fn continue_prev_mode(&self) -> anyhow::Result<()> {
        let mut mode = self.mode.write().unwrap();
        let Some(perv_mode) = mode.aborted.take() else {
            anyhow::bail!("Aborted state is empty");
        };
        mode.active = perv_mode;
        mode.active.continue_work()?;
        let progress = mode.active.progress();
        let mode_type = mode.active.get_type();
        drop(mode);
        self.events.notify(Event::ModeContinued);
        self.events.notify(Event::Progress(progress, mode_type));
        self.events.notify(Event::ModeChanged);
        Ok(())
    }

    fn apply_notify_result(
        self:   &Arc<Self>,
        result: NotifyResult,
        mode:   &mut ModeData,
    ) -> anyhow::Result<()> {
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
            self.events.notify(Event::ModeChanged);
        }
        if let Some((finished_progress, finished_mode_type)) = finished_progress_and_type {
            self.events.notify(Event::Progress(
                finished_progress,
                finished_mode_type,
            ));
        } else {
            self.events.notify(Event::Progress(
                mode.active.progress(),
                mode.active.get_type(),
            ));
        }

        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
