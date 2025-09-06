use std::{
    any::Any, path::Path, sync::{
        atomic::{AtomicBool, Ordering }, mpsc, Arc, Mutex, RwLock, RwLockReadGuard
    },
    thread::JoinHandle,
};
use gtk::glib::PropertySet;
use itertools::Itertools;

use crate::{
    core::{cam_watchdog::{CamWatchdogResult, CameraWatchdog}, consts::*},
    guiding::external_guider::*,
    image::raw::FrameType,
    indi, options::*,
    sky_math::math::EqCoord,
    utils::timer::*
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
    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn custom_command(&mut self, _args: &dyn Any) -> anyhow::Result<Option<Box<dyn Any>>> { Ok(None) }
    fn notify_processing_queue_overflow(&mut self) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
}

pub enum NotifyResult {
    Empty,
    ProgressChanges,
    Finished { next_mode: Option<ModeBox> },
    StartFocusing,
    StartMountCalibr,
    StartCreatingDefectPixelsFile(MasterFileCreationProgramItem),
    StartCreatingMasterDarkFile(MasterFileCreationProgramItem),
    StartCreatingMasterBiasFile(MasterFileCreationProgramItem),
}

pub struct ModeData {
    pub mode:          ModeBox,
    pub finished_mode: Option<ModeBox>,
    pub aborted_mode:  Option<ModeBox>,
    prev_mode:         Option<ModeBox>,
}

impl ModeData {
    fn new() -> Self {
        Self {
            mode:           Box::new(WaitingMode),
            finished_mode:  None,
            aborted_mode:   None,
            prev_mode:      Some(Box::new(WaitingMode)),
        }
    }
}

pub struct Core {
    indi:               Arc<indi::Connection>,
    options:            Arc<RwLock<Options>>,
    mode_data:          RwLock<ModeData>,
    events:             Arc<Events>,
    cur_frame:          Arc<ResultImage>,
    calibr_data:        Arc<Mutex<CalibrData>>,
    live_stacking:      Arc<LiveStackingData>,
    timer:              Arc<Timer>,
    cam_watchdog:       Arc<Mutex<CameraWatchdog>>,
    img_proc_stop_flag: Mutex<Arc<AtomicBool>>, // stop flag for last command

    /// commands for passing into frame processing thread
    img_cmds_sender:    mpsc::Sender<FrameProcessCommand>, // TODO: make API
    frame_proc_thread:  Option<JoinHandle<()>>,
    ext_guider:         Arc<ExternalGuiderCtrl>,
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(frame_proc_thread) = self.frame_proc_thread.take() {
            log::info!("Process thread joining...");
            _ = frame_proc_thread.join();
            log::info!("Process thread joined");
        }

        log::info!("Core dropped");
    }
}

impl Core {
    pub fn new() -> Arc<Self> {
        let (img_cmds_sender, frame_proc_thread) = start_frame_processing_thread();
        let result = Arc::new(Self {
            indi:               Arc::new(indi::Connection::new()),
            options:            Arc::new(RwLock::new(Options::default())),
            mode_data:          RwLock::new(ModeData::new()),
            events:             Arc::new(Events::new()),
            cur_frame:          Arc::new(ResultImage::new()),
            calibr_data:        Arc::new(Mutex::new(CalibrData::default())),
            live_stacking:      Arc::new(LiveStackingData::new()),
            timer:              Arc::new(Timer::new()),
            cam_watchdog:       Arc::new(Mutex::new(CameraWatchdog::new())),
            img_proc_stop_flag: Mutex::new(Arc::new(AtomicBool::new(false))),
            ext_guider:         ExternalGuiderCtrl::new(),
            frame_proc_thread:  Some(frame_proc_thread),
            img_cmds_sender,
        });

        result. set_ext_guider_events_handler();
        result.connect_indi_events();
        result.connect_events();
        result.connect_1s_timer_event();
        result
    }

    pub fn indi(&self) -> &Arc<indi::Connection> {
        &self.indi
    }

    pub fn options(&self) -> &Arc<RwLock<Options>> {
        &self.options
    }

    pub fn cam_watchdog(&self) -> &Arc<Mutex<CameraWatchdog>> {
        &self.cam_watchdog
    }

    pub fn stop(self: &Arc<Self>) {
        self.abort_active_mode();
        self.timer.clear();

        log::info!("Unsubscribing all...");
        self.events.unsubscribe_all();
        self.indi.unsubscribe_all();
        log::info!("Done");

        log::info!("Disconnecting from INDI...");
        _ = self.indi.disconnect_and_wait();
        log::info!("Done!");
    }

    pub fn ext_giuder(&self) -> Arc<ExternalGuiderCtrl> {
        Arc::clone(&self.ext_guider)
    }

    fn set_ext_guider_events_handler(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.ext_guider.set_events_handler(Box::new(move |event| {
            log::info!("External guider event = {:?}", event);
            let result = || -> anyhow::Result<()> {
                let mut mode = self_.mode_data.write().unwrap();
                let res = mode.mode.notify_guider_event(event.clone())?;
                self_.apply_change_result(res, &mut mode)?;
                Ok(())
            } ();
            self_.events.notify(Event::Guider(event));
            self_.process_error(result, "Core::connect_ext_guider_events");
        }));
    }

    pub fn stop_img_process_thread(&self) -> anyhow::Result<()> {
        self.img_cmds_sender
            .send(FrameProcessCommand::Exit)
            .map_err(|_| anyhow::anyhow!("Can't send exit command"))?;
        Ok(())
    }

    pub fn mode_data(&self) -> RwLockReadGuard<'_, ModeData> {
        self.mode_data.read().unwrap()
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
        log::error!("Error in {}: {}", context, err.to_string());

        log::info!("Aborting active mode...");
        self.abort_active_mode();
        log::info!("Active mode aborted!");

        log::info!("Inform about error...");
        self.events.notify(Event::Error(err.to_string()));
        log::info!("Error has informed!");
    }

    fn connect_1s_timer_event(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.timer.exec(1000, true, move || {
            let result = self_.timer_event_handler();
            self_.process_error(result, "Core::connect_events (timer closure)");
        });
    }

    fn timer_event_handler(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut mode_data = self.mode_data.write().unwrap();
        let options = self.options.read().unwrap();

        let debug_blob_frozen = if cfg!(debug_assertions) {
            options.cam.debug.blob_frozen
        } else {
            false
        };

        if let Some(cam_device) = &options.cam.device {
            let mut cam_watchdog = self.cam_watchdog.lock().unwrap();

            let cam_watchdog_1s_res = cam_watchdog.notify_timer_1s(
                &self.indi,
                &options,
                cam_device,
                debug_blob_frozen,
            );

            match cam_watchdog_1s_res {
                Ok(cam_watchdog_result) => {
                    match cam_watchdog_result {
                        CamWatchdogResult::Waiting =>
                            return Ok(()),
                        CamWatchdogResult::RestartCameraShot => {
                            self.restart_camera_exposure(&mut mode_data, &options)?;
                        }
                        _ => {},
                    }
                }
                Err(err) => {
                    log::error!("Error in cam_watchdog.notify_timer_1s(): {}", err)
                }
            }
        }

        let result = mode_data.mode.notify_timer_1s()?;
        self.apply_change_result(result, &mut mode_data)?;

        Ok(())
    }

    fn connect_indi_events(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        let img_cmds_sender = self.img_cmds_sender.clone();
        self.indi.subscribe_events(move |event| {
            let result = || -> anyhow::Result<()> {
                match event {
                    indi::Event::BlobStart(event) => {
                        let mut mode_data = self_.mode_data.write().unwrap();
                        let result = mode_data.mode.notify_blob_start_event(&event)?;
                        self_.apply_change_result(result, &mut mode_data)?;
                    }
                    indi::Event::PropChange(prop_change) => {
                        if let indi::PropChange::Change {
                            value: indi::PropChangeValue{
                                prop_value: indi::PropValue::Blob(blob), ..
                            }, ..
                        } = &prop_change.change {
                            self_.process_indi_blob_event(
                                blob,
                                &prop_change.device_name,
                                &prop_change.prop_name,
                                &img_cmds_sender,
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

    fn connect_events(self: &Arc<Self>) {
        let self_ = Arc::clone(&self);
        self.events.subscribe(move |event| {
            match event {
                Event::CameraDeviceChanged { from, to } => {
                    self_.process_camera_changed(from, to);
                },
                _ => {},
            }
        });
    }

    pub fn connect_indi(
        self: &Arc<Self>,
        indi_drivers: &indi::Drivers
    ) -> anyhow::Result<()> {
        let mut cam_watchdog = self.cam_watchdog.lock().unwrap();
        cam_watchdog.clear();
        drop(cam_watchdog);

        let options = self.options.read().unwrap();
        let drivers = if !options.indi.remote {
            let telescopes = indi_drivers.get_group_by_name("Telescopes")?;
            let cameras = indi_drivers.get_group_by_name("CCDs")?;
            let focusers = indi_drivers.get_group_by_name("Focusers")?;
            let telescope_driver_name = options.indi.mount.as_ref()
                .and_then(|name| telescopes.get_item_by_device_name(name))
                .map(|d| &d.driver);
            let camera_driver_name = options.indi.camera.as_ref()
                .and_then(|name| cameras.get_item_by_device_name(name))
                .map(|d| &d.driver);
            let guid_cam_driver_name = options.indi.guid_cam.as_ref()
                .and_then(|name| cameras.get_item_by_device_name(name))
                .map(|d| &d.driver);
            let focuser_driver_name = options.indi.focuser.as_ref()
                .and_then(|name| focusers.get_item_by_device_name(name))
                .map(|d| &d.driver);
            [ telescope_driver_name,
              camera_driver_name,
              guid_cam_driver_name,
              focuser_driver_name
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
            remote:               options.indi.remote,
            host:                 options.indi.address.clone(),
            activate_all_devices: !options.indi.remote,
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
        let mut mode_data = self.mode_data.write().unwrap();

        let result = mode_data.mode.notify_indi_prop_change(prop_change)?;
        self.apply_change_result(result, &mut mode_data)?;

        drop(mode_data);

        let options = self.options.read().unwrap();

        if let Some(mode_cam_device) = &options.cam.device {
            let mut cam_watchdog = self.cam_watchdog.lock().unwrap();
            cam_watchdog.notify_indi_prop_change(mode_cam_device, prop_change)?;
            drop(cam_watchdog);
        }

        Ok(())
    }


    fn process_indi_blob_event(
        self:              &Arc<Self>,
        blob:              &Arc<indi::BlobPropValue>,
        device_name:       &str,
        device_prop:       &str,
        frame_proc_sender: &mpsc::Sender<FrameProcessCommand>,
    ) -> anyhow::Result<()> {
        if blob.data.is_empty() { return Ok(()); }
        log::debug!(
            "process_blob_event, device_name = {}, device_prop = {}, dl_time = {:.2}s",
            device_name, device_prop, blob.dl_time
        );

        let mut mode = self.mode_data.write().unwrap();
        let Some(mode_cam) = mode.mode.cam_device() else {
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
        let res = mode.mode.notify_before_frame_processing_start(blob, &mut should_be_processed)?;
        self.apply_change_result(res, &mut mode)?;
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
                mode_type:       mode.mode.get_type(),
                camera:          device,
                shot_id:         blob.shot_id,
                img_source:      ImageSource::Blob(Arc::clone(blob)),
                frame:           Arc::clone(&self.cur_frame),
                stop_flag:       new_stop_flag,
                ref_stars:       None,
                calibr_data:     Arc::clone(&self.calibr_data),
                view_options:    options.preview.preview_params(),
                frame_options:   options.cam.frame.clone(),
                quality_options: Some(options.quality.clone()),
                live_stacking:   None,
                calibr_params,
            }
        };

        mode.mode.complete_img_process_params(&mut command_data);

        let result_fun = {
            let self_ = Arc::clone(self);
            move |res: CommandResult| {
                self_.frame_process_result_handler(res);
            }
        };

        frame_proc_sender.send(FrameProcessCommand::ProcessImage {
            command: command_data,
            result_fun: Box::new(result_fun),
        }).unwrap();

        Ok(())
    }

    fn process_camera_changed(self: &Arc<Self>, _from: Option<DeviceAndProp>, to: DeviceAndProp) {
        let options = self.options.read().unwrap();
        if options.cam.device.as_ref() != Some(&to) {
            return;
        }

        let cam_watchdog = self.cam_watchdog.lock().unwrap();

        let res = cam_watchdog.control_camera_cooling(&self.indi, &to.name, &options, true);
        self.process_error(res, "Core::control_camera_cooling");

        let res = cam_watchdog.control_camera_fan(&self.indi,&to.name, &options, true);
        self.process_error(res, "Core::control_camera_fan");

        let res = cam_watchdog.control_camera_heater(&self.indi,&to.name, &options, true);
        self.process_error(res, "Core::control_camera_heater");
    }

    fn frame_process_result_handler(self: &Arc<Self>, res: CommandResult) {
        match res {
            CommandResult::Result(res) => {
                if res.cmd_stop_flag.load(Ordering::Relaxed) {
                    return;
                }

                let is_opening_file = res.mode_type == ModeType::OpeningImgFile;

                let mut mode = self.mode_data.write().unwrap();
                if Some(&res.camera) != mode.mode.cam_device() && !is_opening_file {
                    return;
                }

                if mode.mode.get_type() != res.mode_type && !is_opening_file {
                    return;
                }

                self.events.notify(
                    Event::FrameProcessing(res.clone())
                );

                let result = || -> anyhow::Result<()> {
                    let res = mode.mode.notify_about_frame_processing_result(&res)?;
                    self.apply_change_result(res, &mut mode)?;
                    Ok(())
                } ();
                drop(mode);
                self.process_error(result, "Core::apply_change_result");
            }

            CommandResult::QueueOverflow => {
                let mut mode = self.mode_data.write().unwrap();
                let result = || -> anyhow::Result<()> {
                    let res = mode.mode.notify_processing_queue_overflow()?;
                    self.apply_change_result(res, &mut mode)?;
                    Ok(())
                } ();
                drop(mode);
                self.process_error(result, "Core::apply_change_result");

            }
        }
    }

    fn restart_camera_exposure(self: &Arc<Self>, mode_data: &mut ModeData, options: &Options) -> anyhow::Result<()> {
        let Some(cam_device) = mode_data.mode.cam_device().cloned() else { return Ok(()); };
        log::error!("Begin restart exposure of camera {}...", cam_device.name);

        // Try to restart exposure by current mode
        let restarted_by_mode = mode_data.mode.restart_cam_exposure()?;

        if !restarted_by_mode {
            // Mode not restarted the camera exposure. Do it itself

            abort_camera_exposure(&self.indi, &cam_device)?;

            let mode_cam_opts =
                if let Some(frame_opts) = mode_data.mode.frame_options_to_restart_exposure() {
                    frame_opts
                } else {
                    &options.cam.frame
                };

            apply_camera_options_and_take_shot(
                &self.indi,
                &cam_device,
                mode_cam_opts,
                &options.cam.ctrl
            )?;
        }
        log::error!("Exposure of camera {} restarted!", &cam_device.name);
        Ok(())
    }

    pub fn events(&self) -> &Arc<Events> {
        &self.events
    }

    pub fn exec_mode_custom_command(
        self: &Arc<Self>,
        args: &dyn std::any::Any
    ) -> anyhow::Result<Option<Box<dyn Any>>> {
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode.custom_command(args)
    }

    fn start_new_mode(
        &self,
        mode:                impl Mode + Send + Sync + 'static,
        reset_aborted_mode:  bool,
        reset_finished_mode: bool,
    ) -> anyhow::Result<()> {
        // abort previous mode
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.prev_mode = Some(std::mem::replace(
            &mut mode_data.mode,
            Box::new(WaitingMode)
        ));
        mode_data.mode.abort()?;
        drop(mode_data);

        // init camera for mode
        self.init_cam_for_mode(&mode)?;

        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode = Box::new(mode);
        if reset_aborted_mode {
            mode_data.aborted_mode = None;
        }
        if reset_finished_mode {
            mode_data.finished_mode = None;
        }

        // Start new mode
        mode_data.mode.start()?;

        let progress = mode_data.mode.progress();
        let mode_type = mode_data.mode.get_type();

        drop(mode_data);

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
            shot_id:         None,
            img_source:      ImageSource::FileName(file_name.to_path_buf()),
            frame:           Arc::clone(&self.cur_frame),
            stop_flag:       new_stop_flag,
            ref_stars:       None,
            calibr_data:     Arc::clone(&self.calibr_data),
            view_options:    options.preview.preview_params(),
            frame_options:   options.cam.frame.clone(),
            quality_options: None,
            live_stacking:   None,
            calibr_params,
        };

        let result_fun = {
            let self_ = Arc::clone(self);
            move |res: CommandResult| self_.frame_process_result_handler(res)
        };

        self.img_cmds_sender.send(FrameProcessCommand::ProcessImage {
            command,
            result_fun: Box::new(result_fun),
        }).unwrap();

        Ok(())
    }

    pub fn start_single_shot(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(
            &self.indi,
            &self.events,
            CameraMode::SingleShot,
            &self.options,
        )?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_live_view(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(
            &self.indi,
            &self.events,
            CameraMode::LiveView,
            &self.options,
        )?;
        self.start_new_mode(mode, false, true)?;
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

    pub fn start_saving_raw_frames(&self) -> anyhow::Result<()> {
        self.abort_active_mode();
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            &self.events,
            CameraMode::SavingRawFrames,
            &self.options,
        )?;
        self.live_stacking.clear();
        mode.set_external_guider(&self.ext_guider);
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_live_stacking(&self) -> anyhow::Result<()> {
        self.abort_active_mode();
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            &self.events,
            CameraMode::LiveStacking,
            &self.options,
        )?;
        self.live_stacking.clear();
        mode.set_external_guider(&self.ext_guider);
        mode.set_live_stacking(&self.live_stacking);
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_focusing(&self) -> anyhow::Result<()> {
        self.abort_active_mode();
        let mode = FocusingMode::new(
            &self.indi,
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
        self.abort_active_mode();
        let mode = MountCalibrMode::new(&self.indi, &self.options, None)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_creating_dark_library(
        &self,
        dark_lib_mode: DarkLibMode,
        program: &[MasterFileCreationProgramItem]
    ) -> anyhow::Result<()> {
        self.abort_active_mode();
        let mode = DarkCreationMode::new(
            dark_lib_mode,
            &self.calibr_data,
            &self.options,
            &self.indi,
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
        self.abort_active_mode();
        let mode = GotoMode::new(
            GotoDestination::Coord(*eq_coord),
            config,
            &self.options,
            &self.indi,
            &self.cur_frame,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_goto_image(&self) -> anyhow::Result<()> {
        self.abort_active_mode();
        let image = self.cur_frame.image.read().unwrap();
        if image.is_empty() {
            anyhow::bail!("Image is empty");
        }
        drop(image);
        let image_info = self.cur_frame.info.read().unwrap();
        let ResultImageInfo::LightInfo(light_frame_info) = &*image_info else {
            anyhow::bail!("Image is not light frame");
        };
        self.mode_data.write().unwrap().mode.abort()?;
        let mode = GotoMode::new(
            GotoDestination::Image{
                image: Arc::clone(&self.cur_frame.image),
                info: Arc::clone(&light_frame_info.image),
                stars: Arc::clone(&light_frame_info.stars)
            },
            GotoConfig::GotoPlateSolveAndCorrect,
            &self.options,
            &self.indi,
            &self.cur_frame,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_capture_and_platesolve(&self) -> anyhow::Result<()> {
        self.abort_active_mode();
        let mode = PlatesolveMode::new(
            &self.options,
            &self.indi,
            &self.cur_frame,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_polar_alignment(&self) -> anyhow::Result<()> {
        self.abort_active_mode();
        let mode = PolarAlignMode::new(
            &self.indi,
            &self.cur_frame,
            &self.options,
            &self.events,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn init_cam_telescope_data(&self) -> anyhow::Result<()> {
        if self.indi.state() != indi::ConnState::Connected {
            return Ok(());
        }

        self.init_cam_telescope_data_impl()?;
        Ok(())
    }

    fn init_cam_telescope_data_impl(&self) -> anyhow::Result<()> {
        let cam_devices = self.indi.get_devices_list_by_interface(indi::DriverInterface::CCD);
        let options = self.options.read().unwrap();
        for device in cam_devices {
            if !self.indi.camera_is_telescope_info_supported(&device.name)? {
                continue;
            }
            let is_sim_guider = *device.driver == "indi_simulator_guide";
            let is_selected_guider = options.indi.guid_cam.as_deref() == Some(&*device.name);
            let is_guider_cam = is_sim_guider || is_selected_guider;
            let focal_len = if !is_guider_cam {
                options.telescope.real_focal_length()
            } else {
                options.guiding.foc_len
            };
            let aperture = 0.2 * focal_len;
            self.indi.camera_set_telescope_info(
                &device.name,
                focal_len,
                aperture,
                false,
                INDI_SET_PROP_TIMEOUT
            )?;
        }
        Ok(())
    }

    fn init_cam_for_mode(&self, mode: &dyn Mode) -> anyhow::Result<()> {
        let Some(cam_device) = &mode.cam_device() else {
            return Ok(());
        };

        // Enable blob

        self.indi.command_enable_blob(
            &cam_device.name,
            None,
            indi::BlobEnable::Also,
        )?;

        // Set telescope info into camera props

        self.init_cam_telescope_data_impl()?;

        Ok(())
    }

    pub fn abort_active_mode(&self) {
        let mut mode_data = self.mode_data.write().unwrap();
        if mode_data.mode.get_type() == ModeType::Waiting {
            return;
        }
        _ = mode_data.mode.abort();

        self.img_proc_stop_flag.lock().unwrap().set(true);

        let mut prev_mode = std::mem::replace(&mut mode_data.mode, Box::new(WaitingMode));
        loop {
            if prev_mode.can_be_continued_after_stop() {
                mode_data.aborted_mode = Some(prev_mode);
                break;
            }
            if let Some(next_mode) = prev_mode.take_next_mode() {
                prev_mode = next_mode;
            } else {
                break;
            }
        }
        mode_data.finished_mode = None;
        drop(mode_data);
        self.events.notify(Event::ModeChanged);
    }

    pub fn continue_prev_mode(&self) -> anyhow::Result<()> {
        let mut mode_data = self.mode_data.write().unwrap();
        let Some(perv_mode) = mode_data.aborted_mode.take() else {
            anyhow::bail!("Aborted state is empty");
        };
        mode_data.mode = perv_mode;
        mode_data.mode.continue_work()?;
        let progress = mode_data.mode.progress();
        let mode_type = mode_data.mode.get_type();
        drop(mode_data);
        self.events.notify(Event::ModeContinued);
        self.events.notify(Event::Progress(progress, mode_type));
        self.events.notify(Event::ModeChanged);
        Ok(())
    }

    fn apply_change_result(
        self:      &Arc<Self>,
        result:    NotifyResult,
        mode_data: &mut ModeData,
    ) -> anyhow::Result<()> {
        let mut mode_changed = false;
        let mut progress_changed = false;
        let mut finished_progress_and_type = None;
        match result {
            NotifyResult::ProgressChanges => {
                progress_changed = true;
            }
            NotifyResult::Finished { next_mode } => {
                let next_is_none = next_mode.is_none();
                if next_is_none {
                    finished_progress_and_type = Some((
                        mode_data.mode.progress(),
                        mode_data.mode.get_type()
                    ));
                }
                if let Some(next_mode) = next_mode {
                    _ = std::mem::replace(
                        &mut mode_data.mode,
                        next_mode
                    );
                } else if let Some(prev_mode) = mode_data.prev_mode.take() {
                    mode_data.finished_mode = Some(std::mem::replace(
                        &mut mode_data.mode,
                        prev_mode
                    ));
                } else {
                    mode_data.finished_mode = Some(std::mem::replace(
                        &mut mode_data.mode,
                        Box::new(WaitingMode)
                    ));
                }

                mode_data.mode.continue_work()?;
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartFocusing => {
                mode_data.mode.abort()?;
                let prev_mode = std::mem::replace(&mut mode_data.mode, Box::new(WaitingMode));
                let mut mode = FocusingMode::new(
                    &self.indi,
                    &self.options,
                    &self.events,
                    Some(prev_mode),
                    false,
                    FocusingErrorReaction::IgnoreAndExit
                )?;
                mode.start()?;
                mode_data.mode = Box::new(mode);
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartMountCalibr => {
                mode_data.mode.abort()?;
                let prev_mode = std::mem::replace(&mut mode_data.mode, Box::new(WaitingMode));
                let mut mode = MountCalibrMode::new(&self.indi, &self.options, Some(prev_mode))?;
                mode.start()?;
                mode_data.mode = Box::new(mode);
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartCreatingDefectPixelsFile(item) => {
                self.start_dark_libarary_mode_stage(mode_data, CameraMode::DefectPixels, &item)?;
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartCreatingMasterDarkFile(item) => {
                self.start_dark_libarary_mode_stage(mode_data, CameraMode::MasterDark, &item)?;
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartCreatingMasterBiasFile(item) => {
                self.start_dark_libarary_mode_stage(mode_data, CameraMode::MasterBias, &item)?;
                mode_changed = true;
                progress_changed = true;
            }
            _ => {}
        }

        if mode_changed {
            self.events.notify(Event::ModeChanged);
        }
        if let Some((finished_progress, finished_mode_type)) = finished_progress_and_type {
            self.events.notify(Event::Progress(
                finished_progress,
                finished_mode_type,
            ));
        } else if progress_changed || mode_changed {
            self.events.notify(Event::Progress(
                mode_data.mode.progress(),
                mode_data.mode.get_type(),
            ));
        }

        Ok(())
    }

    fn start_dark_libarary_mode_stage(
        self:         &Arc<Self>,
        mode_data:    &mut ModeData,
        mode:         CameraMode,
        program_item: &MasterFileCreationProgramItem
    ) -> anyhow::Result<()> {
        mode_data.mode.abort()?;
        let prev_mode = std::mem::replace(&mut mode_data.mode, Box::new(WaitingMode));
        let mut mode = TackingPicturesMode::new(&self.indi, &self.events, mode, &self.options)?;
        mode.set_dark_creation_program_item(program_item);
        mode.set_next_mode(Some(prev_mode));
        self.init_cam_for_mode(&mode)?;
        mode.start()?;
        mode_data.mode = Box::new(mode);
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////

pub fn apply_camera_options_and_take_shot(
    indi:     &indi::Connection,
    device:   &DeviceAndProp,
    frame:    &FrameOptions,
    cam_ctrl: &CamCtrlOptions,
) -> anyhow::Result<u64> {
    let cam_ccd = indi::CamCcd::from_ccd_prop_name(&device.prop);

    // Disable fast toggle

    if indi.camera_is_fast_toggle_supported(&device.name).unwrap_or(false) {
         indi.camera_enable_fast_toggle(&device.name, false, false, None)?;
    }

    // Conversion gain

    if let Some(conv_gain_str) = &cam_ctrl.conv_gain_str {
        if indi.camera_is_conversion_gain_str_supported(&device.name)? {
            indi.camera_set_conversion_gain_str(
                &device.name,
                conv_gain_str,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }
    }

    // Low noise mode

    if indi.camera_is_low_noise_supported(&device.name)? {
        indi.camera_set_low_noise(
            &device.name,
            cam_ctrl.low_noise,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // High fullwell mode

    if indi.camera_is_high_fullwell_supported(&device.name)? {
        indi.camera_set_high_fullwell(
            &device.name,
            cam_ctrl.high_fullwell,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Polling period

    if indi.device_is_polling_period_supported(&device.name)? {
        indi.device_set_polling_period(&device.name, 500, true, None)?;
    }

    // Frame type

    use crate::image::raw::*; // for FrameType::
    let frame_type = match frame.frame_type {
        FrameType::Lights => indi::FrameType::Light,
        FrameType::Flats  => indi::FrameType::Flat,
        FrameType::Darks  => indi::FrameType::Dark,
        FrameType::Biases => indi::FrameType::Bias,
        FrameType::Undef  => panic!("Undefined frame type"),
    };

    indi.camera_set_frame_type(
        &device.name,
        cam_ccd,
        frame_type,
        true,
        INDI_SET_PROP_TIMEOUT
    )?;

    // Frame size

    if indi.camera_is_frame_supported(&device.name, cam_ccd)? {
        let (width, height) = indi.camera_get_max_frame_size(&device.name, cam_ccd)?;
        let crop_width = frame.crop.translate(width);
        let crop_height = frame.crop.translate(height);
        indi.camera_set_frame_size(
            &device.name,
            cam_ccd,
            (width - crop_width) / 2,
            (height - crop_height) / 2,
            crop_width,
            crop_height,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Make binning mode is alwais AVG (if camera supports it)

    if indi.camera_is_binning_mode_supported(&device.name, cam_ccd)?
    && frame.binning != Binning::Orig {
        indi.camera_set_binning_mode(
            &device.name,
            indi::BinningMode::Avg,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Binning

    if indi.camera_is_binning_supported(&device.name, cam_ccd)? {
        indi.camera_set_binning(
            &device.name,
            cam_ccd,
            frame.binning.get_ratio(),
            frame.binning.get_ratio(),
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Gain

    if indi.camera_is_gain_supported(&device.name)? {
        indi.camera_set_gain(
            &device.name,
            frame.gain,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Offset

    if indi.camera_is_offset_supported(&device.name)? {
        indi.camera_set_offset(
            &device.name,
            frame.offset as f64,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Capture format = RAW

    if indi.camera_is_capture_format_supported(&device.name)? {
        indi.camera_set_capture_format(
            &device.name,
            indi::CaptureFormat::Raw,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;
    }

    // Start exposure

    let shot_id = indi.camera_start_exposure(
        &device.name,
        indi::CamCcd::from_ccd_prop_name(&device.prop),
        frame.exposure()
    )?;

    Ok(shot_id)
}

pub fn abort_camera_exposure(
    indi:   &indi::Connection,
    device: &DeviceAndProp,
) -> anyhow::Result<()> {
    indi.camera_abort_exposure(
        &device.name,
        indi::CamCcd::from_ccd_prop_name(&device.prop)
    )?;
    Ok(())
}

