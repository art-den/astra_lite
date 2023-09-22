use std::{
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering, AtomicU16 }, RwLock, RwLockReadGuard, mpsc},
    collections::VecDeque,
    any::Any, f64::consts::PI, path::PathBuf, thread::JoinHandle,
};
use chrono::Utc;
use itertools::Itertools;

use crate::{
    gui_camera::*,
    options::*,
    indi_api,
    image_raw::{FrameType, RawAdder},
    image_info::{LightFrameInfo, Stars},
    math::*,
    stars_offset::*,
    image_processing::*,
    io_utils::*,
    phd2_conn::*,
};

#[derive(Clone)]
pub struct Progress {
    pub cur: usize,
    pub total: usize,
}

#[derive(Clone)]
pub struct FocusingEvt {
    pub samples: Vec<FocuserSample>,
    pub coeffs:  Option<SquareCoeffs>,
    pub result:  Option<f64>,
}

pub enum StateEvent {
    Error(String),
    ModeChanged,
    ModeContinued,
    Propress(Option<Progress>),
    Focusing(FocusingEvt),
    FocusResultValue{ value: f64 },
}

#[derive(PartialEq, Copy, Clone)]
pub enum ModeType {
    Waiting,
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
    Focusing,
    DitherCalibr
}

type SubscribersFun = dyn Fn(StateEvent) + Send + Sync + 'static;
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

    fn add(&mut self, fun: impl Fn(StateEvent) + Send + Sync + 'static) {
        self.items.push(Box::new(fun));
    }

    fn inform_error(&self, error_text: &str) {
        for s in &self.items {
            s(StateEvent::Error(error_text.to_string()));
        }
    }

    fn inform_mode_changed(&self) {
        for s in &self.items {
            s(StateEvent::ModeChanged);
        }
    }

    fn inform_progress(&self, progress: Option<Progress>) {
        for s in &self.items {
            s(StateEvent::Propress(progress.clone()));
        }
    }

    fn inform_mode_continued(&self) {
        for s in &self.items {
            s(StateEvent::ModeContinued);
        }
    }

    fn inform_focusing(&self, data: FocusingEvt) {
        for s in &self.items {
            s(StateEvent::Focusing(data.clone()));
        }
    }

    fn inform_focusing_result(&self, value: f64) {
        for s in &self.items {
            s(StateEvent::FocusResultValue{value});
        }
    }
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
    fn notify_indi_prop_change(&mut self, _prop_change: &indi_api::PropChangeEvent) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_blob_start_event(&mut self, _event: &indi_api::BlobStartEvent) -> anyhow::Result<()> { Ok(()) }
    fn notify_about_frame_processing_started(&mut self) -> anyhow::Result<()> { Ok(()) }
    fn notify_about_light_frame_info(&mut self, _info: &LightFrameInfo, _subscribers: &Arc<RwLock<Subscribers>>) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_about_frame_processing_finished(&mut self, _frame_is_ok: bool) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
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

pub enum ExtGuiderType {
    Phd2,
}

pub trait ExternalGuider {
    fn get_type(&self) -> ExtGuiderType;
    fn connect(&self) -> anyhow::Result<()>;
    fn pause_guiding(&self, pause: bool) -> anyhow::Result<()>;
    fn start_dithering(&self) -> anyhow::Result<()>;
    fn disconnect(&self) -> anyhow::Result<()>;
}

struct ExternalGuiderPhd2 {
    phd2: Arc<Phd2Conn>
}

impl ExternalGuider for ExternalGuiderPhd2 {
    fn get_type(&self) -> ExtGuiderType {
        ExtGuiderType::Phd2
    }

    fn connect(&self) -> anyhow::Result<()> {
        self.phd2.work("127.0.0.1", 4400)?;
        Ok(())
    }

    fn pause_guiding(&self, _pause: bool) -> anyhow::Result<()> {
        todo!()
    }

    fn start_dithering(&self) -> anyhow::Result<()> {
        todo!()
    }

    fn disconnect(&self) -> anyhow::Result<()> {
        self.phd2.stop()?;
        Ok(())
    }
}

pub struct State {
    indi:            Arc<indi_api::Connection>,
    phd2:            Arc<Phd2Conn>,
    options:         Arc<RwLock<Options>>,
    mode_data:       Arc<RwLock<ModeData>>,
    subscribers:     Arc<RwLock<Subscribers>>,
    cur_frame:       Arc<ResultImage>,
    ref_stars:       Arc<Mutex<Option<Vec<Point>>>>,
    calibr_images:   Arc<Mutex<CalibrImages>>,
    live_stacking:   Arc<LiveStackingData>,
    timer:           Arc<Timer>,
    exp_stuck_wd:    Arc<AtomicU16>,
    process_thread:  Option<JoinHandle<()>>,
    img_cmds_sender: mpsc::Sender<FrameProcessCommand>,
    ext_guider:      Arc<Mutex<Option<Box<dyn ExternalGuider>>>>,
}

impl State {
    pub fn new(
        indi:    &Arc<indi_api::Connection>,
        options: &Arc<RwLock<Options>>
    ) -> Self {
        let (img_cmds_sender, process_thread) = start_main_cam_frame_processing_thread();
        let result = Self {
            indi:           Arc::clone(indi),
            phd2:      Arc::new(Phd2Conn::new()),
            options:        Arc::clone(options),
            mode_data:      Arc::new(RwLock::new(ModeData::new())),
            subscribers:    Arc::new(RwLock::new(Subscribers::new())),
            cur_frame:      Arc::new(ResultImage::new()),
            ref_stars:      Arc::new(Mutex::new(None)),
            calibr_images:  Arc::new(Mutex::new(CalibrImages::default())),
            live_stacking:  Arc::new(LiveStackingData::new()),
            timer:          Arc::new(Timer::new()),
            exp_stuck_wd:   Arc::new(AtomicU16::new(0)),
            process_thread: Some(process_thread),
            img_cmds_sender,
            ext_guider:     Arc::new(Mutex::new(None)),
        };
        result.connect_indi_events();
        result.start_taking_frames_restart_timer();
        result
    }

    pub fn phd2(&self) -> &Arc<Phd2Conn> {
        &self.phd2
    }

    pub fn connect_ext_guider(&self) -> anyhow::Result<()> {
        let options = self.options.read().unwrap();
        let mut ext_guider = self.ext_guider.lock().unwrap();
        if ext_guider.is_some() {
            return Err(anyhow::anyhow!("Already connected"));
        }
        match options.guiding.mode {
            GuidingMode::MainCamera =>
                return Err(anyhow::anyhow!("External guider in not selected")),
            GuidingMode::Phd2 => {
                let guider = Box::new(ExternalGuiderPhd2 {
                    phd2: Arc::clone(&self.phd2)
                });
                guider.connect()?;
                *ext_guider = Some(guider);
            }
        }
        Ok(())
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

    pub fn ext_guider(&self) -> &Arc<Mutex<Option<Box<dyn ExternalGuider>>>> {
        &self.ext_guider
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
        mode_data:    &Arc<RwLock<ModeData>>,
        subscribers:  &Arc<RwLock<Subscribers>>,
        exp_stuck_wd: &Arc<AtomicU16>,
    ) {
        let Err(err) = result else { return; };
        Self::abort_active_mode_impl(mode_data, subscribers, exp_stuck_wd);
        let subscribers = subscribers.read().unwrap();
        subscribers.inform_error(&err.to_string());
    }

    pub fn connect_main_cam_proc_result_event(&self, fun: impl Fn(FrameProcessResult) + Send + Sync + 'static) {
        let mut subscribers = self.subscribers.write().unwrap();
        assert!(subscribers.frame_evt.is_none());
        subscribers.frame_evt = Some(Box::new(fun));
    }

    pub fn disconnect_main_cam_proc_result_event(&self) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.frame_evt = None;
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
        self.indi.subscribe_events(move |event| {
            let result = || -> anyhow::Result<()> {
                match event {
                    indi_api::Event::BlobStart(event) => {
                        let mut mode_data = mode_data.write().unwrap();
                        mode_data.mode.notify_blob_start_event(&event)?;
                    }
                    indi_api::Event::PropChange(prop_change) => {
                        if let indi_api::PropChange::Change {
                            value: indi_api::PropChangeValue{
                                prop_value: indi_api::PropValue::Blob(blob), ..
                            }, ..
                        } = &prop_change.change {
                            Self::process_indi_blob_event(
                                &blob, &prop_change.device_name,
                                &prop_change.prop_name, &mode_data,
                                &options, &cur_frame, &ref_stars,
                                &calibr_images, &subscribers,
                                &img_cmds_sender,
                            )?;
                        } else {
                            Self::process_indi_prop_change_event(
                                &prop_change, &mode_data, &indi, &options,
                                &subscribers, &exp_stuck_wd
                            )?;
                        }
                    },
                    _ => {}
                }
                Ok(())
            } ();
            Self::process_error(result, &mode_data, &subscribers, &exp_stuck_wd);
        });
    }

    fn start_taking_frames_restart_timer(&self) {
        const MAX_EXP_STACK_WD_CNT: u16 = 30;
        let exp_stuck_wd = Arc::clone(&self.exp_stuck_wd);
        let mode_data = Arc::clone(&self.mode_data);
        let indi = Arc::clone(&self.indi);
        let subscribers = Arc::clone(&self.subscribers);
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
                let result = Self::restart_camera_exposure(&indi, &mode_data);
                Self::process_error(result, &mode_data, &subscribers, &exp_stuck_wd);
            }
        });
    }

    fn process_indi_prop_change_event(
        prop_change:  &indi_api::PropChangeEvent,
        mode_data:    &Arc<RwLock<ModeData>>,
        indi:         &Arc<indi_api::Connection>,
        options:      &Arc<RwLock<Options>>,
        subscribers:  &Arc<RwLock<Subscribers>>,
        exp_stuck_wd: &Arc<AtomicU16>,
    ) -> anyhow::Result<()> {
        let mut mode_data = mode_data.write().unwrap();
        let result = mode_data.mode.notify_indi_prop_change(&prop_change)?;
        Self::apply_change_result(
            result,
            &mut mode_data,
            &indi,
            &options,
            &subscribers
        )?;

        if let (indi_api::PropChange::Change { value, new_state, .. }, Some(cur_device))
        = (&prop_change.change, mode_data.mode.cam_device()) {
            let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&cur_device.prop);
            if indi_api::Connection::camera_is_exposure_property(&prop_change.prop_name, &value.elem_name, cam_ccd)
            && cur_device.name == prop_change.device_name
            && cur_device.prop == prop_change.prop_name {
                // exposure = 0.0 and state = busy means exposure has ended
                // but still no blob received
                if value.prop_value.as_f64().unwrap_or(0.0) == 0.0
                && *new_state == indi_api::PropState::Busy {
                    _ = exp_stuck_wd.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
                } else {
                    exp_stuck_wd.store(0, Ordering::Relaxed);
                }
            }
        }
        Ok(())
    }

    fn process_indi_blob_event(
        blob:              &Arc<indi_api::BlobPropValue>,
        device_name:       &str,
        device_prop:       &str,
        mode_data:         &Arc<RwLock<ModeData>>,
        options:           &Arc<RwLock<Options>>,
        cur_frame:         &Arc<ResultImage>,
        ref_stars:         &Arc<Mutex<Option<Vec<Point>>>>,
        calibr_images:     &Arc<Mutex<CalibrImages>>,
        subscribers:       &Arc<RwLock<Subscribers>>,
        frame_proc_sender: &mpsc::Sender<FrameProcessCommand>,
    ) -> anyhow::Result<()> {
        if blob.data.is_empty() { return Ok(()); }
        log::debug!(
            "process_blob_event, device_name = {}, device_prop = {}, dl_time = {:.2}s",
            device_name, device_prop, blob.dl_time
        );

        let mode_data = mode_data.read().unwrap();
        let Some(mode_cam) = mode_data.mode.cam_device() else {
            return Ok(());
        };

        if device_name != mode_cam.name
        || device_prop != mode_cam.prop {
            return Ok(());
        }

        let mut command_data = {
            let options = options.read().unwrap();
            let device = DeviceAndProp {
                name: device_name.to_string(),
                prop: device_prop.to_string(),
            };
            FrameProcessCommandData {
                mode_type:       mode_data.mode.get_type(),
                camera:          device,
                flags:           ProcessImageFlags::empty(),
                blob:            Arc::clone(blob),
                frame:           Arc::clone(&cur_frame),
                ref_stars:       Arc::clone(&ref_stars),
                calibr_params:   options.calibr.into_params(),
                calibr_images:   Arc::clone(&calibr_images),
                view_options:    options.preview.preview_params(),
                frame_options:   options.cam.frame.clone(),
                quality_options: Some(options.quality.clone()),
                fn_gen:          None,
                save_path:       None,
                raw_adder:       None,
                live_stacking:   None,
            }
        };

        mode_data.mode.complete_img_process_params(&mut command_data);

        let result_fun = {
            let subscribers = Arc::clone(subscribers);
            let options = Arc::clone(&options);
            move |res: FrameProcessResult| {
                let subscribers = subscribers.read().unwrap();
                let options = options.read().unwrap();
                if options.cam.device == res.camera {
                    if let Some(evt) = &subscribers.frame_evt {
                        evt(res);
                    }
                }
            }
        };
        frame_proc_sender.send(FrameProcessCommand::ProcessImage{
            command: command_data,
            result_fun: Box::new(result_fun),
        }).unwrap();

        Ok(())
    }

    fn restart_camera_exposure(
        indi:      &Arc<indi_api::Connection>,
        mode_data: &Arc<RwLock<ModeData>>,
    ) -> anyhow::Result<()> {
        let mode_data = mode_data.read().unwrap();
        let Some(cam_device) = mode_data.mode.cam_device() else { return Ok(()); };
        let Some(cur_exposure) = mode_data.mode.get_cur_exposure() else { return Ok(()); };
        let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&cam_device.prop);
        indi.camera_abort_exposure(&cam_device.name, cam_ccd)?;
        if indi.camera_is_fast_toggle_supported(&cam_device.name)?
        && indi.camera_is_fast_toggle_enabled(&cam_device.name)? {
            let prop_info = indi.camera_get_fast_frames_count_prop_info(
                &cam_device.name,
            ).unwrap();
            indi.camera_set_fast_frames_count(
                &cam_device.name,
                prop_info.max as usize,
                true,
                SET_PROP_TIMEOUT,
            )?;
        }
        indi.camera_start_exposure(&cam_device.name, cam_ccd, cur_exposure)?;
        log::error!("Camera exposure restarted!");
        Ok(())
    }

    pub fn subscribe_events(
        &self,
        fun: impl Fn(StateEvent) + Send + Sync + 'static
    ) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.add(fun);
    }

    pub fn start_single_shot(&self) -> anyhow::Result<()> {
        let mut mode = TackingFramesMode::new(
            &self.indi,
            None,
            CamMode::SingleShot,
            &self.options
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
        let mut mode = TackingFramesMode::new(
            &self.indi,
            Some(&self.timer),
            CamMode::LiveView,
            &self.options
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
        let mut mode = TackingFramesMode::new(
            &self.indi,
            Some(&self.timer),
            CamMode::SavingRawFrames,
            &self.options
        );
        mode.ref_stars = Some(Arc::clone(&self.ref_stars));
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
        let mut mode = TackingFramesMode::new(
            &self.indi,
            Some(&self.timer),
            CamMode::LiveStacking,
            &self.options
        );
        mode.ref_stars = Some(Arc::clone(&self.ref_stars));
        mode.live_stacking = Some(Arc::clone(&self.live_stacking));
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
        let mut mode = FocusingMode::new(&self.indi, &self.options, None);
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

    pub fn notify_about_frame_processing_started(&self) {
        let mut mode_data = self.mode_data.write().unwrap();
        let result = mode_data.mode.notify_about_frame_processing_started();
        drop(mode_data);
        Self::process_error(result, &self.mode_data, &self.subscribers, &self.exp_stuck_wd);
    }

    pub fn notify_about_light_frame_info(
        &self,
        info: &LightFrameInfo
    ) {
        let result = || -> anyhow::Result<()> {
            let mut mode_data = self.mode_data.write().unwrap();
            let res = mode_data.mode.notify_about_light_frame_info(info, &self.subscribers)?;
            Self::apply_change_result(res, &mut mode_data, &self.indi, &self.options, &self.subscribers)?;
            Ok(())
        } ();
        Self::process_error(result, &self.mode_data, &self.subscribers, &self.exp_stuck_wd);
    }

    pub fn notify_about_frame_processing_finished(
        &self,
        frame_is_ok: bool,
    ) {
        let result = || -> anyhow::Result<()> {
            let mut mode_data = self.mode_data.write().unwrap();
            let result = mode_data.mode.notify_about_frame_processing_finished(frame_is_ok)?;
            Self::apply_change_result(result, &mut mode_data, &self.indi, &self.options, &self.subscribers)?;
            Ok(())
        } ();
        Self::process_error(result, &self.mode_data, &self.subscribers, &self.exp_stuck_wd);
    }

    fn apply_change_result(
        result:      NotifyResult,
        mode_data:   &mut ModeData,
        indi:        &Arc<indi_api::Connection>,
        options:     &Arc<RwLock<Options>>,
        subscribers: &Arc<RwLock<Subscribers>>,
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
                let mut mode = FocusingMode::new(indi, options, Some(prev_mode));
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

impl Drop for State {
    fn drop(&mut self) {
        if let Some(process_thread) = self.process_thread.take() {
            _ = process_thread.join();
        }
        log::info!("State dropped");
    }
}

///////////////////////////////////////////////////////////////////////////////

fn start_taking_shots(
    indi:         &indi_api::Connection,
    frame:        &FrameOptions,
    device:       &DeviceAndProp,
    continuously: bool,
) -> anyhow::Result<()> {
    indi.command_enable_blob(
        &device.name,
        None,
        indi_api::BlobEnable::Also
    )?;
    if indi.camera_is_fast_toggle_supported(&device.name,)? {
        let use_fast_toggle =
            continuously && !frame.have_to_use_delay();
        indi.camera_enable_fast_toggle(
            &device.name,
            use_fast_toggle,
            true,
            SET_PROP_TIMEOUT,
        )?;
        if use_fast_toggle {
            let prop_info = indi.camera_get_fast_frames_count_prop_info(
                &device.name,
            )?;
            indi.camera_set_fast_frames_count(
                &device.name,
                prop_info.max as usize,
                true,
                SET_PROP_TIMEOUT,
            )?;
        }
    }
    apply_camera_options_and_take_shot(
        indi,
        device,
        frame
    )?;
    Ok(())
}

fn apply_camera_options_and_take_shot(
    indi:        &indi_api::Connection,
    device:      &DeviceAndProp,
    frame:       &FrameOptions,
) -> anyhow::Result<()> {
    let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&device.prop);

    // Polling period
    if indi.device_is_polling_period_supported(&device.name)? {
        indi.device_set_polling_period(&device.name, 500, true, None)?;
    }

    // Frame type
    indi.camera_set_frame_type(
        &device.name,
        cam_ccd,
        frame.frame_type.to_indi_frame_type(),
        true,
        SET_PROP_TIMEOUT
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
            SET_PROP_TIMEOUT
        )?;
    }

    // Make binning mode is alwais AVG (if camera supports it)
    if indi.camera_is_binning_mode_supported(&device.name, cam_ccd)?
    && frame.binning != Binning::Orig {
        indi.camera_set_binning_mode(
            &device.name,
            indi_api::BinningMode::Avg,
            true,
            SET_PROP_TIMEOUT
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
            SET_PROP_TIMEOUT
        )?;
    }

    // Gain
    if indi.camera_is_gain_supported(&device.name)? {
        indi.camera_set_gain(
            &device.name,
            frame.gain,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Offset
    if indi.camera_is_offset_supported(&device.name)? {
        indi.camera_set_offset(
            &device.name,
            frame.offset as f64,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Low noise mode
    if indi.camera_is_low_noise_ctrl_supported(&device.name)? {
        indi.camera_control_low_noise(
            &device.name,
            frame.low_noise,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Capture format = RAW
    if indi.camera_is_capture_format_supported(&device.name)? {
        indi.camera_set_capture_format(
            &device.name,
            indi_api::CaptureFormat::Raw,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Start exposure
    indi.camera_start_exposure(&device.name, cam_ccd, frame.exposure())?;

    Ok(())
}


///////////////////////////////////////////////////////////////////////////////

pub struct Timer {
    thread:    Option<std::thread::JoinHandle<()>>,
    commands:  Arc<Mutex<Vec<TimerCommand>>>,
    exit_flag: Arc<AtomicBool>,
}

struct TimerCommand {
    fun: Option<Box<dyn Fn() + Sync + Send + 'static>>,
    time: std::time::Instant,
    to_ms: u32,
    periodic: bool,
}

impl Drop for Timer {
    fn drop(&mut self) {
        log::info!("Stopping ThreadTimer thread...");
        self.exit_flag.store(true, Ordering::Relaxed);
        let thread = self.thread.take().unwrap();
        _ = thread.join();
        log::info!("Done!");
    }
}

impl Timer {
    pub fn new() -> Self {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let exit_flag = Arc::new(AtomicBool::new(false));

        let thread = {
            let commands = Arc::clone(&commands);
            let exit_flag = Arc::clone(&exit_flag);
            std::thread::spawn(move || {
                Self::thread_fun(&commands, &exit_flag);
            })
        };
        Self {
            thread: Some(thread),
            commands,
            exit_flag,
        }
    }

    pub fn exec(&self, to_ms: u32, periodic: bool, fun: impl Fn() + Sync + Send + 'static) {
        let mut commands = self.commands.lock().unwrap();
        let command = TimerCommand {
            fun: Some(Box::new(fun)),
            time: std::time::Instant::now(),
            to_ms,
            periodic,
        };
        commands.push(command);
    }

    fn thread_fun(
        commands:  &Mutex<Vec<TimerCommand>>,
        exit_flag: &AtomicBool
    ) {
        while !exit_flag.load(Ordering::Relaxed) {
            let mut commands = commands.lock().unwrap();
            for cmd in &mut *commands {
                if cmd.time.elapsed().as_millis() as u32 >= cmd.to_ms {
                    if let Some(fun) = &mut cmd.fun {
                        fun();
                    }
                    if cmd.periodic {
                        cmd.time = std::time::Instant::now();
                    } else {
                        cmd.fun = None;
                    }
                }
            }
            commands.retain(|cmd| cmd.fun.is_some());
            drop(commands);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

///////////////////////////////////////////////////////////////////////////////

struct WaitingMode;

impl Mode for WaitingMode {
    fn get_type(&self) -> ModeType {
        ModeType::Waiting
    }

    fn progress_string(&self) -> String {
        "Waiting...".to_string()
    }

    fn can_be_stopped(&self) -> bool {
        false
    }
}

///////////////////////////////////////////////////////////////////////////////


const MAX_TIMED_GUIDE: f64 = 20.0; // in seconds


// Guider for guiding by main camera
struct SimpleGuider {
    mnt_calibr:        Option<MountMoveCalibrRes>,
    dither_x:          f64,
    dither_y:          f64,
    cur_timed_guide_n: f64,
    cur_timed_guide_s: f64,
    cur_timed_guide_w: f64,
    cur_timed_guide_e: f64,
    dither_exp_sum:    f64,
}

impl SimpleGuider {
    fn new() -> Self {
        Self {
            mnt_calibr: None,
            dither_x: 0.0,
            dither_y: 0.0,
            cur_timed_guide_n: 0.0,
            cur_timed_guide_s: 0.0,
            cur_timed_guide_w: 0.0,
            cur_timed_guide_e: 0.0,
            dither_exp_sum:    0.0,
        }
    }
}

#[derive(PartialEq)]
enum CamMode {
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
}

#[derive(PartialEq)]
enum CamState {
    Usual,
    MountCorrection
}

struct TackingFramesMode {
    cam_mode:      CamMode,
    state:         CamState,
    device:        DeviceAndProp,
    mount_device:  String,
    fn_gen:        Arc<Mutex<SeqFileNameGen>>,
    indi:          Arc<indi_api::Connection>,
    timer:         Option<Arc<Timer>>,
    raw_adder:     Arc<Mutex<RawAdder>>,
    options:       Arc<RwLock<Options>>,
    frame_options: FrameOptions,
    focus_options: Option<FocuserOptions>,
    guid_options:  Option<SimpleGuidingOptions>,
    ref_stars:     Option<Arc<Mutex<Option<Vec<Point>>>>>,
    progress:      Option<Progress>,
    cur_exposure:  f64,
    exp_sum:       f64,
    simple_guider: Option<SimpleGuider>,
    live_stacking: Option<Arc<LiveStackingData>>,
    save_dir:      PathBuf,
    master_file:   PathBuf,
}

impl TackingFramesMode {
    fn new(
        indi:     &Arc<indi_api::Connection>,
        timer:    Option<&Arc<Timer>>,
        cam_mode: CamMode,
        options:  &Arc<RwLock<Options>>
    ) -> Self {
        let opts = options.read().unwrap();
        let progress = match cam_mode {
            CamMode::SavingRawFrames => {
                if opts.raw_frames.use_cnt && opts.raw_frames.frame_cnt != 0 {
                    Some(Progress { cur: 0, total: opts.raw_frames.frame_cnt })
                } else {
                    None
                }
            },
            _ => None,
        };
        Self {
            cam_mode,
            state:         CamState::Usual,
            device:        opts.cam.device.clone(),
            mount_device:  opts.mount.device.to_string(),
            fn_gen:        Arc::new(Mutex::new(SeqFileNameGen::new())),
            indi:          Arc::clone(indi),
            timer:         timer.cloned(),
            raw_adder:     Arc::new(Mutex::new(RawAdder::new())),
            options:       Arc::clone(options),
            frame_options: opts.cam.frame.clone(),
            focus_options: None,
            guid_options:  None,
            ref_stars:     None,
            progress,
            cur_exposure:  0.0,
            exp_sum:       0.0,
            simple_guider:     None,
            live_stacking: None,
            save_dir:      PathBuf::new(),
            master_file:   PathBuf::new(),
        }
    }

    fn update_options_copies(&mut self) {
        let opts = self.options.read().unwrap();
        let work_mode =
            self.cam_mode == CamMode::SavingRawFrames ||
            self.cam_mode == CamMode::LiveStacking;
        self.focus_options = if opts.focuser.is_used() && work_mode {
            Some(opts.focuser.clone())
        } else {
            None
        };
        self.guid_options = if opts.simp_guide.is_used() && work_mode {
            Some(opts.simp_guide.clone())
        } else {
            None
        };
    }

    fn correct_options_before_start(&self) {
        if self.cam_mode == CamMode::LiveStacking {
            let mut options = self.options.write().unwrap();
            options.cam.frame.frame_type = FrameType::Lights;
        }
    }

    fn start_or_continue(&mut self) -> anyhow::Result<()> {
        let continuously = match (&self.cam_mode, &self.frame_options.frame_type) {
            (CamMode::SingleShot,      _                ) => false,
            (CamMode::LiveView,        _                ) => false,
            (CamMode::SavingRawFrames, FrameType::Flats ) => false,
            (CamMode::SavingRawFrames, FrameType::Biases) => false,
            (CamMode::SavingRawFrames, _                ) => true,
            (CamMode::LiveStacking,    _                ) => true,
        };
        start_taking_shots(
            &self.indi,
            &self.frame_options,
            &self.device,
            continuously
        )?;
        self.state = CamState::Usual;
        self.cur_exposure = self.frame_options.exposure();
        Ok(())
    }

    fn create_file_names_for_raw_saving(&mut self) {
        let now_date_str = Utc::now().format("%Y-%m-%d").to_string();
        let options = self.options.read().unwrap();
        let bin = options.cam.frame.binning.get_ratio();
        let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&self.device.prop);
        let (width, height) =
            self.indi
                .camera_get_max_frame_size(&self.device.name, cam_ccd)
                .unwrap_or((0, 0));
        let cropped_width = options.cam.frame.crop.translate(width/bin);
        let cropped_height = options.cam.frame.crop.translate(height/bin);
        let exp_to_str = |exp: f64| {
            if exp > 1.0 {
                format!("{:.0}", exp)
            } else if exp >= 0.1 {
                format!("{:.1}", exp)
            } else {
                format!("{:.3}", exp)
            }
        };
        let mut common_part = format!(
            "{}s_g{}_offs{}_{}x{}",
            exp_to_str(options.cam.frame.exposure()),
            options.cam.frame.gain,
            options.cam.frame.offset,
            cropped_width,
            cropped_height,
        );
        if bin != 1 {
            common_part.push_str(&format!("_bin{}x{}", bin, bin));
        }
        let type_part = match options.cam.frame.frame_type {
            FrameType::Undef => unreachable!(),
            FrameType::Lights => "light",
            FrameType::Flats => "flat",
            FrameType::Darks => "dark",
            FrameType::Biases => "bias",
        };
        let cam_cooler_supported = self.indi
            .camera_is_cooler_supported(&self.device.name)
            .unwrap_or(false);
        let temp_path = if cam_cooler_supported && options.cam.ctrl.enable_cooler {
            Some(format!("{:+.0}C", options.cam.ctrl.temperature))
        } else {
            None
        };
        if options.cam.frame.frame_type != FrameType::Lights {
            let mut master_file = String::new();
            master_file.push_str(type_part);
            master_file.push_str("_");
            master_file.push_str(&common_part);
            if options.cam.frame.frame_type != FrameType::Flats {
                if let Some(temp) = &temp_path {
                    master_file.push_str("_");
                    master_file.push_str(&temp);
                }
            }
            if options.cam.frame.frame_type == FrameType::Flats {
                master_file.push_str("_");
                master_file.push_str(&now_date_str);
            }
            master_file.push_str(".fit");

            let mut path = options.raw_frames.out_path.clone();
            path.push(&master_file);
            self.master_file = path;
        }
        let mut save_dir = String::new();
        save_dir.push_str(type_part);
        save_dir.push_str("_");
        save_dir.push_str(&now_date_str);
        save_dir.push_str("__");
        save_dir.push_str(&common_part);
        if options.cam.frame.frame_type != FrameType::Flats {
            if let Some(temp) = &temp_path {
                save_dir.push_str("_");
                save_dir.push_str(&temp);
            }
        }
        let mut path = options.raw_frames.out_path.clone();
        path.push(&save_dir);
        self.save_dir = get_free_folder_name(&path);
    }
}

impl Mode for TackingFramesMode {
    fn get_type(&self) -> ModeType {
        match self.cam_mode {
            CamMode::SingleShot => ModeType::SingleShot,
            CamMode::LiveView => ModeType::LiveView,
            CamMode::SavingRawFrames => ModeType::SavingRawFrames,
            CamMode::LiveStacking => ModeType::LiveStacking,
        }
    }

    fn progress_string(&self) -> String {
        let mut mode_str = match (&self.state, &self.cam_mode) {
            (CamState::MountCorrection, _) =>
                "Mount position correction".to_string(),
            (_, CamMode::SingleShot) =>
                "Taking shot".to_string(),
            (_, CamMode::LiveView) =>
                "Live view from camera".to_string(),
            (_, CamMode::SavingRawFrames) =>
                self.frame_options.frame_type.to_readable_str().to_string(),
            (_, CamMode::LiveStacking) =>
                "Live stacking".to_string(),
        };
        let mut extra_modes = Vec::new();
        if matches!(self.cam_mode, CamMode::SavingRawFrames|CamMode::LiveStacking)
        && self.frame_options.frame_type == FrameType::Lights
        && self.state == CamState::Usual {
            if let Some(focus_options) = &self.focus_options {
                if focus_options.on_fwhm_change
                || focus_options.on_temp_change
                || focus_options.periodically {
                    extra_modes.push("F");
                }
            }
            if let Some(guid_options) = &self.guid_options {
                if guid_options.enabled {
                    extra_modes.push("G");
                }
                if guid_options.dith_period != 0 {
                    extra_modes.push("D");
                }
            }
        }
        if !extra_modes.is_empty() {
            mode_str += " ";
            mode_str += &extra_modes.join(" + ");
        }
        mode_str
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.device)
    }

    fn progress(&self) -> Option<Progress> {
        self.progress.clone()
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cur_exposure)
    }

    fn can_be_stopped(&self) -> bool {
        matches!(
            &self.cam_mode,
            CamMode::SingleShot |
            CamMode::SavingRawFrames|
            CamMode::LiveStacking
        )
    }

    fn can_be_continued_after_stop(&self) -> bool {
        matches!(
            &self.cam_mode,
            CamMode::SavingRawFrames|
            CamMode::LiveStacking
        )
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.correct_options_before_start();
        self.update_options_copies();
        if let Some(ref_stars) = &mut self.ref_stars {
            let mut ref_stars = ref_stars.lock().unwrap();
            *ref_stars = None;
        }
        if let Some(live_stacking) = &mut self.live_stacking {
            let mut adder = live_stacking.adder.write().unwrap();
            adder.clear();
        }

        if let CamMode::SavingRawFrames|CamMode::LiveStacking = self.cam_mode {
            self.create_file_names_for_raw_saving();
        }

        self.start_or_continue()?;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&self.device.prop);
        self.indi.camera_abort_exposure(&self.device.name, cam_ccd)?;
        Ok(())
    }

    fn continue_work(&mut self) -> anyhow::Result<()> {
        self.correct_options_before_start();
        self.update_options_copies();
        self.state = CamState::Usual;

        // Restore original frame options
        // in saving raw or live stacking mode
        if self.cam_mode == CamMode::SavingRawFrames
        || self.cam_mode == CamMode::LiveStacking {
            let mut options = self.options.write().unwrap();
            options.cam.frame = self.frame_options.clone();
        }
        self.start_or_continue()?;
        Ok(())
    }

    fn set_or_correct_value(&mut self, value: &mut dyn Any) {
        if let Some(value) = value.downcast_mut::<MountMoveCalibrRes>() {
            let dith_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
            dith_data.mnt_calibr = Some(value.clone());
            log::debug!("New mount calibration set: {:?}", dith_data.mnt_calibr);
        }
    }

    fn notify_blob_start_event(
        &mut self,
        event: &indi_api::BlobStartEvent
    ) -> anyhow::Result<()> {
        if event.device_name != self.device.name
        || event.prop_name != self.device.prop {
            return Ok(());
        }
        match (&self.cam_mode, &self.frame_options.frame_type) {
            (CamMode::SingleShot,      _                ) => return Ok(()),
            (CamMode::SavingRawFrames, FrameType::Flats ) => return Ok(()),
            (CamMode::SavingRawFrames, FrameType::Biases) => return Ok(()),
            _ => {},
        }
        if self.cam_mode == CamMode::LiveView {
            // We need fresh frame options in live view mode
            let options = self.options.read().unwrap();
            self.frame_options = options.cam.frame.clone();
        }
        let fast_mode_enabled =
            self.indi.camera_is_fast_toggle_supported(&self.device.name).unwrap_or(false) &&
            self.indi.camera_is_fast_toggle_enabled(&self.device.name).unwrap_or(false);
        if !fast_mode_enabled {
            self.cur_exposure = self.frame_options.exposure();
            if !self.frame_options.have_to_use_delay() {
                apply_camera_options_and_take_shot(
                    &self.indi,
                    &self.device,
                    &self.frame_options
                )?;
            } else {
                let indi = Arc::clone(&self.indi);
                let camera = self.device.clone();
                let frame = self.frame_options.clone();

                if let Some(thread_timer) = &self.timer {
                    thread_timer.exec((frame.delay * 1000.0) as u32, false, move || {
                        let res = apply_camera_options_and_take_shot(&indi, &camera, &frame);
                        if let Err(err) = res {
                            log::error!("{} during trying start next shot", err.to_string());
                            // TODO: show error!!!
                        }
                    });
                }
            }
        }
        Ok(())
    }

    fn notify_about_frame_processing_started(&mut self) -> anyhow::Result<()> {
        if let Some(progress) = &mut self.progress {
            if progress.cur+1 == progress.total &&
            self.indi.camera_is_fast_toggle_enabled(&self.device.name)? {
                self.abort()?;
            }
        }
        Ok(())
    }

    fn notify_about_light_frame_info(
        &mut self,
        info:         &LightFrameInfo,
        _subscribers: &Arc<RwLock<Subscribers>>
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;

        if !info.is_ok() { return Ok(result); }
        let mount_device_active = self.indi.is_device_enabled(&self.mount_device).unwrap_or(false);

        if let Some(guid_options) = &self.guid_options { // Guiding and dithering
            let guid_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
            if (guid_options.enabled || guid_options.dith_period != 0)
            && mount_device_active {
                if guid_data.mnt_calibr.is_none() { // mount moving calibration
                    return Ok(NotifyResult::StartMountCalibr);
                }
            }
        }

        // Refocus
        let use_focus =
            self.cam_mode == CamMode::LiveStacking ||
            self.cam_mode == CamMode::SavingRawFrames;
        if let (Some(focuser_options), true) = (&self.focus_options, use_focus) {
            let mut have_to_refocus = false;
            if self.indi.is_device_enabled(&focuser_options.device).unwrap_or(false) {
                if focuser_options.periodically && focuser_options.period_minutes != 0 {
                    self.exp_sum += self.frame_options.exposure();
                    let max_exp_sum = (focuser_options.period_minutes * 60) as f64;
                    if self.exp_sum >= max_exp_sum {
                        have_to_refocus = true;
                        self.exp_sum = 0.0;
                    }
                }
            }
            if have_to_refocus {
                return Ok(NotifyResult::StartFocusing);
            }
        }

        let mount_device_active = self.indi.is_device_enabled(&self.mount_device).unwrap_or(false);
        if self.state == CamState::Usual && mount_device_active {
            let mut move_offset = None;
            if let Some(guid_options) = &self.guid_options {
                let guid_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
                let mut prev_dither_x = 0_f64;
                let mut prev_dither_y = 0_f64;
                let mut dithering_flag = false;
                if guid_options.dith_period != 0 { // dithering
                    guid_data.dither_exp_sum += info.exposure;
                    if guid_data.dither_exp_sum > (guid_options.dith_period * 60) as f64 {
                        guid_data.dither_exp_sum = 0.0;
                        let min_size = ((info.width + info.height) / 2) as f64;
                        let dither_max_size = min_size as f64 * guid_options.dith_percent / 100.0;
                        use rand::prelude::*;
                        let mut rng = rand::thread_rng();
                        prev_dither_x = guid_data.dither_x;
                        prev_dither_y = guid_data.dither_y;
                        guid_data.dither_x = dither_max_size * (rng.gen::<f64>() - 0.5);
                        guid_data.dither_y = dither_max_size * (rng.gen::<f64>() - 0.5);
                        log::debug!("dithering position = {}px,{}px", guid_data.dither_x, guid_data.dither_y);
                        dithering_flag = true;
                    }
                }
                if let (Some(offset), true) = (&info.stars_offset, guid_options.enabled) { // guiding
                    let mut offset_x = offset.x;
                    let mut offset_y = offset.y;
                    offset_x -= guid_data.dither_x;
                    offset_y -= guid_data.dither_y;
                    let diff_dist = f64::sqrt(offset_x * offset_x + offset_y * offset_y);
                    log::debug!("diff_dist = {}px", diff_dist);
                    if diff_dist > guid_options.max_error || dithering_flag {
                        move_offset = Some((-offset_x, -offset_y));
                        log::debug!(
                            "diff_dist > guid_options.max_error ({} > {}), start mount correction",
                            diff_dist,
                            guid_options.max_error
                        );
                    }
                } else if dithering_flag {
                    move_offset = Some((
                        guid_data.dither_x-prev_dither_x,
                        guid_data.dither_y-prev_dither_y
                    ));
                }
            }
            if let Some((offset_x, offset_y)) = move_offset { // Move mount position
                let guid_data = self.simple_guider.get_or_insert_with(|| SimpleGuider::new());
                let mnt_calibr = guid_data.mnt_calibr.clone().unwrap_or_default();
                if mnt_calibr.is_ok() {
                    if let Some((mut ra, mut dec)) = mnt_calibr.calc(offset_x, offset_y) {
                        guid_data.cur_timed_guide_n = 0.0;
                        guid_data.cur_timed_guide_s = 0.0;
                        guid_data.cur_timed_guide_w = 0.0;
                        guid_data.cur_timed_guide_e = 0.0;
                        self.abort()?;
                        let can_set_guide_rate =
                            self.indi.mount_is_guide_rate_supported(&self.mount_device)? &&
                            self.indi.mount_get_guide_rate_prop_data(&self.mount_device)?.perm == indi_api::PropPerm::RW;
                        if can_set_guide_rate {
                            self.indi.mount_set_guide_rate(
                                &self.mount_device,
                                DITHER_CALIBR_SPEED,
                                DITHER_CALIBR_SPEED,
                                true,
                                SET_PROP_TIMEOUT
                            )?;
                        }
                        let (max_dec, max_ra) = self.indi.mount_get_timed_guide_max(&self.mount_device)?;
                        let max_dec = f64::min(MAX_TIMED_GUIDE * 1000.0, max_dec);
                        let max_ra = f64::min(MAX_TIMED_GUIDE * 1000.0, max_ra);
                        ra *= 1000.0;
                        dec *= 1000.0;
                        if ra > max_ra { ra = max_ra; }
                        if ra < -max_ra { ra = -max_ra; }
                        if dec > max_dec { dec = max_dec; }
                        if dec < -max_dec { dec = -max_dec; }
                        log::debug!("Timed guide, NS = {:.2}s, WE = {:.2}s", dec, ra);
                        self.indi.mount_timed_guide(&self.mount_device, dec, ra)?;
                        self.state = CamState::MountCorrection;
                        result = NotifyResult::ModeChanged;
                    }
                }
            }
        }

        Ok(result)
    }

    fn notify_about_frame_processing_finished(
        &mut self,
        frame_is_ok: bool
    ) -> anyhow::Result<NotifyResult> {
        if self.cam_mode == CamMode::SingleShot {
            return Ok(NotifyResult::Finished { next_mode: None });
        }
        let mut result = NotifyResult::Empty;
        if let Some(progress) = &mut self.progress {
            if frame_is_ok && progress.cur != progress.total {
                progress.cur += 1;
                result = NotifyResult::ProgressChanges;
            }
            if progress.cur == progress.total {
                let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&self.device.prop);
                self.indi.camera_abort_exposure(&self.device.name, cam_ccd)?;
                result = NotifyResult::Finished { next_mode: None };
            } else {
                let have_shart_new_shot = match (&self.cam_mode, &self.frame_options.frame_type) {
                    (CamMode::SavingRawFrames, FrameType::Biases) => true,
                    (CamMode::SavingRawFrames, FrameType::Flats) => true,
                    _ => false
                };
                if have_shart_new_shot {
                    apply_camera_options_and_take_shot(
                        &self.indi,
                        &self.device,
                        &self.frame_options
                    )?;
                }
            }
        }
        Ok(result)
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        let options = self.options.read().unwrap();
        cmd.fn_gen = Some(Arc::clone(&self.fn_gen));
        let last_in_seq = if let Some(progress) = &self.progress {
            progress.cur + 1 == progress.total
        } else {
            false
        };
        match self.cam_mode {
            CamMode::SavingRawFrames => {
                cmd.save_path = Some(self.save_dir.clone());
                if options.raw_frames.create_master {
                    cmd.raw_adder = Some(RawAdderParams {
                        adder: Arc::clone(&self.raw_adder),
                        save_fn: if last_in_seq { Some(get_free_file_name(&self.master_file)) } else { None },
                    });
                }
                if options.cam.frame.frame_type == FrameType::Lights
                && !options.mount.device.is_empty() && options.simp_guide.enabled {
                    cmd.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
                }
                cmd.flags |= ProcessImageFlags::SAVE_RAW;
            },
            CamMode::LiveStacking => {
                cmd.save_path = Some(self.save_dir.clone());
                cmd.live_stacking = Some(LiveStackingParams {
                    data:    Arc::clone(self.live_stacking.as_ref().unwrap()),
                    options: options.live.clone(),
                });
                cmd.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
                if options.live.save_orig {
                    cmd.flags |= ProcessImageFlags::SAVE_RAW;
                }
            },
            _ => {},
        }
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi_api::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if self.state == CamState::MountCorrection {
            if let ("TELESCOPE_TIMED_GUIDE_NS"|"TELESCOPE_TIMED_GUIDE_WE", indi_api::PropChange::Change { value, .. }, Some(guid_data))
            = (prop_change.prop_name.as_str(), &prop_change.change, &mut self.simple_guider) {
                match value.elem_name.as_str() {
                    "TIMED_GUIDE_N" => guid_data.cur_timed_guide_n = value.prop_value.as_f64()?,
                    "TIMED_GUIDE_S" => guid_data.cur_timed_guide_s = value.prop_value.as_f64()?,
                    "TIMED_GUIDE_W" => guid_data.cur_timed_guide_w = value.prop_value.as_f64()?,
                    "TIMED_GUIDE_E" => guid_data.cur_timed_guide_e = value.prop_value.as_f64()?,
                    _ => {},
                }
                if guid_data.cur_timed_guide_n == 0.0
                && guid_data.cur_timed_guide_s == 0.0
                && guid_data.cur_timed_guide_w == 0.0
                && guid_data.cur_timed_guide_e == 0.0 {
                    start_taking_shots(
                        &self.indi,
                        &self.frame_options,
                        &self.device,
                        true
                    )?;
                    self.state = CamState::Usual;
                    result = NotifyResult::ModeChanged;
                }
            }
        }
        Ok(result)
    }
}


///////////////////////////////////////////////////////////////////////////////

const MAX_FOCUS_TOTAL_TRY_CNT: usize = 8;
const MAX_FOCUS_SAMPLE_TRY_CNT: usize = 4;
const MAX_FOCUS_STAR_OVALITY: f32 = 2.0;

#[derive(PartialEq)]
enum FocusingStage {
    Undef,
    Preliminary,
    Final
}

struct FocusingMode {
    indi:       Arc<indi_api::Connection>,
    state:      FocusingState,
    camera:     DeviceAndProp,
    options:    FocuserOptions,
    frame:      FrameOptions,
    before_pos: f64,
    to_go:      VecDeque<f64>,
    samples:    Vec<FocuserSample>,
    result_pos: Option<f64>,
    try_cnt:    usize,
    stage:      FocusingStage,
    next_mode:  Option<Box<dyn Mode + Sync + Send>>,
}

#[derive(PartialEq)]
enum FocusingState {
    Undefined,
    WaitingPositionAntiBacklash{
        before_pos: f64,
        begin_pos: f64
    },
    WaitingPosition(f64),
    WaitingFrame(f64),
    WaitingResultPosAntiBacklash{
        before_pos: f64,
        begin_pos: f64
    },
    WaitingResultPos(f64),
    WaitingResultImg,
}

#[derive(Clone)]
pub struct FocuserSample {
    pub focus_pos:     f64,
    pub stars_fwhm:    f32,
    pub stars_ovality: f32,
}

impl FocusingMode {
    fn new(
        indi:      &Arc<indi_api::Connection>,
        options:   &Arc<RwLock<Options>>,
        next_mode: Option<Box<dyn Mode + Sync + Send>>,
    ) -> Self {
        let options = options.read().unwrap();
        let mut frame = options.cam.frame.clone();
        frame.exp_main = options.focuser.exposure;
        FocusingMode {
            indi:       Arc::clone(indi),
            state:      FocusingState::Undefined,
            options:    options.focuser.clone(),
            frame,
            before_pos: 0.0,
            to_go:      VecDeque::new(),
            samples:    Vec::new(),
            result_pos: None,
            stage:      FocusingStage::Undef,
            try_cnt:    0,
            next_mode,
            camera:     options.cam.device.clone(),
        }
    }

    fn start_stage(
        &mut self,
        middle_pos: f64,
        stage:      FocusingStage
    ) -> anyhow::Result<()> {
        self.samples.clear();
        self.to_go.clear();
        for step in 0..self.options.measures {
            let step = step as f64;
            let half_progress = (self.options.measures as f64 - 1.0) / 2.0;
            let pos_to_go = middle_pos + self.options.step * (step - half_progress);
            self.to_go.push_back(pos_to_go);
        }
        self.stage = stage;
        self.start_sample(true)?;
        Ok(())
    }

    fn start_sample(
        &mut self,
        first_time: bool
    ) -> anyhow::Result<()> {
        let Some(pos) = self.to_go.pop_front() else {
            return Ok(());
        };
        if !first_time {
            self.indi.focuser_set_abs_value(&self.options.device, pos, true, None)?;
            self.state = FocusingState::WaitingPosition(pos);
        } else {
            let mut before_pos = pos - self.options.step;
            let cur_pos = self.indi.focuser_get_abs_value(&self.options.device)?;
            if f64::abs(before_pos - cur_pos) < 1.0 {
                before_pos -= 1.0;
            }
            self.indi.focuser_set_abs_value(&self.options.device, before_pos, true, None)?;
            self.state = FocusingState::WaitingPositionAntiBacklash{
                before_pos,
                begin_pos: pos
            };
        }
        Ok(())
    }
}

impl Mode for FocusingMode {
    fn get_type(&self) -> ModeType {
        ModeType::Focusing
    }

    fn progress_string(&self) -> String {
        match self.stage {
            FocusingStage::Preliminary =>
                "Focusing (preliminary)".to_string(),
            FocusingStage::Final =>
                "Focusing (final)".to_string(),
            _ => unreachable!(),
        }
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.samples.len(),
            total: self.samples.len() + self.to_go.len() + 1
        })
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.frame.exposure())
    }

    fn can_be_continued_after_stop(&self) -> bool {
        false
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let cur_pos = self.indi.focuser_get_abs_value(&self.options.device)?.round();
        self.before_pos = cur_pos;
        self.start_stage(cur_pos, FocusingStage::Preliminary)?;
        Ok(())
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&self.camera.prop);
        self.indi.camera_abort_exposure(&self.camera.name, cam_ccd)?;
        self.indi.focuser_set_abs_value(&self.options.device, self.before_pos, true, None)?;
        Ok(())
    }

    fn take_next_mode(&mut self) -> Option<ModeBox> {
        self.next_mode.take()
    }

    fn complete_img_process_params(&self, cmd: &mut FrameProcessCommandData) {
        if let Some(quality_options) = &mut cmd.quality_options {
            quality_options.use_max_fwhm = false;
        }
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi_api::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        if prop_change.device_name != self.options.device {
            return Ok(NotifyResult::Empty);
        }
        if let ("ABS_FOCUS_POSITION", indi_api::PropChange::Change { value, .. })
        = (prop_change.prop_name.as_str(), &prop_change.change) {
            let cur_focus = value.prop_value.as_f64()?;
            match self.state {
                FocusingState::WaitingPositionAntiBacklash {before_pos, begin_pos} => {
                    if f64::abs(cur_focus-before_pos) < 1.01 {
                        self.indi.focuser_set_abs_value(&self.options.device, begin_pos, true, None)?;
                        self.state = FocusingState::WaitingPosition(begin_pos);
                    }
                }
                FocusingState::WaitingPosition(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        start_taking_shots(&self.indi, &self.frame, &self.camera, false)?;
                        self.state = FocusingState::WaitingFrame(desired_focus);
                    }
                }
                FocusingState::WaitingResultPosAntiBacklash { before_pos, begin_pos } => {
                    if f64::abs(cur_focus-before_pos) < 1.01 {
                        self.indi.focuser_set_abs_value(&self.options.device, begin_pos, true, None)?;
                        self.state = FocusingState::WaitingResultPos(begin_pos);
                    }
                }
                FocusingState::WaitingResultPos(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        start_taking_shots(&self.indi, &self.frame, &self.camera, false)?;
                        self.state = FocusingState::WaitingResultImg;
                    }
                }
                _ => {}
            }
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_light_frame_info(
        &mut self,
        info:        &LightFrameInfo,
        subscribers: &Arc<RwLock<Subscribers>>,
    ) -> anyhow::Result<NotifyResult> {
        let subscribers = subscribers.read().unwrap();
        let mut result = NotifyResult::Empty;
        if let FocusingState::WaitingFrame(focus_pos) = self.state {
            let mut ok = false;
            if let (Some(stars_ovality), Some(stars_fwhm)) = (info.stars_ovality, info.stars_fwhm) {
                self.try_cnt = 0;
                if stars_ovality < MAX_FOCUS_STAR_OVALITY {
                    let sample = FocuserSample {
                        focus_pos,
                        stars_fwhm,
                        stars_ovality
                    };
                    self.samples.push(sample);
                    self.samples.sort_by(|s1, s2| cmp_f64(&s1.focus_pos, &s2.focus_pos));
                    ok = true;
                    self.try_cnt = 0;
                }
                subscribers.inform_focusing(FocusingEvt {
                    samples: self.samples.clone(),
                    coeffs: None,
                    result: None,
                });
            } else {
                self.try_cnt += 1;
            }
            let too_much_total_tries =
                self.try_cnt >= MAX_FOCUS_TOTAL_TRY_CNT &&
                !self.samples.is_empty();
            if ok
            || self.try_cnt >= MAX_FOCUS_SAMPLE_TRY_CNT
            || too_much_total_tries {
                result = NotifyResult::ProgressChanges;
                if self.to_go.is_empty() || too_much_total_tries {
                    let mut x = Vec::new();
                    let mut y = Vec::new();
                    for sample in &self.samples {
                        x.push(sample.focus_pos);
                        y.push(sample.stars_fwhm as f64);
                    }
                    let coeffs = square_ls(&x, &y)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus function"))?;

                    if coeffs.a2 <= 0.0 {
                        subscribers.inform_focusing(FocusingEvt {
                            samples: self.samples.clone(),
                            coeffs: Some(coeffs.clone()),
                            result: None,
                        });
                        anyhow::bail!("Wrong focuser curve result");
                    }
                    let extr = parabola_extremum(&coeffs)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus extremum"))?;
                    subscribers.inform_focusing(FocusingEvt {
                        samples: self.samples.clone(),
                        coeffs: Some(coeffs.clone()),
                        result: Some(extr),
                    });
                    let focuser_info = self.indi.focuser_get_abs_value_prop_info(&self.options.device)?;
                    if extr < focuser_info.min || extr > focuser_info.max {
                        anyhow::bail!(
                            "Focuser extremum {0:.1} out of focuser range ({1:.1}..{2:.1})",
                            extr, focuser_info.min, focuser_info.max
                        );
                    }
                    let min_sample_pos = self.samples.iter().map(|v|v.focus_pos).min_by(cmp_f64).unwrap_or_default();
                    let max_sample_pos = self.samples.iter().map(|v|v.focus_pos).max_by(cmp_f64).unwrap_or_default();
                    let min_acceptable = min_sample_pos + (max_sample_pos-min_sample_pos) * 0.33;
                    let max_acceptable = min_sample_pos + (max_sample_pos-min_sample_pos) * 0.66;
                    if extr < min_acceptable || extr > max_acceptable {
                        // Result is too far from center of samples.
                        // Will do more measures.
                        self.to_go.clear();
                        if extr < min_acceptable {
                            for i in (1..(self.options.measures+1)/2).rev() {
                                self.to_go.push_back(min_sample_pos - i as f64 * self.options.step);
                            }
                        } else {
                            for i in 1..(self.options.measures+1)/2 {
                                self.to_go.push_back(max_sample_pos + i as f64 * self.options.step);
                            }
                        }
                        self.start_sample(true)?;
                        return Ok(result);
                    }
                    if self.stage == FocusingStage::Preliminary {
                        self.start_stage(extr, FocusingStage::Final)?;
                        result = NotifyResult::ModeChanged;
                        return Ok(result)
                    }

                    self.result_pos = Some(extr);
                    // for anti-backlash first move to minimum position
                    self.indi.focuser_set_abs_value(
                        &self.options.device,
                        extr - self.options.step,
                        true,
                        None
                    )?;
                    self.state = FocusingState::WaitingResultPosAntiBacklash {
                        before_pos: extr - self.options.step,
                        begin_pos: extr
                    };
                    subscribers.inform_focusing_result(extr);
                } else {
                    self.start_sample(false)?;
                }
            } else {
                start_taking_shots(
                    &self.indi,
                    &self.frame,
                    &self.camera,
                    false
                )?;
            }
        }
        if self.state == FocusingState::WaitingResultImg {
            result = NotifyResult::Finished { next_mode: self.next_mode.take() };
        }
        Ok(result)
    }
}

///////////////////////////////////////////////////////////////////////////////

const DITHER_CALIBR_ATTEMPTS_CNT: usize = 11;
const DITHER_CALIBR_SPEED: f64 = 1.0;

#[derive(Debug, Default, Clone)]
struct MountMoveCalibrRes {
    move_x_ra: f64,
    move_y_ra: f64,
    move_x_dec: f64,
    move_y_dec: f64,
}

impl MountMoveCalibrRes {
    fn is_ok(&self) -> bool {
        self.move_x_ra != 0.0 ||
        self.move_y_ra != 0.0 ||
        self.move_x_dec != 0.0 ||
        self.move_y_dec != 0.0
    }

    fn calc(&self, x0: f64, y0: f64) -> Option<(f64, f64)> {
        let calc_t = |x1, y1, x2, y2| -> Option<f64> {
            let divider = y2 * x1 - x2 * y1;
            if divider != 0.0 {
                Some((y2 * x0 - x2 * y0) / divider)
            } else {
                None
            }
        };
        let t_ra = calc_t(self.move_x_ra, self.move_y_ra, self.move_x_dec, self.move_y_dec)?;
        let t_dec = calc_t(self.move_x_dec, self.move_y_dec, self.move_x_ra, self.move_y_ra)?;
        Some((t_ra, t_dec))
    }
}

struct MountCalibrMode {
    indi:              Arc<indi_api::Connection>,
    state:             DitherCalibrState,
    axis:              DitherCalibrAxis,
    frame:             FrameOptions,
    telescope:         TelescopeOptions,
    start_dec:         f64,
    start_ra:          f64,
    mount_device:      String,
    camera:            DeviceAndProp,
    attempt_num:       usize,
    attempts:          Vec<DitherCalibrAtempt>,
    cur_timed_guide_n: f64,
    cur_timed_guide_s: f64,
    cur_timed_guide_w: f64,
    cur_timed_guide_e: f64,
    cur_ra:            f64,
    cur_dec:           f64,
    image_width:       usize,
    image_height:      usize,
    move_period:       f64,
    result:            MountMoveCalibrRes,
    next_mode:         Option<Box<dyn Mode + Sync + Send>>,
    can_change_g_rate: bool,
    calibr_speed:      f64,
}

#[derive(PartialEq)]
enum DitherCalibrAxis {
    Undefined,
    Ra,
    Dec,
}

#[derive(PartialEq)]
enum DitherCalibrState {
    Undefined,
    WaitForImage,
    WaitForSlew,
    WaitForOrigCoords,
}

struct DitherCalibrAtempt {
    stars: Stars,
}

impl MountCalibrMode {
    fn new(
        indi:      &Arc<indi_api::Connection>,
        options:   &Arc<RwLock<Options>>,
        next_mode: Option<Box<dyn Mode + Sync + Send>>,
    ) -> Self {
        let opts = options.read().unwrap();
        let mut frame = opts.cam.frame.clone();
        frame.exp_main = opts.simp_guide.calibr_exposure;
        Self {
            indi:              Arc::clone(indi),
            state:             DitherCalibrState::Undefined,
            axis:              DitherCalibrAxis::Undefined,
            frame,
            telescope:         opts.telescope.clone(),
            start_dec:         0.0,
            start_ra:          0.0,
            mount_device:      opts.mount.device.clone(),
            camera:            opts.cam.device.clone(),
            attempt_num:       0,
            attempts:          Vec::new(),
            cur_timed_guide_n: 0.0,
            cur_timed_guide_s: 0.0,
            cur_timed_guide_w: 0.0,
            cur_timed_guide_e: 0.0,
            cur_ra:            0.0,
            cur_dec:           0.0,
            image_width:       0,
            image_height:      0,
            move_period:       0.0,
            result:            MountMoveCalibrRes::default(),
            next_mode,
            can_change_g_rate: false,
            calibr_speed:      0.0,
        }
    }

    fn start_for_axis(&mut self, axis: DitherCalibrAxis) -> anyhow::Result<()> {
        start_taking_shots(
            &self.indi,
            &self.frame,
            &self.camera,
            false
        )?;

        let guid_rate_supported = self.indi.mount_is_guide_rate_supported(&self.mount_device)?;
        self.can_change_g_rate =
            guid_rate_supported &&
            self.indi.mount_get_guide_rate_prop_data(&self.mount_device)?.perm == indi_api::PropPerm::RW;

        if self.can_change_g_rate {
            self.calibr_speed = DITHER_CALIBR_SPEED;
        } else if guid_rate_supported {
            self.calibr_speed = self.indi.mount_get_guide_rate(&self.mount_device)?.0;
        } else {
            self.calibr_speed = 1.0;
        }
        self.attempt_num = 0;
        self.state = DitherCalibrState::WaitForImage;
        self.axis = axis;
        self.attempts.clear();
        Ok(())
    }

    fn process_axis_results(&mut self) -> anyhow::Result<()> {
        struct AttemptRes {move_x: f64, move_y: f64, dist: f64}
        let mut result = Vec::new();
        for (prev, cur) in self.attempts.iter().tuple_windows() {
            let prev_points: Vec<_> = prev.stars.iter().map(|s| Point { x: s.x, y: s.y }).collect();
            let points: Vec<_> = cur.stars.iter().map(|s| Point { x: s.x, y: s.y }).collect();
            let offset = Offset::calculate(
                &prev_points,
                &points,
                self.image_width as f64,
                self.image_height as f64
            );
            if let Some(offset) = offset {
                result.push(AttemptRes{
                    move_x: offset.x,
                    move_y: offset.y,
                    dist: f64::sqrt(offset.x * offset.x + offset.y * offset.y),
                })
            }
        }
        // TODO: check result is not empty

        let dist_max = result.iter().map(|r|r.dist).max_by(cmp_f64).unwrap_or(0.0);
        let min_dist = 0.5 * dist_max;

        result.retain(|r| r.dist > min_dist);
        if self.axis == DitherCalibrAxis::Dec && result.len() >= 2 {
            result.remove(0);
        }

        let x_sum: f64 = result.iter().map(|r| r.move_x).sum();
        let y_sum: f64 = result.iter().map(|r| r.move_y).sum();
        let cnt = result.len() as f64;
        let move_x = x_sum / cnt;
        let move_x = move_x / self.move_period;

        let move_y = y_sum / cnt;
        let move_y = move_y / self.move_period;

        match self.axis {
            DitherCalibrAxis::Ra => {
                self.result.move_x_ra = move_x;
                self.result.move_y_ra = move_y;
                self.start_for_axis(DitherCalibrAxis::Dec)?;
            }
            DitherCalibrAxis::Dec => {
                self.result.move_x_dec = move_x;
                self.result.move_y_dec = move_y;
                if let Some(next_mode) = &mut self.next_mode {
                    next_mode.set_or_correct_value(&mut self.result);
                }
                self.restore_orig_coords()?;
                self.state = DitherCalibrState::WaitForOrigCoords;
            }
            _ => unreachable!()
        }
        Ok(())
    }

    fn restore_orig_coords(&self) -> anyhow::Result<()> {
        self.indi.mount_set_eq_coord(
            &self.mount_device,
            self.start_ra,
            self.start_dec,
            true, None
        )?;
        Ok(())
    }
}

impl Mode for MountCalibrMode {
    fn get_type(&self) -> ModeType {
        ModeType::DitherCalibr
    }

    fn progress_string(&self) -> String {
        match self.axis {
            DitherCalibrAxis::Undefined =>
                "Dithering calibration".to_string(),
            DitherCalibrAxis::Ra =>
                "Dithering calibration (RA)".to_string(),
            DitherCalibrAxis::Dec =>
                "Dithering calibration (DEC)".to_string(),
        }
    }

    fn abort(&mut self) -> anyhow::Result<()> {
        self.restore_orig_coords()?;
        Ok(())
    }

    fn take_next_mode(&mut self) -> Option<ModeBox> {
        self.next_mode.take()
    }

    fn cam_device(&self) -> Option<&DeviceAndProp> {
        Some(&self.camera)
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.frame.exposure())
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.attempt_num,
            total: DITHER_CALIBR_ATTEMPTS_CNT
        })
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.start_dec = self.indi.mount_get_eq_dec(&self.mount_device)?;
        self.start_ra = self.indi.mount_get_eq_ra(&self.mount_device)?;
        self.start_for_axis(DitherCalibrAxis::Ra)?;
        Ok(())
    }

    fn notify_about_light_frame_info(
        &mut self,
        info:         &LightFrameInfo,
        _subscribers: &Arc<RwLock<Subscribers>>,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if info.good_fwhm && info.good_ovality {
            if self.image_width == 0 || self.image_height == 0 {
                self.image_width = info.width;
                self.image_height = info.height;
                let cam_ccd = indi_api::CamCcd::from_ccd_prop_name(&self.camera.prop);
                if let Ok((pix_size_x, pix_size_y)) = self.indi.camera_get_pixel_size(&self.camera.name, cam_ccd) {
                    let min_size = f64::min(info.width as f64, info.height as f64);
                    let min_pix_size = f64::min(pix_size_x, pix_size_y);
                    let cam_size_mm = min_size * min_pix_size / 1000.0;
                    let camera_angle = f64::atan2(cam_size_mm, self.telescope.real_focal_length());
                    let sky_angle_is_second = 2.0 * PI / (60.0 * 60.0 * 24.0);
                    // time when point went all camera matrix on sky rotation speed = DITHER_CALIBR_SPEED
                    let cam_time = camera_angle / (sky_angle_is_second * self.calibr_speed);
                    let total_time = cam_time * 0.5; // half of matrix
                    self.move_period = total_time / (DITHER_CALIBR_ATTEMPTS_CNT - 1) as f64;
                    if self.move_period > 3.0 {
                        self.move_period = 3.0;
                    }
                } else {
                    self.move_period = 1.0;
                }
            }
            self.attempts.push(DitherCalibrAtempt {
                stars: info.stars.clone(),
            });
            self.attempt_num += 1;
            result = NotifyResult::ProgressChanges;
            if self.attempt_num >= DITHER_CALIBR_ATTEMPTS_CNT {
                result = NotifyResult::ModeChanged;
                self.process_axis_results()?;
            } else {
                let (ns, we) = match self.axis {
                    DitherCalibrAxis::Ra => (0.0, 1000.0 * self.move_period),
                    DitherCalibrAxis::Dec => (1000.0 * self.move_period, 0.0),
                    _ => unreachable!()
                };
                if self.can_change_g_rate {
                    self.indi.mount_set_guide_rate(
                        &self.mount_device,
                        DITHER_CALIBR_SPEED,
                        DITHER_CALIBR_SPEED,
                        true,
                        SET_PROP_TIMEOUT
                    )?;
                }
                self.indi.mount_timed_guide(&self.mount_device, ns, we)?;
                self.state = DitherCalibrState::WaitForSlew;
            }
        } else {
            start_taking_shots(
                &self.indi,
                &self.frame,
                &self.camera,
                false
            )?;
        }
        Ok(result)
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi_api::PropChangeEvent
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;

        if prop_change.device_name != self.mount_device {
            return Ok(result);
        }
        match self.state {
            DitherCalibrState::WaitForSlew => {
                if let ("TELESCOPE_TIMED_GUIDE_NS"|"TELESCOPE_TIMED_GUIDE_WE", indi_api::PropChange::Change { value, .. })
                = (prop_change.prop_name.as_str(), &prop_change.change) {
                    match value.elem_name.as_str() {
                        "TIMED_GUIDE_N" => self.cur_timed_guide_n = value.prop_value.as_f64()?,
                        "TIMED_GUIDE_S" => self.cur_timed_guide_s = value.prop_value.as_f64()?,
                        "TIMED_GUIDE_W" => self.cur_timed_guide_w = value.prop_value.as_f64()?,
                        "TIMED_GUIDE_E" => self.cur_timed_guide_e = value.prop_value.as_f64()?,
                        _ => {},
                    }
                    if self.cur_timed_guide_n == 0.0 && self.cur_timed_guide_s == 0.0
                    && self.cur_timed_guide_w == 0.0 && self.cur_timed_guide_e == 0.0 {
                        start_taking_shots(
                            &self.indi,
                            &self.frame,
                            &self.camera,
                            false
                        )?;
                        self.state = DitherCalibrState::WaitForImage;
                    }
                }
            }

            DitherCalibrState::WaitForOrigCoords => {
                if let ("EQUATORIAL_EOD_COORD", indi_api::PropChange::Change { value, .. })
                = (prop_change.prop_name.as_str(), &prop_change.change) {
                    match value.elem_name.as_str() {
                        "RA" => self.cur_ra = value.prop_value.as_f64()?,
                        "DEC" => self.cur_dec = value.prop_value.as_f64()?,
                        _ => {},
                    }
                    if f64::abs(self.cur_ra-self.start_ra) < 0.001
                    && f64::abs(self.cur_dec-self.start_dec) < 0.001 {
                        result = NotifyResult::Finished {
                            next_mode: self.next_mode.take()
                        };
                    }
                }
            }

            _ => {},
        }
        Ok(result)
    }

}
