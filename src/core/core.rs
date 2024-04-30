use std::{
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering, AtomicU16 },
        RwLock,
        RwLockReadGuard,
        mpsc
    },
    any::Any
};
use crate::{
    core::consts::INDI_SET_PROP_TIMEOUT, guiding::{external_guider::*, phd2_conn, phd2_guider::*}, image::stars_offset::*, indi, options::*, utils::timer::*
};
use super::{
    frame_processing::*,
    mode_waiting::*,
    mode_tacking_pictures::*,
    mode_focusing::*,
    mode_mount_calibration::*,
};

#[derive(Clone)]
pub struct Progress {
    pub cur: usize,
    pub total: usize,
}

pub enum CoreEvent {
    Error(String),
    ModeChanged,
    ModeContinued,
    Propress(Option<Progress>),
    Focusing(FocusingStateEvent),
}

#[derive(PartialEq, Copy, Clone, Debug)]
pub enum ModeType {
    Waiting,
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
    Focusing,
    DitherCalibr
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
    fn notify_about_frame_processing_result(&mut self, _fp_result: &FrameProcessResult, _subscribers: &Arc<RwLock<Subscribers>>) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_guider_event(&mut self, _event: ExtGuiderEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
}

pub enum NotifyResult {
    Empty,
    ProgressChanges,
    ModeChanged,
    Finished { next_mode: Option<ModeBox> },
    StartFocusing,
    StartMountCalibr,
}

pub struct ModeData {
    pub mode:          ModeBox,
    pub finished_mode: Option<ModeBox>,
    pub aborted_mode:  Option<ModeBox>,
}

impl ModeData {
    fn new() -> Self {
        Self {
            mode:          Box::new(WaitingMode),
            finished_mode: None,
            aborted_mode:  None,
        }
    }
}

type SubscribersFun = dyn Fn(CoreEvent) + Send + Sync + 'static;
type FrameProcessResultFun = dyn Fn(FrameProcessResult) + Send + Sync + 'static;

pub struct Subscribers {
    items:     Vec<Box<SubscribersFun>>,
    frame_evt: Option<Box<FrameProcessResultFun>>,
}

impl Subscribers {
    fn new() -> Self {
        Self {
            items:     Vec::new(),
            frame_evt: None,
        }
    }

    fn add(&mut self, fun: impl Fn(CoreEvent) + Send + Sync + 'static) {
        self.items.push(Box::new(fun));
    }

    fn inform_error(&self, error_text: &str) {
        for s in &self.items {
            s(CoreEvent::Error(error_text.to_string()));
        }
    }

    fn inform_mode_changed(&self) {
        for s in &self.items {
            s(CoreEvent::ModeChanged);
        }
    }

    fn inform_progress(&self, progress: Option<Progress>) {
        for s in &self.items {
            s(CoreEvent::Propress(progress.clone()));
        }
    }

    fn inform_mode_continued(&self) {
        for s in &self.items {
            s(CoreEvent::ModeContinued);
        }
    }

    pub fn inform_focusing(&self, data: FocusingStateEvent) {
        for s in &self.items {
            s(CoreEvent::Focusing(data.clone()));
        }
    }
}

pub struct Core {
    indi:               Arc<indi::Connection>,
    phd2:               Arc<phd2_conn::Connection>,
    options:            Arc<RwLock<Options>>,
    mode_data:          Arc<RwLock<ModeData>>,
    subscribers:        Arc<RwLock<Subscribers>>,
    cur_frame:          Arc<ResultImage>,
    ref_stars:          Arc<Mutex<Option<Vec<Point>>>>,
    calibr_images:      Arc<Mutex<CalibrImages>>,
    live_stacking:      Arc<LiveStackingData>,
    timer:              Arc<Timer>,
    exp_stuck_wd:       Arc<AtomicU16>,
    img_proc_stop_flag: Arc<Mutex<Arc<AtomicBool>>>, // stop flag for last command

    /// commands for passing into frame processing thread
    img_cmds_sender:    mpsc::Sender<FrameProcessCommand>, // TODO: make API
    ext_guider:         Arc<Mutex<Option<Box<dyn ExternalGuider + Send>>>>,
}

impl Core {
    pub fn new(
        indi:            &Arc<indi::Connection>,
        options:         &Arc<RwLock<Options>>,
        img_cmds_sender: mpsc::Sender<FrameProcessCommand>
    ) -> Self {
        let result = Self {
            indi:               Arc::clone(indi),
            phd2:               Arc::new(phd2_conn::Connection::new()),
            options:            Arc::clone(options),
            mode_data:          Arc::new(RwLock::new(ModeData::new())),
            subscribers:        Arc::new(RwLock::new(Subscribers::new())),
            cur_frame:          Arc::new(ResultImage::new()),
            ref_stars:          Arc::new(Mutex::new(None)),
            calibr_images:      Arc::new(Mutex::new(CalibrImages::default())),
            live_stacking:      Arc::new(LiveStackingData::new()),
            timer:              Arc::new(Timer::new()),
            exp_stuck_wd:       Arc::new(AtomicU16::new(0)),
            img_proc_stop_flag: Arc::new(Mutex::new(Arc::new(AtomicBool::new(false)))),
            ext_guider:         Arc::new(Mutex::new(None)),
            img_cmds_sender,
        };
        result.connect_indi_events();
        result.start_taking_frames_restart_timer();
        result
    }

    pub fn phd2(&self) -> &Arc<phd2_conn::Connection> {
        &self.phd2
    }

    pub fn create_ext_guider(&self) -> anyhow::Result<()> {
        let options = self.options.read().unwrap();
        let mut ext_guider = self.ext_guider.lock().unwrap();
        if ext_guider.is_some() {
            return Err(anyhow::anyhow!("Already connected"));
        }
        match options.guiding.mode {
            GuidingMode::MainCamera =>
                return Err(anyhow::anyhow!("External guider in not selected")),
            GuidingMode::Phd2 => {
                let guider = Box::new(ExternalGuiderPhd2::new(&self.phd2));
                guider.connect()?;
                *ext_guider = Some(guider);
                drop(ext_guider);
                self.connect_ext_guider_events();
            }
        }
        Ok(())
    }

    pub fn connect_ext_guider_events(&self) {
        let mut ext_guider = self.ext_guider.lock().unwrap();
        if let Some(ext_guider) = &mut *ext_guider {
            let mode_data = Arc::clone(&self.mode_data);
            let subscribers = Arc::clone(&self.subscribers);
            let indi = Arc::clone(&self.indi);
            let options = Arc::clone(&self.options);
            let exp_stuck_wd = Arc::clone(&self.exp_stuck_wd);
            let img_proc_stop_flag = Arc::clone(&self.img_proc_stop_flag);
            ext_guider.connect_event_handler(Box::new(move |event| {
                log::info!("External guider event = {:?}", event);
                let result = || -> anyhow::Result<()> {
                    let mut mode = mode_data.write().unwrap();
                    let res = mode.mode.notify_guider_event(event)?;
                    Self::apply_change_result(
                        res,
                        &mut mode,
                        &indi,
                        &options,
                        &subscribers,
                        &img_proc_stop_flag
                    )?;
                    Ok(())
                } ();
                Self::process_error(
                    result, "Core::connect_ext_guider_events",
                    &mode_data, &subscribers, &exp_stuck_wd
                );
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
        result:       anyhow::Result<()>,
        context:      &str,
        mode_data:    &Arc<RwLock<ModeData>>,
        subscribers:  &Arc<RwLock<Subscribers>>,
        exp_stuck_wd: &Arc<AtomicU16>,
    ) {
        let Err(err) = result else { return; };
        log::error!("Error in {}: {}", context, err.to_string());

        log::info!("Aborting active mode...");
        Self::abort_active_mode_impl(mode_data, subscribers, exp_stuck_wd);
        log::info!("Active mode aborted!");

        log::info!("Inform about error...");
        let subscribers = subscribers.read().unwrap();
        subscribers.inform_error(&err.to_string());
        log::info!("Error has informed!");
    }

    pub fn connect_main_cam_proc_result_event(
        &self,
        fun: impl Fn(FrameProcessResult) + Send + Sync + 'static
    ) {
        let mut subscribers = self.subscribers.write().unwrap();
        assert!(subscribers.frame_evt.is_none());
        subscribers.frame_evt = Some(Box::new(fun));
    }

    fn connect_indi_events(&self) {
        let mode_data = Arc::clone(&self.mode_data);
        let exp_stuck_wd = Arc::clone(&self.exp_stuck_wd);
        let indi = Arc::clone(&self.indi);
        let options = Arc::clone(&self.options);
        let subscribers = Arc::clone(&self.subscribers);
        let cur_frame = Arc::clone(&self.cur_frame);
        let ref_stars = Arc::clone(&self.ref_stars);
        let calibr_images = Arc::clone(&self.calibr_images);
        let img_cmds_sender = self.img_cmds_sender.clone();
        let img_proc_stop_flag = Arc::clone(&self.img_proc_stop_flag);
        self.indi.subscribe_events(move |event| {
            let result = || -> anyhow::Result<()> {
                match event {
                    indi::Event::BlobStart(event) => {
                        let mut mode_data = mode_data.write().unwrap();
                        let result = mode_data.mode.notify_blob_start_event(&event)?;
                        Self::apply_change_result(
                            result,
                            &mut mode_data,
                            &indi,
                            &options,
                            &subscribers,
                            &img_proc_stop_flag
                        )?;
                    }
                    indi::Event::PropChange(prop_change) => {
                        if let indi::PropChange::Change {
                            value: indi::PropChangeValue{
                                prop_value: indi::PropValue::Blob(blob), ..
                            }, ..
                        } = &prop_change.change {
                            Self::process_indi_blob_event(
                                &indi, &blob, &prop_change.device_name,
                                &prop_change.prop_name, &mode_data,
                                &options, &cur_frame, &ref_stars,
                                &calibr_images, &subscribers,
                                &exp_stuck_wd, &img_proc_stop_flag,
                                &img_cmds_sender,
                            )?;
                        } else {
                            Self::process_indi_prop_change_event(
                                &prop_change, &mode_data, &indi, &options,
                                &subscribers, &exp_stuck_wd, &img_proc_stop_flag
                            )?;
                        }
                    },
                    _ => {}
                }
                Ok(())
            } ();
            Self::process_error(
                result, "Core::connect_indi_events",
                &mode_data, &subscribers, &exp_stuck_wd
            );
        });
    }

    fn start_taking_frames_restart_timer(&self) {
        const MAX_EXP_STACK_WD_CNT: u16 = 30;
        let exp_stuck_wd = Arc::clone(&self.exp_stuck_wd);
        let mode_data = Arc::clone(&self.mode_data);
        let indi = Arc::clone(&self.indi);
        let subscribers = Arc::clone(&self.subscribers);
        let img_proc_stop_flag = Arc::clone(&self.img_proc_stop_flag);
        self.timer.exec(1000, true, move || {
            let prev = exp_stuck_wd.fetch_update(
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
                let result = Self::restart_camera_exposure(
                    &indi,
                    &mode_data,
                    &img_proc_stop_flag
                );
                Self::process_error(
                    result, "Core::start_taking_frames_restart_timer",
                    &mode_data, &subscribers, &exp_stuck_wd
                );
            }
        });
    }

    fn process_indi_prop_change_event(
        prop_change:        &indi::PropChangeEvent,
        mode_data:          &Arc<RwLock<ModeData>>,
        indi:               &Arc<indi::Connection>,
        options:            &Arc<RwLock<Options>>,
        subscribers:        &Arc<RwLock<Subscribers>>,
        exp_stuck_wd:       &Arc<AtomicU16>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
    ) -> anyhow::Result<()> {
        let mut mode_data = mode_data.write().unwrap();
        let result = mode_data.mode.notify_indi_prop_change(&prop_change)?;
        Self::apply_change_result(
            result,
            &mut mode_data,
            indi,
            options,
            subscribers,
            img_proc_stop_flag
        )?;

        if let (indi::PropChange::Change { value, new_state, .. }, Some(cur_device))
        = (&prop_change.change, mode_data.mode.cam_device()) {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cur_device.prop);
            if indi::Connection::camera_is_exposure_property(&prop_change.prop_name, &value.elem_name, cam_ccd)
            && cur_device.name == *prop_change.device_name {
                // exposure = 0.0 and state = busy means exposure has ended
                // but still no blob received
                if value.prop_value.to_f64().unwrap_or(0.0) == 0.0
                && *new_state == indi::PropState::Busy {
                    _ = exp_stuck_wd.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
                } else {
                    exp_stuck_wd.store(0, Ordering::Relaxed);
                }
            }
        }
        Ok(())
    }

    fn process_indi_blob_event(
        indi:               &Arc<indi::Connection>,
        blob:               &Arc<indi::BlobPropValue>,
        device_name:        &str,
        device_prop:        &str,
        mode_data:          &Arc<RwLock<ModeData>>,
        options:            &Arc<RwLock<Options>>,
        cur_frame:          &Arc<ResultImage>,
        ref_stars:          &Arc<Mutex<Option<Vec<Point>>>>,
        calibr_images:      &Arc<Mutex<CalibrImages>>,
        subscribers:        &Arc<RwLock<Subscribers>>,
        exp_stuck_wd:       &Arc<AtomicU16>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
        frame_proc_sender:  &mpsc::Sender<FrameProcessCommand>,
    ) -> anyhow::Result<()> {
        if blob.data.is_empty() { return Ok(()); }
        log::debug!(
            "process_blob_event, device_name = {}, device_prop = {}, dl_time = {:.2}s",
            device_name, device_prop, blob.dl_time
        );

        let mut mode = mode_data.write().unwrap();
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
        Self::apply_change_result(
            res,
            &mut mode,
            indi,
            options,
            subscribers,
            img_proc_stop_flag,
        )?;
        if !should_be_processed {
            return Ok(());
        }

        let mut command_data = {
            let options = options.read().unwrap();
            let device = DeviceAndProp {
                name: device_name.to_string(),
                prop: device_prop.to_string(),
            };
            FrameProcessCommandData {
                mode_type:       mode.mode.get_type(),
                camera:          device,
                flags:           ProcessImageFlags::empty(),
                blob:            Arc::clone(blob),
                frame:           Arc::clone(cur_frame),
                stop_flag:       Arc::clone(&img_proc_stop_flag.lock().unwrap()),
                ref_stars:       Arc::clone(ref_stars),
                calibr_params:   options.calibr.into_params(),
                calibr_images:   Arc::clone(calibr_images),
                view_options:    options.preview.preview_params(),
                frame_options:   options.cam.frame.clone(),
                quality_options: Some(options.quality.clone()),
                fn_gen:          None,
                save_path:       None,
                raw_adder:       None,
                live_stacking:   None,
            }
        };

        mode.mode.complete_img_process_params(&mut command_data);
        command_data.stop_flag.store(false, Ordering::Relaxed);

        let result_fun = {
            let subscribers = Arc::clone(subscribers);
            let options = Arc::clone(&options);
            let mode_data = Arc::clone(&mode_data);
            let indi = Arc::clone(&indi);
            let exp_stuck_wd = Arc::clone(&exp_stuck_wd);
            let img_proc_stop_flag = Arc::clone(img_proc_stop_flag);
            move |res: FrameProcessResult| {
                if res.cmd_stop_flag.load(Ordering::Relaxed) {
                    return;
                }
                let mut mode = mode_data.write().unwrap();
                if Some(&res.camera) != mode.mode.cam_device() {
                    return;
                }
                if let Some(evt) = &subscribers.read().unwrap().frame_evt {
                    evt(res.clone());
                }
                let result = || -> anyhow::Result<()> {
                    let res = mode.mode.notify_about_frame_processing_result(
                        &res,
                        &subscribers
                    )?;
                    Self::apply_change_result(
                        res,
                        &mut mode,
                        &indi,
                        &options,
                        &subscribers,
                        &img_proc_stop_flag,
                    )?;
                    Ok(())
                } ();
                Self::process_error(
                    result, "Core::process_indi_blob_event",
                    &mode_data, &subscribers, &exp_stuck_wd
                );
            }
        };

        frame_proc_sender.send(FrameProcessCommand::ProcessImage {
            command: command_data,
            result_fun: Box::new(result_fun),
        }).unwrap();

        Ok(())
    }

    fn restart_camera_exposure(
        indi:               &Arc<indi::Connection>,
        mode_data:          &Arc<RwLock<ModeData>>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
    ) -> anyhow::Result<()> {
        log::error!("Beging camera exposure restarting...");
        let mode_data = mode_data.read().unwrap();
        let Some(cam_device) = mode_data.mode.cam_device() else { return Ok(()); };
        let Some(cur_exposure) = mode_data.mode.get_cur_exposure() else { return Ok(()); };
        abort_camera_exposure(indi, &cam_device, img_proc_stop_flag)?;
        if indi.camera_is_fast_toggle_supported(&cam_device.name)?
        && indi.camera_is_fast_toggle_enabled(&cam_device.name)? {
            let prop_info = indi.camera_get_fast_frames_count_prop_info(
                &cam_device.name,
            ).unwrap();
            indi.camera_set_fast_frames_count(
                &cam_device.name,
                prop_info.max as usize,
                true,
                INDI_SET_PROP_TIMEOUT,
            )?;
        }
        start_camera_exposure(
            indi,
            cam_device,
            cur_exposure,
            img_proc_stop_flag,
        )?;
        log::error!("Camera exposure restarted!");
        Ok(())
    }

    pub fn subscribe_events(
        &self,
        fun: impl Fn(CoreEvent) + Send + Sync + 'static
    ) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.add(fun);
    }

    pub fn start_single_shot(&self) -> anyhow::Result<()> {
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            None,
            CameraMode::SingleShot,
            &self.options,
            &self.img_proc_stop_flag,
        );
        mode.start()?;
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode = Box::new(mode);
        mode_data.finished_mode = None;
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    pub fn start_live_view(&self) -> anyhow::Result<()> {
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            Some(&self.timer),
            CameraMode::LiveView,
            &self.options,
            &self.img_proc_stop_flag,
        );
        mode.start()?;
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode = Box::new(mode);
        mode_data.finished_mode = None;
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    pub fn start_saving_raw_frames(&self) -> anyhow::Result<()> {
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            Some(&self.timer),
            CameraMode::SavingRawFrames,
            &self.options,
            &self.img_proc_stop_flag,
        );
        mode.set_guider(&self.ext_guider);
        mode.set_ref_stars(&self.ref_stars);
        mode.start()?;
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode = Box::new(mode);
        mode_data.aborted_mode = None;
        mode_data.finished_mode = None;
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    pub fn start_live_stacking(&self) -> anyhow::Result<()> {
        let mut mode = TackingPicturesMode::new(
            &self.indi,
            Some(&self.timer),
            CameraMode::LiveStacking,
            &self.options,
            &self.img_proc_stop_flag,
        );
        mode.set_guider(&self.ext_guider);
        mode.set_ref_stars(&self.ref_stars);
        mode.set_live_stacking(&self.live_stacking);
        mode.start()?;
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode = Box::new(mode);
        mode_data.aborted_mode = None;
        mode_data.finished_mode = None;
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    pub fn start_focusing(&self) -> anyhow::Result<()> {
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode.abort()?;
        let mut mode = FocusingMode::new(
            &self.indi,
            &self.options,
            &self.img_proc_stop_flag,
            None
        );
        mode.start()?;
        mode_data.mode = Box::new(mode);
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    pub fn start_mount_calibr(&self) -> anyhow::Result<()> {
        let mut mode_data = self.mode_data.write().unwrap();
        mode_data.mode.abort()?;
        let mut mode = MountCalibrMode::new(
            &self.indi,
            &self.options,
            &self.img_proc_stop_flag,
            None
        );
        mode.start()?;
        mode_data.mode = Box::new(mode);
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    pub fn abort_active_mode(&self) {
        Self::abort_active_mode_impl(
            &self.mode_data,
            &self.subscribers,
            &self.exp_stuck_wd
        );
    }

    fn abort_active_mode_impl(
        mode_data:    &Arc<RwLock<ModeData>>,
        subscribers:  &Arc<RwLock<Subscribers>>,
        exp_stuck_wd: &Arc<AtomicU16>,
    ) {
        let mut mode_data = mode_data.write().unwrap();
        if mode_data.mode.get_type() == ModeType::Waiting {
            return;
        }
        _ = mode_data.mode.abort();
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
        let subscribers = subscribers.read().unwrap();
        subscribers.inform_mode_changed();
        exp_stuck_wd.store(0, Ordering::Relaxed);
    }

    pub fn continue_prev_mode(&self) -> anyhow::Result<()> {
        let mut mode_data = self.mode_data.write().unwrap();
        let Some(perv_mode) = mode_data.aborted_mode.take() else {
            anyhow::bail!("Aborted state is empty");
        };
        mode_data.mode = perv_mode;
        mode_data.mode.continue_work()?;
        let progress = mode_data.mode.progress();
        drop(mode_data);
        let subscribers = self.subscribers.read().unwrap();
        subscribers.inform_mode_continued();
        subscribers.inform_progress(progress);
        subscribers.inform_mode_changed();
        Ok(())
    }

    fn apply_change_result(
        result:             NotifyResult,
        mode_data:          &mut ModeData,
        indi:               &Arc<indi::Connection>,
        options:            &Arc<RwLock<Options>>,
        subscribers:        &Arc<RwLock<Subscribers>>,
        img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
    ) -> anyhow::Result<()> {
        let mut mode_changed = false;
        let mut progress_changed = false;
        match result {
            NotifyResult::ProgressChanges => {
                progress_changed = true;
            }
            NotifyResult::ModeChanged => {
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::Finished { next_mode } => {
                let next_is_none = next_mode.is_none();
                let prev_mode = std::mem::replace(
                    &mut mode_data.mode,
                    next_mode.unwrap_or_else(|| Box::new(WaitingMode))
                );
                if next_is_none {
                    mode_data.finished_mode = Some(prev_mode);
                }
                mode_data.mode.continue_work()?;
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartFocusing => {
                mode_data.mode.abort()?;
                let prev_mode = std::mem::replace(&mut mode_data.mode, Box::new(WaitingMode));
                let mut mode = FocusingMode::new(
                    indi,
                    options,
                    img_proc_stop_flag,
                    Some(prev_mode)
                );
                mode.start()?;
                mode_data.mode = Box::new(mode);
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartMountCalibr => {
                mode_data.mode.abort()?;
                let prev_mode = std::mem::replace(&mut mode_data.mode, Box::new(WaitingMode));
                let mut mode = MountCalibrMode::new(
                    indi,
                    options,
                    img_proc_stop_flag,
                    Some(prev_mode)
                );
                mode.start()?;
                mode_data.mode = Box::new(mode);
                mode_changed = true;
                progress_changed = true;
            }
            _ => {}
        }

        if mode_changed || progress_changed {
            let subscribers = subscribers.read().unwrap();
            if mode_changed {
                subscribers.inform_mode_changed();
            }
            if progress_changed {
                subscribers.inform_progress(mode_data.mode.progress());
            }
        }

        Ok(())
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        log::info!("Core dropped");
    }
}

///////////////////////////////////////////////////////////////////////////////

pub fn start_taking_shots(
    indi:               &indi::Connection,
    frame:              &FrameOptions,
    device:             &DeviceAndProp,
    img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
    continuously:       bool,
) -> anyhow::Result<()> {
    indi.command_enable_blob(
        &device.name,
        None,
        indi::BlobEnable::Also
    )?;
    if indi.camera_is_fast_toggle_supported(&device.name,)? {
        let use_fast_toggle =
            continuously && !frame.have_to_use_delay();
        indi.camera_enable_fast_toggle(
            &device.name,
            use_fast_toggle,
            true,
            INDI_SET_PROP_TIMEOUT,
        )?;
        if use_fast_toggle {
            let prop_info = indi.camera_get_fast_frames_count_prop_info(
                &device.name,
            )?;
            indi.camera_set_fast_frames_count(
                &device.name,
                prop_info.max as usize,
                true,
                INDI_SET_PROP_TIMEOUT,
            )?;
        }
    }
    apply_camera_options_and_take_shot(
        indi,
        device,
        frame,
        img_proc_stop_flag,
    )?;
    Ok(())
}

pub fn apply_camera_options_and_take_shot(
    indi:               &indi::Connection,
    device:             &DeviceAndProp,
    frame:              &FrameOptions,
    img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
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

    start_camera_exposure(
        indi,
        device,
        frame.exposure(),
        &img_proc_stop_flag
    )?;

    Ok(())
}

pub fn start_camera_exposure(
    indi:               &indi::Connection,
    device:             &DeviceAndProp,
    exposure:           f64,
    img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
) -> anyhow::Result<()> {
    *img_proc_stop_flag.lock().unwrap() = Arc::new(AtomicBool::new(false));
    indi.camera_start_exposure(
        &device.name,
        indi::CamCcd::from_ccd_prop_name(&device.prop),
        exposure
    )?;
    Ok(())
}

pub fn abort_camera_exposure(
    indi:               &indi::Connection,
    device:             &DeviceAndProp,
    img_proc_stop_flag: &Arc<Mutex<Arc<AtomicBool>>>,
) -> anyhow::Result<()> {
    indi.camera_abort_exposure(
        &device.name,
        indi::CamCcd::from_ccd_prop_name(&device.prop)
    )?;
    img_proc_stop_flag.lock().unwrap().store(true, Ordering::Relaxed);
    Ok(())
}

