use std::{
    any::Any, path::Path, sync::{
        atomic::{AtomicBool, AtomicU16, Ordering }, mpsc, Arc, Mutex, RwLock, RwLockReadGuard
    }
};
use gtk::glib::PropertySet;

use crate::{
    core::consts::*, guiding::{external_guider::*, phd2_conn, phd2_guider::*}, image::stars_offset::*, indi, options::*, ui::sky_map::math::EqCoord, utils::timer::*
};
use super::{
    events::*, frame_processing::*, mode_capture_platesolve::*, mode_darks_library::*, mode_focusing::*, mode_goto::*, mode_mount_calibration::*, mode_polar_align::PolarAlignMode, mode_tacking_pictures::*, mode_waiting::*
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
    fn take_next_mode(&mut self) -> Option<ModeBox> { None }
    fn set_or_correct_value(&mut self, _value: &mut dyn Any) {}
    fn complete_img_process_params(&self, _cmd: &mut FrameProcessCommandData) {}
    fn notify_indi_prop_change(&mut self, _prop_change: &indi::PropChangeEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_blob_start_event(&mut self, _event: &indi::BlobStartEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_before_frame_processing_start(&mut self, _should_be_processed: &mut bool) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_about_frame_processing_result(&mut self, _fp_result: &FrameProcessResult) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_guider_event(&mut self, _event: ExtGuiderEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
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
    phd2:               Arc<phd2_conn::Connection>,
    options:            Arc<RwLock<Options>>,
    mode_data:          RwLock<ModeData>,
    subscribers:        Arc<EventSubscriptions>,
    cur_frame:          Arc<ResultImage>,
    ref_stars:          Arc<Mutex<Option<Vec<Point>>>>,
    calibr_data:        Arc<Mutex<CalibrData>>,
    live_stacking:      Arc<LiveStackingData>,
    timer:              Arc<Timer>,
    exp_stuck_wd:       AtomicU16,
    img_proc_stop_flag: Mutex<Arc<AtomicBool>>, // stop flag for last command

    /// commands for passing into frame processing thread
    img_cmds_sender:    mpsc::Sender<FrameProcessCommand>, // TODO: make API
    ext_guider:         Arc<Mutex<Option<Box<dyn ExternalGuider + Send>>>>,
}

impl Core {
    pub fn new(
        indi:            &Arc<indi::Connection>,
        options:         &Arc<RwLock<Options>>,
        img_cmds_sender: mpsc::Sender<FrameProcessCommand>
    ) -> Arc<Self> {
        let result = Arc::new(Self {
            indi:               Arc::clone(indi),
            phd2:               Arc::new(phd2_conn::Connection::new()),
            options:            Arc::clone(options),
            mode_data:          RwLock::new(ModeData::new()),
            subscribers:        Arc::new(EventSubscriptions::new()),
            cur_frame:          Arc::new(ResultImage::new()),
            ref_stars:          Arc::new(Mutex::new(None)),
            calibr_data:        Arc::new(Mutex::new(CalibrData::default())),
            live_stacking:      Arc::new(LiveStackingData::new()),
            timer:              Arc::new(Timer::new()),
            exp_stuck_wd:       AtomicU16::new(0),
            img_proc_stop_flag: Mutex::new(Arc::new(AtomicBool::new(false))),
            ext_guider:         Arc::new(Mutex::new(None)),
            img_cmds_sender,
        });
        result.connect_indi_events();
        result.connect_1s_timer_event();
        result.start_taking_frames_restart_timer();
        result
    }

    pub fn stop(self: &Arc<Self>) {
        self.abort_active_mode();
        self.timer.clear();
    }

    pub fn phd2(&self) -> &Arc<phd2_conn::Connection> {
        &self.phd2
    }

    pub fn create_ext_guider(self: &Arc<Self>, guider: ExtGuiderType) -> anyhow::Result<()> {
        let mut ext_guider = self.ext_guider.lock().unwrap();

        if let Some(ext_guider) = &mut *ext_guider {
            ext_guider.disconnect()?;
        }

        match guider {
            ExtGuiderType::Phd2 => {
                let guider = Box::new(ExternalGuiderPhd2::new(&self.phd2));
                guider.connect()?;
                *ext_guider = Some(guider);
                drop(ext_guider);
                self.connect_ext_guider_events();
            }
        }
        Ok(())
    }

    pub fn connect_ext_guider_events(self: &Arc<Self>) {
        let mut ext_guider = self.ext_guider.lock().unwrap();
        if let Some(ext_guider) = &mut *ext_guider {
            let self_ = Arc::clone(self);
            ext_guider.connect_event_handler(Box::new(move |event| {
                log::info!("External guider event = {:?}", event);
                let result = || -> anyhow::Result<()> {
                    let mut mode = self_.mode_data.write().unwrap();
                    let res = mode.mode.notify_guider_event(event)?;
                    self_.apply_change_result(res, &mut mode)?;
                    Ok(())
                } ();
                self_.process_error(result, "Core::connect_ext_guider_events");
            }));
        }
    }

    pub fn disconnect_ext_guider(&self) -> anyhow::Result<()> {
        let mut ext_guider = self.ext_guider.lock().unwrap();
        if let Some(guider) = ext_guider.take() {
            guider.disconnect()?;
        } else {
            return Err(anyhow::anyhow!("Not connected"));
        }
        Ok(())
    }

    pub fn stop_img_process_thread(&self) -> anyhow::Result<()> {
        self.img_cmds_sender
            .send(FrameProcessCommand::Exit)
            .map_err(|_| anyhow::anyhow!("Can't send exit command"))?;
        Ok(())
    }

    pub fn mode_data(&self) -> RwLockReadGuard<ModeData> {
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
        self.subscribers.notify(Event::Error(err.to_string()));
        log::info!("Error has informed!");
    }

    fn connect_1s_timer_event(self: &Arc<Self>) {
        let self_ = Arc::clone(self);
        self.timer.exec(1000, true, move || {
            let result = || -> anyhow::Result<()> {
                let mut mode_data = self_.mode_data.write().unwrap();
                let result = mode_data.mode.notify_timer_1s()?;
                self_.apply_change_result(result, &mut mode_data)?;
                Ok(())
            }();
            self_.process_error(result, "Core::connect_events (timer closure)");
        });
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
                                &blob,
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

    fn start_taking_frames_restart_timer(self: &Arc<Self>) {
        const MAX_EXP_STACK_WD_CNT: u16 = 30;
        let self_ = Arc::clone(self);
        self.timer.exec(1000, true, move || {
            let prev = self_.exp_stuck_wd.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |v| {
                    if v == 0 {
                        None
                    } else if v == MAX_EXP_STACK_WD_CNT {
                        Some(0)
                    } else {
                        Some(v+1)
                    }
                }
            );
            // Restart exposure if image can't be downloaded
            // from camera during 30 seconds
            if prev == Ok(MAX_EXP_STACK_WD_CNT) {
                let result = self_.restart_camera_exposure();
                self_.process_error(result, "Core::start_taking_frames_restart_timer");
            }
        });
    }

    fn process_indi_prop_change_event(
        self:        &Arc<Self>,
        prop_change: &indi::PropChangeEvent,
    ) -> anyhow::Result<()> {
        let mut mode_data = self.mode_data.write().unwrap();
        let result = mode_data.mode.notify_indi_prop_change(&prop_change)?;
        self.apply_change_result(result, &mut mode_data)?;

        if let (indi::PropChange::Change { value, new_state, .. }, Some(cur_device))
        = (&prop_change.change, mode_data.mode.cam_device()) {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cur_device.prop);
            if indi::Connection::camera_is_exposure_property(&prop_change.prop_name, &value.elem_name, cam_ccd)
            && cur_device.name == *prop_change.device_name {
                // exposure = 0.0 and state = busy means exposure has ended
                // but still no blob received
                if value.prop_value.to_f64().unwrap_or(0.0) == 0.0
                && *new_state == indi::PropState::Busy {
                    _ = self.exp_stuck_wd.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
                } else {
                    self.exp_stuck_wd.store(0, Ordering::Relaxed);
                }
            }
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
        let res = mode.mode.notify_before_frame_processing_start(&mut should_be_processed)?;
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

            let calibr_params = Some(CalibrParams {
                extract_dark:  options.calibr.dark_frame_en,
                dark_lib_path: options.calibr.dark_library_path.clone(),
                flat_fname:    None,
                sar_hot_pixs:  options.calibr.hot_pixels,
            });

            let new_stop_flag = Arc::new(AtomicBool::new(false));
            *self.img_proc_stop_flag.lock().unwrap() = Arc::clone(&new_stop_flag);

            FrameProcessCommandData {
                mode_type:       mode.mode.get_type(),
                camera:          device,
                flags:           ProcessImageFlags::empty(),
                img_source:      ImageSource::Blob(Arc::clone(blob)),
                frame:           Arc::clone(&self.cur_frame),
                stop_flag:       new_stop_flag,
                ref_stars:       Arc::clone(&self.ref_stars),
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
            move |res: FrameProcessResult| self_.frame_process_result_handler(res)
        };

        frame_proc_sender.send(FrameProcessCommand::ProcessImage {
            command: command_data,
            result_fun: Box::new(result_fun),
        }).unwrap();

        Ok(())
    }

    fn frame_process_result_handler(self: &Arc<Self>, res: FrameProcessResult) {
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

        self.subscribers.notify(
            Event::FrameProcessing(res.clone())
        );

        let result = || -> anyhow::Result<()> {
            let res = mode.mode.notify_about_frame_processing_result(&res)?;
            self.apply_change_result(res, &mut mode)?;
            Ok(())
        } ();
        drop(mode);
        self.process_error(result, "Core::process_indi_blob_event");
    }

    fn restart_camera_exposure(self: &Arc<Self>) -> anyhow::Result<()> {
        log::error!("Beging camera exposure restarting...");
        let mode_data = self.mode_data.read().unwrap();
        let Some(cam_device) = mode_data.mode.cam_device() else { return Ok(()); };
        let Some(cur_exposure) = mode_data.mode.get_cur_exposure() else { return Ok(()); };
        abort_camera_exposure(&self.indi, &cam_device)?;
        if self.indi.camera_is_fast_toggle_supported(&cam_device.name)?
        && self.indi.camera_is_fast_toggle_enabled(&cam_device.name)? {
            let prop_info = self.indi.camera_get_fast_frames_count_prop_info(
                &cam_device.name,
            ).unwrap();
            self.indi.camera_set_fast_frames_count(
                &cam_device.name,
                prop_info.max as usize,
                true,
                INDI_SET_PROP_TIMEOUT,
            )?;
        }
        start_camera_exposure(&self.indi, cam_device, cur_exposure)?;
        log::error!("Camera exposure restarted!");
        Ok(())
    }

    pub fn event_subscriptions(&self) -> Arc<EventSubscriptions> {
        self.subscribers.clone()
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
        self.subscribers.notify(Event::Progress(progress, mode_type));
        self.subscribers.notify(Event::ModeChanged);


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
        });

        let command = FrameProcessCommandData {
            mode_type:       ModeType::OpeningImgFile,
            camera:          DeviceAndProp::default(),
            flags:           ProcessImageFlags::empty(),
            img_source:      ImageSource::FileName(file_name.to_path_buf()),
            frame:           Arc::clone(&self.cur_frame),
            stop_flag:       new_stop_flag,
            ref_stars:       Arc::clone(&self.ref_stars),
            calibr_data:     Arc::clone(&self.calibr_data),
            view_options:    options.preview.preview_params(),
            frame_options:   options.cam.frame.clone(),
            quality_options: None,
            live_stacking:   None,
            calibr_params,
        };

        let result_fun = {
            let self_ = Arc::clone(self);
            move |res: FrameProcessResult| self_.frame_process_result_handler(res)
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
            &self.subscribers,
            CameraMode::SingleShot,
            &self.options,
        )?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_live_view(&self) -> anyhow::Result<()> {
        let mode = TackingPicturesMode::new(
            &self.indi,
            &self.subscribers,
            CameraMode::LiveView,
            &self.options,
        )?;
        self.start_new_mode(mode, false, true)?;
        Ok(())
    }

    pub fn start_saving_raw_frames(&self) -> anyhow::Result<()> {
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            &self.subscribers,
            CameraMode::SavingRawFrames,
            &self.options,
        )?;
        self.live_stacking.clear();
        mode.set_guider(&self.ext_guider);
        mode.set_ref_stars(&self.ref_stars);
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_live_stacking(&self) -> anyhow::Result<()> {
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            &self.subscribers,
            CameraMode::LiveStacking,
            &self.options,
        )?;
        self.live_stacking.clear();
        mode.set_guider(&self.ext_guider);
        mode.set_ref_stars(&self.ref_stars);
        mode.set_live_stacking(&self.live_stacking);
        self.start_new_mode(mode, true, true)?;
        Ok(())
    }

    pub fn start_focusing(&self) -> anyhow::Result<()> {
        self.mode_data.write().unwrap().mode.abort()?;
        let mode = FocusingMode::new(&self.indi, &self.options, &self.subscribers, None)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_mount_calibr(&self) -> anyhow::Result<()> {
        self.mode_data.write().unwrap().mode.abort()?;
        let mode = MountCalibrMode::new(&self.indi, &self.options, None)?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_creating_dark_library(
        &self,
        dark_lib_mode: DarkLibMode,
        program: &[MasterFileCreationProgramItem]
    ) -> anyhow::Result<()> {
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
        self.mode_data.write().unwrap().mode.abort()?;
        let mode = GotoMode::new(
            GotoDestination::Coord(eq_coord.clone()),
            config,
            &self.options,
            &self.indi,
            &self.subscribers,
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
        self.mode_data.write().unwrap().mode.abort()?;
        let mode = GotoMode::new(
            GotoDestination::Image{
                image: Arc::clone(&self.cur_frame.image),
                info: Arc::clone(light_frame_info),
            },
            GotoConfig::GotoPlateSolveAndCorrect,
            &self.options,
            &self.indi,
            &self.subscribers,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_capture_and_platesolve(&self) -> anyhow::Result<()> {
        let mode = CapturePlatesolveMode::new(
            &self.options,
            &self.indi,
            &self.subscribers,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn start_polar_alignment(&self) -> anyhow::Result<()> {
        let mode = PolarAlignMode::new(
            &self.indi,
            &self.options,
            &self.subscribers,
        )?;
        self.start_new_mode(mode, false, false)?;
        Ok(())
    }

    pub fn init_cam_telescope_data(&self) -> anyhow::Result<()> {
        if self.indi.state() != indi::ConnState::Connected {
            return Ok(());
        }
        let mode_data = self.mode_data.write().unwrap();
        let Some(cam_device) = &mode_data.mode.cam_device() else {
            return Ok(());
        };
        let options = self.options.read().unwrap();
        self.init_cam_telescope_data_impl(&cam_device.name, &options)?;
        Ok(())
    }

    fn init_cam_telescope_data_impl(&self, cam_name: &str, options: &Options) -> anyhow::Result<()> {
        let focal_len = options.telescope.real_focal_length();
        // Aperture info for simulator only
        // TODO: real aperture config
        let aperture = 0.2 * focal_len;
        self.indi.camera_set_telescope_info(
            cam_name,
            focal_len,
            aperture,
            false,
            INDI_SET_PROP_TIMEOUT
        )?;
        Ok(())
    }

    fn init_cam_for_mode(&self, mode: &dyn Mode) -> anyhow::Result<()> {
        let Some(cam_device) = &mode.cam_device() else {
            return Ok(());
        };

        // Disable fast toggle

        self.indi.camera_enable_fast_toggle(
            &cam_device.name,
            false, // <- do not use fast toggle
            true,
            INDI_SET_PROP_TIMEOUT,
        )?;

        // Enable blob

        self.indi.command_enable_blob(
            &cam_device.name,
            None,
            indi::BlobEnable::Also,
        )?;

        // Set telescope info into camera props

        if self.indi.camera_is_telescope_info_supported(&cam_device.name)? {
            let options = self.options.read().unwrap();
            self.init_cam_telescope_data_impl(&cam_device.name, &options)?;
        }

        Ok(())
    }

    pub fn abort_active_mode(self: &Arc<Self>) {
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
        self.subscribers.notify(Event::ModeChanged);
        self.exp_stuck_wd.store(0, Ordering::Relaxed);
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
        self.subscribers.notify(Event::ModeContinued);
        self.subscribers.notify(Event::Progress(progress, mode_type));
        self.subscribers.notify(Event::ModeChanged);
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
                let mut mode = FocusingMode::new(&self.indi, &self.options, &self.subscribers, Some(prev_mode))?;
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
            self.subscribers.notify(Event::ModeChanged);
        }
        if let Some((finished_progress, finished_mode_type)) = finished_progress_and_type {
            self.subscribers.notify(Event::Progress(
                finished_progress,
                finished_mode_type,
            ));
        } else if progress_changed || mode_changed {
            self.subscribers.notify(Event::Progress(
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
        let mut mode = TackingPicturesMode::new(&self.indi, &self.subscribers, mode, &self.options)?;
        mode.set_dark_creation_program_item(program_item);
        mode.set_next_mode(Some(prev_mode));
        self.init_cam_for_mode(&mode)?;
        mode.start()?;
        mode_data.mode = Box::new(mode);
        Ok(())
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        log::info!("Core dropped");
    }
}

///////////////////////////////////////////////////////////////////////////////

pub fn apply_camera_options_and_take_shot(
    indi:   &indi::Connection,
    device: &DeviceAndProp,
    frame:  &FrameOptions,
) -> anyhow::Result<()> {
    let cam_ccd = indi::CamCcd::from_ccd_prop_name(&device.prop);

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

    // Low noise mode

    if indi.camera_is_low_noise_ctrl_supported(&device.name)? {
        indi.camera_control_low_noise(
            &device.name,
            frame.low_noise,
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

    start_camera_exposure(indi, device, frame.exposure())?;

    Ok(())
}

pub fn start_camera_exposure(
    indi:     &indi::Connection,
    device:   &DeviceAndProp,
    exposure: f64,
) -> anyhow::Result<()> {
    indi.camera_start_exposure(
        &device.name,
        indi::CamCcd::from_ccd_prop_name(&device.prop),
        exposure
    )?;
    Ok(())
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

