use std::{
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering }, RwLock},
    collections::VecDeque,
    any::Any,
};
use bitflags::bitflags;
use itertools::Itertools;

use crate::{
    gui_camera::*,
    indi_api,
    image_raw::FrameType,
    image_info::{LightImageInfo, Stars},
    math::*,
    stars_offset::{Offset, Point},
    image_processing::{LightFrameShortInfo, LightFrameShortInfoFlags, LiveStackingData},
};

#[derive(Clone)]
pub struct Progress {
    pub cur: usize,
    pub total: usize,
}

pub struct FocusingEvt {
    pub samples: Vec<FocuserSample>,
    pub coeffs:  Option<SquareCoeffs>,
    pub result:  Option<f64>,
}

pub enum Event {
    ModeChanged,
    ModeContinued,
    Propress(Option<Progress>),
    Focusing(FocusingEvt),
    FocusResultValue{
        value: f64
    },
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

type Subscribers = Vec<Box<dyn Fn(Event) + Send + Sync + 'static>>;

pub trait Mode {
    fn get_type(&self) -> ModeType;
    fn set_value(&mut self, _value: &dyn Any) {}
    fn progress_string(&self) -> String;
    fn cam_device(&self) -> Option<&str> { None }
    fn progress(&self) -> Option<Progress> { None }
    fn get_cur_exposure(&self) -> Option<f64> { None }
    fn get_frame_options_mut(&mut self) -> Option<&mut FrameOptions> { None }
    fn get_frame_options(&self) -> Option<&FrameOptions> { None }
    fn can_be_stopped(&self) -> bool { false }
    fn can_be_continued_after_stop(&self) -> bool { false }
    fn start(&mut self, _indi: &indi_api::Connection) -> anyhow::Result<()> { Ok(()) }
    fn abort(&mut self, _indi: &indi_api::Connection) -> anyhow::Result<()> { Ok(()) }
    fn continue_work(&mut self, _indi: &indi_api::Connection) -> anyhow::Result<()> { Ok(()) }
    fn notify_indi_prop_change(&mut self, _prop_change: &indi_api::PropChangeEvent, _indi: &indi_api::Connection) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_blob_start_event(&mut self, _indi: &Arc<indi_api::Connection>, _thread_timer: &ThreadTimer) -> anyhow::Result<()> { Ok(()) }
    fn notify_about_frame_processing_started(&mut self, _indi: &indi_api::Connection) -> anyhow::Result<()> { Ok(()) }
    fn notify_about_light_frame_info(&mut self, _info: &LightImageInfo, _indi: &indi_api::Connection, _subscribers: &Subscribers) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_about_frame_processing_finished(&mut self, _indi: &indi_api::Connection, _frame_is_ok: bool) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
    fn notify_about_light_short_info(&mut self, _indi: &indi_api::Connection, _info: &LightFrameShortInfo) -> anyhow::Result<NotifyResult> { Ok(NotifyResult::Empty) }
}

bitflags! { pub struct ChangeResFlags: u32 {
    const PROGRESS_CHANGED = (1 << 0);
    const MODE_CHANGED     = (1 << 1);
    const FINISHED         = (1 << 2);
    const START_FOCUSING   = (1 << 3);
}}

pub enum NotifyResult {
    Empty,
    ProgressChanges,
    ModeChanged,
    Finished {
        next_mode: Option<Box<dyn Mode + Send + Sync>>
    },
    StartFocusing {
        options: FocuserOptions,
        frame:   FrameOptions,
        camera:  String,
    },
    StartMountCalibr {
        options: GuidingOptions,
        frame:   FrameOptions,
        mount:   String,
        camera:  String,
    }
}

pub struct State {
    mode:          Box<dyn Mode + Send + Sync>,
    finished_mode: Option<Box<dyn Mode + Send + Sync>>,
    aborted_mode:  Option<Box<dyn Mode + Send + Sync>>,
    subscribers:   Subscribers,
}

impl State {
    pub fn new() -> Self {
        Self {
            mode:          Box::new(WaitingMode),
            finished_mode: None,
            aborted_mode:  None,
            subscribers:   Vec::new(),
        }
    }

    pub fn mode(&self) -> &Box<dyn Mode + Send + Sync> {
        &self.mode
    }

    pub fn mode_mut(&mut self) -> &mut Box<dyn Mode + Send + Sync> {
        &mut self.mode
    }

    pub fn finished_mode(&self) -> &Option<Box<dyn Mode + Send + Sync>> {
        &self.finished_mode
    }

    pub fn aborted_mode(&self) -> &Option<Box<dyn Mode + Send + Sync>> {
        &self.aborted_mode
    }

    pub fn connect_indi_events(
        state:        &Arc<RwLock<State>>,
        indi:         &Arc<indi_api::Connection>,
        thread_timer: &Arc<ThreadTimer>
    ) {
        let state = Arc::clone(&state);
        let indi_clone = Arc::clone(&indi);
        let thread_timer = Arc::clone(&thread_timer);
        indi.subscribe_events(move |event| {
            match event {
                indi_api::Event::BlobStart(_) => {
                    let self_ = &mut *state.write().unwrap();
                    _ = self_.mode.notify_blob_start_event(
                        &indi_clone,
                        &thread_timer
                    ); // TODO: process error
                }
                indi_api::Event::PropChange(prop_change) => {
                    let mut self_ = state.write().unwrap();
                    let result = self_.mode.notify_indi_prop_change(
                        &prop_change,
                        &indi_clone
                    );
                    if let Ok(result) = result {
                        _ = self_.apply_change_result(&indi_clone, result); // TODO: process error
                    } // TODO: process error
                }
                _ => {}
            }
        });
    }

    pub fn subscribe_events(
        &mut self,
        fun: impl Fn(Event) + Send + Sync + 'static
    ) {
        self.subscribers.push(Box::new(fun))
    }

    fn inform_subcribers_about_mode_changed(&self) {
        for s in &self.subscribers {
            s(Event::ModeChanged);
        }
    }

    fn inform_subcribers_about_progress(&self) {
        let progress = self.mode.progress();
        for s in &self.subscribers {
            s(Event::Propress(progress.clone()));
        }
    }

    pub fn start_single_shot(
        &mut self,
        cam_name: &str,
        indi:     &indi_api::Connection,
        frame:    &FrameOptions,
    ) -> anyhow::Result<()> {
        let mut mode = CameraActiveMode::new(CamMode::SingleShot, cam_name, "", frame);
        mode.start(indi)?;
        self.mode = Box::new(mode);
        self.finished_mode = None;
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn start_live_view(
        &mut self,
        cam_name: &str,
        indi:     &indi_api::Connection,
        frame:    &FrameOptions
    ) -> anyhow::Result<()> {
        let mut mode = CameraActiveMode::new(CamMode::LiveView, cam_name, "", frame);
        mode.start(indi)?;
        self.mode = Box::new(mode);
        self.finished_mode = None;
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn start_saving_raw_frames(
        &mut self,
        cam_device:    &str,
        mount_device:  &str,
        indi:          &indi_api::Connection,
        ref_stars:     &Arc<RwLock<Option<Vec<Point>>>>,
        frame_options: &FrameOptions,
        focus_options: &FocuserOptions,
        guid_options:  &GuidingOptions,
        options:       &RawFrameOptions,
    ) -> anyhow::Result<()> {
        let mut mode = CameraActiveMode::new(
            CamMode::SavingRawFrames,
            cam_device,
            mount_device,
            frame_options
        );
        mode.progress = if options.use_cnt && options.frame_cnt != 0 {
            Some(Progress { cur: 0, total: options.frame_cnt })
        } else {
            None
        };
        mode.focus_options = Some(focus_options.clone());
        mode.guid_options = Some(guid_options.clone());
        mode.ref_stars = Some(Arc::clone(ref_stars));
        mode.start(indi)?;
        self.mode = Box::new(mode);
        self.aborted_mode = None;
        self.finished_mode = None;
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn start_live_stacking(
        &mut self,
        cam_name:      &str,
        mount_device:  &str,
        indi:          &indi_api::Connection,
        ref_stars:     &Arc<RwLock<Option<Vec<Point>>>>,
        live_stacking: &Arc<LiveStackingData>,
        frame_options: &FrameOptions,
        focus_options: &FocuserOptions,
        guid_options:  &GuidingOptions,
        _options:      &LiveStackingOptions
    ) -> anyhow::Result<()> {
        let mut mode = CameraActiveMode::new(CamMode::LiveStacking, cam_name, mount_device, frame_options);
        mode.focus_options = Some(focus_options.clone());
        mode.guid_options = Some(guid_options.clone());
        mode.ref_stars = Some(Arc::clone(ref_stars));
        mode.live_stacking = Some(Arc::clone(live_stacking));
        mode.start(indi)?;
        self.mode = Box::new(mode);
        self.aborted_mode = None;
        self.finished_mode = None;
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn start_focusing(
        &mut self,
        indi:    &indi_api::Connection,
        options: &FocuserOptions,
        frame:   &FrameOptions,
        camera:  &str,
    ) -> anyhow::Result<()> {
        self.mode.abort(indi)?;
        let mut mode = FocusingMode::new(options, frame, camera, None);
        mode.start(indi)?;
        self.mode = Box::new(mode);
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn start_mount_calibr(
        &mut self,
        indi:          &indi_api::Connection,
        frame:         &FrameOptions,
        options:       &GuidingOptions,
        mount_device:  &str,
        camera_device: &str,
    ) -> anyhow::Result<()> {
        self.mode.abort(indi)?;
        let mut mode = MountCalibrMode::new(frame, options, mount_device, camera_device, None);
        mode.start(indi)?;
        self.mode = Box::new(mode);
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn abort_active_mode(
        &mut self,
        indi: &indi_api::Connection,
    ) -> anyhow::Result<()> {
        self.mode.abort(indi)?;
        let can_be_continued = self.mode.can_be_continued_after_stop();
        let prev_mode = std::mem::replace(&mut self.mode, Box::new(WaitingMode));
        if can_be_continued {
            self.aborted_mode = Some(prev_mode);
        }
        self.finished_mode = None;
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn continue_prev_mode(
        &mut self,
        indi: &indi_api::Connection
    ) -> anyhow::Result<()> {
        let Some(perv_mode) = self.aborted_mode.take() else {
            anyhow::bail!("Aborted state is empty");
        };
        self.mode = perv_mode;
        self.mode.continue_work(indi)?;
        for s in &self.subscribers {
            s(Event::ModeContinued);
        }
        self.inform_subcribers_about_progress();
        self.inform_subcribers_about_mode_changed();
        Ok(())
    }

    pub fn notify_about_frame_processing_started(
        &mut self,
        indi: &Arc<indi_api::Connection>
    ) -> anyhow::Result<()> {
        self.mode.notify_about_frame_processing_started(indi)?;
        Ok(())
    }

    pub fn notify_about_light_frame_info(
        &mut self,
        info: &LightImageInfo,
        indi: &Arc<indi_api::Connection>
    ) -> anyhow::Result<()> {
        let res = self.mode.notify_about_light_frame_info(info, indi, &self.subscribers)?;
        self.apply_change_result(indi, res)?;
        Ok(())
    }

    pub fn notify_about_frame_processing_finished(
        &mut self,
        indi:        &Arc<indi_api::Connection>,
        frame_is_ok: bool,
    ) -> anyhow::Result<()> {
        let result = self.mode.notify_about_frame_processing_finished(indi, frame_is_ok)?;
        self.apply_change_result(indi, result)?;
        Ok(())
    }

    pub fn notify_about_light_short_info(
        &mut self,
        indi: &indi_api::Connection,
        info: &LightFrameShortInfo
    ) -> anyhow::Result<()> {
        let result = self.mode.notify_about_light_short_info(indi, info)?;
        self.apply_change_result(indi, result)?;
        Ok(())
    }

    fn apply_change_result(
        &mut self,
        indi:   &indi_api::Connection,
        result: NotifyResult
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
                    &mut self.mode,
                    next_mode.unwrap_or_else(|| Box::new(WaitingMode))
                );
                if next_is_none {
                    self.finished_mode = Some(prev_mode);
                }
                self.mode.continue_work(indi)?;
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartFocusing { options, frame, camera } => {
                self.mode.abort(indi)?;
                let prev_mode = std::mem::replace(&mut self.mode, Box::new(WaitingMode));
                let mut mode = FocusingMode::new(&options, &frame, &camera, Some(prev_mode));
                mode.start(indi)?;
                self.mode = Box::new(mode);
                mode_changed = true;
                progress_changed = true;
            }
            NotifyResult::StartMountCalibr { options, frame, camera, mount } => {
                self.mode.abort(indi)?;
                let prev_mode = std::mem::replace(&mut self.mode, Box::new(WaitingMode));
                let mut mode = MountCalibrMode::new(&frame, &options, &mount, &camera, Some(prev_mode));
                mode.start(indi)?;
                self.mode = Box::new(mode);
                mode_changed = true;
                progress_changed = true;
            }
            _ => {}
        }
        if mode_changed {
            self.inform_subcribers_about_mode_changed();
        }
        if progress_changed {
            self.inform_subcribers_about_progress();
        }
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////

fn start_taking_shots(
    indi:         &indi_api::Connection,
    frame:        &FrameOptions,
    camera_name:  &str,
    continuously: bool,
) -> anyhow::Result<()> {
    indi.command_enable_blob(
        camera_name,
        None,
        indi_api::BlobEnable::Also
    )?;
    if indi.camera_is_fast_toggle_supported(camera_name)? {
        let use_fast_toggle =
            continuously && !frame.have_to_use_delay();
        indi.camera_enable_fast_toggle(
            camera_name,
            use_fast_toggle,
            true,
            SET_PROP_TIMEOUT,
        )?;
        if use_fast_toggle {
            indi.camera_set_fast_frames_count(
                camera_name,
                100_000,
                true,
                SET_PROP_TIMEOUT,
            )?;
        }
    }
    apply_camera_options_and_take_shot(
        indi,
        camera_name,
        frame
    )?;
    Ok(())
}

fn apply_camera_options_and_take_shot(
    indi:          &indi_api::Connection,
    camera_name:   &str,
    frame:         &FrameOptions
) -> anyhow::Result<()> {
    // Polling period
    indi.set_polling_period(camera_name, 500, false, None)?;

    // Frame type
    indi.camera_set_frame_type(
        camera_name,
        frame.frame_type.to_indi_frame_type(),
        true,
        SET_PROP_TIMEOUT
    )?;

    // Frame size
    let (width, height) = indi.camera_get_max_frame_size(camera_name)?;
    let crop_width = frame.crop.translate(width);
    let crop_height = frame.crop.translate(height);
    indi.camera_set_frame_size(
        camera_name,
        (width - crop_width) / 2,
        (height - crop_height) / 2,
        crop_width,
        crop_height,
        false,
        SET_PROP_TIMEOUT
    )?;

    // Binning mode = AVG
    if indi.camera_is_binning_mode_supported(camera_name)?
    && frame.binning != Binning::Orig {
        indi.camera_set_binning_mode(
            camera_name,
            indi_api::BinningMode::Avg,
            true,
            None, //SET_PROP_TIMEOUT
        )?;
    }

    // Binning
    if indi.camera_is_binning_supported(camera_name)? {
        indi.camera_set_binning(
            camera_name,
            frame.binning.get_ratio(),
            frame.binning.get_ratio(),
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Gain
    if indi.camera_is_gain_supported(camera_name)? {
        indi.camera_set_gain(
            camera_name,
            frame.gain,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Offset
    if indi.camera_is_offset_supported(camera_name)? {
        let offset =
            if frame.frame_type == FrameType::Flats {
                0
            } else {
                frame.offset
            };
        indi.camera_set_offset(
            camera_name,
            offset as f64,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Low noise mode
    if indi.camera_is_low_noise_ctrl_supported(camera_name)? {
        indi.camera_control_low_noise(
            camera_name,
            frame.low_noise,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Capture format = RAW
    if indi.camera_is_capture_format_supported(camera_name)? {
        indi.camera_set_capture_format(
            camera_name,
            indi_api::CaptureFormat::Raw,
            true,
            SET_PROP_TIMEOUT
        )?;
    }

    // Start exposure
    indi.camera_start_exposure(camera_name, frame.exposure)?;

    Ok(())
}


///////////////////////////////////////////////////////////////////////////////

pub struct ThreadTimer {
    thread:    Option<std::thread::JoinHandle<()>>,
    commands:  Arc<Mutex<Vec<TimerCommand>>>,
    exit_flag: Arc<AtomicBool>,
}

struct TimerCommand {
    fun: Option<Box<dyn FnOnce() + Sync + Send + 'static>>,
    time: std::time::Instant,
    to_ms: u32,
}

impl Drop for ThreadTimer {
    fn drop(&mut self) {
        log::info!("Stopping ThreadTimer thread...");
        self.exit_flag.store(true, Ordering::Relaxed);
        let thread = self.thread.take().unwrap();
        _ = thread.join();
        log::info!("Done!");
    }
}

impl ThreadTimer {
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

    pub fn exec(&self, to_ms: u32, fun: impl FnOnce() + Sync + Send + 'static) {
        let mut commands = self.commands.lock().unwrap();
        let command = TimerCommand {
            fun: Some(Box::new(fun)),
            time: std::time::Instant::now(),
            to_ms,
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
                    let fun = cmd.fun.take().unwrap();
                    fun();
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
}

///////////////////////////////////////////////////////////////////////////////

struct GuidingData {
    mnt_calibr:        Option<MountMoveCalibrRes>,
    dither_x:          f64,
    dither_y:          f64,
    cur_timed_guide_n: f64,
    cur_timed_guide_s: f64,
    cur_timed_guide_w: f64,
    cur_timed_guide_e: f64,
    dither_exp_sum:    f64,
}

impl GuidingData {
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

struct CameraActiveMode {
    cam_mode:      CamMode,
    state:         CamState,
    device:        String,
    mount_device:  String,
    ref_stars:     Option<Arc<RwLock<Option<Vec<Point>>>>>,
    frame_options: FrameOptions,
    focus_options: Option<FocuserOptions>,
    guid_options:  Option<GuidingOptions>,
    progress:      Option<Progress>,
    cur_exposure:  f64,
    exp_sum:       f64,
    guid_data:     Option<GuidingData>,
    live_stacking: Option<Arc<LiveStackingData>>,
}

impl CameraActiveMode {
    fn new(
        cam_mode:     CamMode,
        device:       &str,
        mount_device: &str,
        frame:        &FrameOptions
    ) -> Self {
        Self {
            cam_mode,
            state:         CamState::Usual,
            device:        device.to_string(),
            mount_device:  mount_device.to_string(),
            ref_stars:     None,
            frame_options: frame.clone(),
            focus_options: None,
            guid_options:  None,
            progress:      None,
            cur_exposure:  frame.exposure,
            exp_sum:       0.0,
            guid_data:     None,
            live_stacking: None,
        }
    }
}

impl Mode for CameraActiveMode {
    fn get_type(&self) -> ModeType {
        match self.cam_mode {
            CamMode::SingleShot => ModeType::SingleShot,
            CamMode::LiveView => ModeType::LiveView,
            CamMode::SavingRawFrames => ModeType::SavingRawFrames,
            CamMode::LiveStacking => ModeType::LiveStacking,
        }
    }

    fn set_value(&mut self, value: &dyn Any) {
        if let Some(value) = value.downcast_ref::<MountMoveCalibrRes>() {
            let dith_data = self.guid_data.get_or_insert_with(|| GuidingData::new());
            dith_data.mnt_calibr = Some(value.clone());
            log::debug!("New mount calibration set: {:?}", dith_data.mnt_calibr);
        }
    }

    fn progress_string(&self) -> String {
        match (&self.state, &self.cam_mode) {
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
        }
    }

    fn cam_device(&self) -> Option<&str> {
        Some(self.device.as_str())
    }

    fn progress(&self) -> Option<Progress> {
        self.progress.clone()
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.cur_exposure)
    }

    fn get_frame_options_mut(&mut self) -> Option<&mut FrameOptions> {
        Some(&mut self.frame_options)
    }

    fn get_frame_options(&self) -> Option<&FrameOptions> {
        Some(&self.frame_options)
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

    fn start(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        if let Some(ref_stars) = &mut self.ref_stars {
            let mut ref_stars = ref_stars.write().unwrap();
            *ref_stars = None;
        }
        if let Some(live_stacking) = &mut self.live_stacking {
            let mut adder = live_stacking.adder.write().unwrap();
            adder.clear();
        }
        self.state = CamState::Usual;
        let continuously = match self.cam_mode {
            CamMode::SingleShot => false,
            CamMode::LiveView => false,
            CamMode::SavingRawFrames => true,
            CamMode::LiveStacking => true,
        };
        start_taking_shots(
            indi,
            &self.frame_options,
            &self.device,
            continuously
        )?;
        Ok(())
    }

    fn abort(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        indi.camera_abort_exposure(&self.device)?;
        Ok(())
    }

    fn continue_work(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        self.state = CamState::Usual;
        start_taking_shots(
            indi,
            &self.frame_options,
            &self.device,
            true
        )?;
        Ok(())
    }

    fn notify_blob_start_event(
        &mut self,
        indi:         &Arc<indi_api::Connection>,
        thread_timer: &ThreadTimer
    ) -> anyhow::Result<()> {
        if self.cam_mode == CamMode::SingleShot {
            return Ok(());
        }
        self.cur_exposure = self.frame_options.exposure;
        let fast_mode_enabled =
            indi.camera_is_fast_toggle_supported(&self.device).unwrap_or(false) &&
            indi.camera_is_fast_toggle_enabled(&self.device).unwrap_or(false);
        if !fast_mode_enabled {
            if !self.frame_options.have_to_use_delay() {
                apply_camera_options_and_take_shot(
                    indi,
                    &self.device,
                    &self.frame_options
                )?;
            } else {
                let indi = Arc::clone(indi);
                let camera = self.device.clone();
                let frame = self.frame_options.clone();

                thread_timer.exec((frame.delay * 1000.0) as u32, move || {
                    let res = apply_camera_options_and_take_shot(
                        &indi,
                        &camera,
                        &frame
                    );
                    if let Err(err) = res {
                        log::error!("{} during trying start next shot", err.to_string());
                        // TODO: show error!!!
                    }
                });
            }
        }
        Ok(())
    }

    fn notify_about_frame_processing_started(
        &mut self,
        indi: &indi_api::Connection
    ) -> anyhow::Result<()> {
        if let Some(progress) = &mut self.progress {
            if progress.cur+1 == progress.total &&
            indi.camera_is_fast_toggle_enabled(&self.device)? {
                indi.camera_abort_exposure(&self.device)?;
            }
        }
        Ok(())
    }

    fn notify_about_light_frame_info(
        &mut self,
        info:         &LightImageInfo,
        _indi:        &indi_api::Connection,
        _subscribers: &Subscribers
    ) -> anyhow::Result<NotifyResult> {
        if !info.is_ok() { return Ok(NotifyResult::Empty); }
        if let Some(guid_options) = &self.guid_options { // Guiding and dithering
            let guid_data = self.guid_data.get_or_insert_with(|| GuidingData::new());
            if (guid_options.enabled || guid_options.dith_period != 0)
            && !self.mount_device.is_empty() {
                if guid_data.mnt_calibr.is_none() { // mount moving calibration
                    return Ok(NotifyResult::StartMountCalibr {
                        options: guid_options.clone(),
                        frame:   self.frame_options.clone(),
                        camera:  self.device.clone(),
                        mount:   self.mount_device.clone(),
                    });
                }
            }
        }

        // Refocus
        let use_focus =
            self.cam_mode == CamMode::LiveStacking ||
            self.cam_mode == CamMode::SavingRawFrames;
        if let (Some(focuser_options), true) = (&self.focus_options, use_focus) {
            let mut have_to_refocus = false;
            if focuser_options.periodically && focuser_options.period_minutes != 0 {
                self.exp_sum += self.frame_options.exposure;
                let max_exp_sum = (focuser_options.period_minutes * 60) as f64;
                if self.exp_sum >= max_exp_sum {
                    have_to_refocus = true;
                    self.exp_sum = 0.0;
                }
            }
            if have_to_refocus {
                return Ok(NotifyResult::StartFocusing {
                    options: focuser_options.clone(),
                    frame:   self.frame_options.clone(),
                    camera:  self.device.clone(),
                })
            }
        }
        Ok(NotifyResult::Empty)
    }

    fn notify_about_light_short_info(
        &mut self,
        indi: &indi_api::Connection,
        info: &LightFrameShortInfo
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if info.flags.contains(LightFrameShortInfoFlags::BAD_STARS_FWHM)
        || info.flags.contains(LightFrameShortInfoFlags::BAD_STARS_OVAL) {
            return Ok(result);
        }
        if self.state == CamState::Usual {
            if let (Some(guid_options), false, Some(mut offset_x), Some(mut offset_y))
            = (&self.guid_options, self.mount_device.is_empty(), info.offset_x, info.offset_y) {
                let guid_data = self.guid_data.get_or_insert_with(|| GuidingData::new());

                if guid_options.dith_period != 0 { // dithering
                    guid_data.dither_exp_sum += info.exposure;
                    if guid_data.dither_exp_sum > (guid_options.dith_period * 60) as f64 {
                        guid_data.dither_exp_sum = 0.0;
                        let min_size = ((info.width + info.height) / 2) as f64;
                        let dither_max_size = min_size as f64 * guid_options.dith_percent / 100.0;
                        use rand::prelude::*;
                        let mut rng = rand::thread_rng();
                        guid_data.dither_x = dither_max_size * (rng.gen::<f64>() - 0.5);
                        guid_data.dither_y = dither_max_size * (rng.gen::<f64>() - 0.5);
                    }
                }

                offset_x -= guid_data.dither_x;
                offset_y -= guid_data.dither_y;
                let diff_dist = f64::sqrt(offset_x * offset_x + offset_y * offset_y);
                log::debug!("diff_dist = {}", diff_dist);
                if diff_dist > guid_options.max_error { // correvt mount position
                    log::info!(
                        "diff_dist > guid_options.max_error ({} > {}), start mount correction",
                        diff_dist,
                        guid_options.max_error
                    );
                    let mnt_calibr = guid_data.mnt_calibr.clone().unwrap_or_default();
                    if mnt_calibr.is_ok() {
                        if let Some((ra, dec)) = mnt_calibr.calc(-offset_x, -offset_y) {
                            guid_data.cur_timed_guide_n = 0.0;
                            guid_data.cur_timed_guide_s = 0.0;
                            guid_data.cur_timed_guide_w = 0.0;
                            guid_data.cur_timed_guide_e = 0.0;
                            indi.camera_abort_exposure(&self.device)?;
                            indi.mount_timed_guide(&self.mount_device, 1000.0 * dec, 1000.0 * ra)?;
                            self.state = CamState::MountCorrection;
                            result = NotifyResult::ModeChanged;
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    fn notify_about_frame_processing_finished(
        &mut self,
        indi:        &indi_api::Connection,
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
                indi.camera_abort_exposure(&self.device)?;
                result = NotifyResult::Finished { next_mode: None };
            }
        }
        Ok(result)
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi_api::PropChangeEvent,
        indi:        &indi_api::Connection,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if self.state == CamState::MountCorrection {
            if let ("TELESCOPE_TIMED_GUIDE_NS"|"TELESCOPE_TIMED_GUIDE_WE", indi_api::PropChange::Change { value, .. }, Some(guid_data))
            = (prop_change.prop_name.as_str(), &prop_change.change, &mut self.guid_data) {
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
                        indi,
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
    state:      FocusingState,
    device:     String,
    camera:     String,
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
        options:   &FocuserOptions,
        frame:     &FrameOptions,
        camera:    &str,
        next_mode: Option<Box<dyn Mode + Sync + Send>>,
    ) -> Self {
        let mut frame = frame.clone();
        frame.exposure = options.exposure;
        FocusingMode {
            state:      FocusingState::Undefined,
            device:     options.device.as_deref().unwrap_or("").to_string(),
            options:    options.clone(),
            frame,
            before_pos: 0.0,
            to_go:      VecDeque::new(),
            camera:     camera.to_string(),
            samples:    Vec::new(),
            result_pos: None,
            stage:      FocusingStage::Undef,
            try_cnt:    0,
            next_mode,
        }
    }

    fn start_stage(
        &mut self,
        indi:       &indi_api::Connection,
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
        self.start_sample(true, indi)?;
        Ok(())
    }

    fn start_sample(
        &mut self,
        first_time: bool,
        indi:       &indi_api::Connection
    ) -> anyhow::Result<()> {
        let Some(pos) = self.to_go.pop_front() else {
            return Ok(());
        };
        if !first_time {
            indi.focuser_set_abs_value(&self.device, pos, true, None)?;
            self.state = FocusingState::WaitingPosition(pos);
        } else {
            let mut before_pos = pos - self.options.step;
            let cur_pos = indi.focuser_get_abs_value(&self.device)?;
            if f64::abs(before_pos - cur_pos) < 1.0 {
                before_pos -= 1.0;
            }
            indi.focuser_set_abs_value(&self.device, before_pos, true, None)?;
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

    fn cam_device(&self) -> Option<&str> {
        Some(self.camera.as_str())
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.samples.len(),
            total: self.samples.len() + self.to_go.len() + 1
        })
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.frame.exposure)
    }

    fn can_be_stopped(&self) -> bool {
        true
    }

    fn can_be_continued_after_stop(&self) -> bool {
        false
    }

    fn start(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        let cur_pos = indi.focuser_get_abs_value(&self.device)?.round();
        self.before_pos = cur_pos;
        self.start_stage(indi, cur_pos, FocusingStage::Preliminary)?;
        Ok(())
    }

    fn abort(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        indi.camera_abort_exposure(&self.camera)?;
        indi.focuser_set_abs_value(&self.device, self.before_pos, true, None)?;
        Ok(())
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi_api::PropChangeEvent,
        indi:        &indi_api::Connection,
    ) -> anyhow::Result<NotifyResult> {
        if prop_change.device_name != self.device {
            return Ok(NotifyResult::Empty);
        }
        if let ("ABS_FOCUS_POSITION", indi_api::PropChange::Change { value, .. })
        = (prop_change.prop_name.as_str(), &prop_change.change) {
            let cur_focus = value.prop_value.as_f64()?;
            match self.state {
                FocusingState::WaitingPositionAntiBacklash {before_pos, begin_pos} => {
                    if f64::abs(cur_focus-before_pos) < 1.01 {
                        indi.focuser_set_abs_value(&self.device, begin_pos, true, None)?;
                        self.state = FocusingState::WaitingPosition(begin_pos);
                    }
                }
                FocusingState::WaitingPosition(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        start_taking_shots(indi, &self.frame, &self.camera, false)?;
                        self.state = FocusingState::WaitingFrame(desired_focus);
                    }
                }
                FocusingState::WaitingResultPosAntiBacklash { before_pos, begin_pos } => {
                    if f64::abs(cur_focus-before_pos) < 1.01 {
                        indi.focuser_set_abs_value(&self.device, begin_pos, true, None)?;
                        self.state = FocusingState::WaitingResultPos(begin_pos);
                    }
                }
                FocusingState::WaitingResultPos(desired_focus) => {
                    if f64::abs(cur_focus-desired_focus) < 1.01 {
                        start_taking_shots(indi, &self.frame, &self.camera, false)?;
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
        info:        &LightImageInfo,
        indi:        &indi_api::Connection,
        subscribers: &Subscribers,
    ) -> anyhow::Result<NotifyResult> {
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
                for s in subscribers {
                    s(Event::Focusing(FocusingEvt {
                        samples: self.samples.clone(),
                        coeffs: None,
                        result: None,
                    }));
                }
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
                        for s in subscribers {
                            s(Event::Focusing(FocusingEvt {
                                samples: self.samples.clone(),
                                coeffs: Some(coeffs.clone()),
                                result: None,
                            }));
                        }
                        anyhow::bail!("Wrong focuser curve result");
                    }
                    let extr = parabola_extremum(&coeffs)
                        .ok_or_else(|| anyhow::anyhow!("Can't find focus extremum"))?;
                    for s in subscribers {
                        s(Event::Focusing(FocusingEvt {
                            samples: self.samples.clone(),
                            coeffs: Some(coeffs.clone()),
                            result: Some(extr),
                        }));
                    }
                    let focuser_info = indi.focuser_get_abs_value_prop_info(&self.device)?;
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
                        self.start_sample(true, indi)?;
                        return Ok(result);
                    }
                    if self.stage == FocusingStage::Preliminary {
                        self.start_stage(indi, extr, FocusingStage::Final)?;
                        result = NotifyResult::ModeChanged;
                        return Ok(result)
                    }

                    self.result_pos = Some(extr);
                    // for anti-backlash first move to minimum position
                    indi.focuser_set_abs_value(
                        &self.device,
                        extr - self.options.step,
                        true,
                        None
                    )?;
                    self.state = FocusingState::WaitingResultPosAntiBacklash {
                        before_pos: extr - self.options.step,
                        begin_pos: extr
                    };
                    for s in subscribers {
                        s(Event::FocusResultValue {
                            value: extr,
                        });
                    }
                } else {
                    self.start_sample(false, indi)?;
                }
            } else {
                start_taking_shots(
                    indi,
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
const DITHER_CALIBR_TEST_PERIOD: f64 = 3.0; // seconds

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
    state:             DitherCalibrState,
    axis:              DitherCalibrAxis,
    frame:             FrameOptions,
    start_dec:         f64,
    start_ra:          f64,
    mount_device:      String,
    camera_device:     String,
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
    result:            MountMoveCalibrRes,
    next_mode:         Option<Box<dyn Mode + Sync + Send>>,
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
        frame:         &FrameOptions,
        options:       &GuidingOptions,
        mount_device:  &str,
        camera_device: &str,
        next_mode:     Option<Box<dyn Mode + Sync + Send>>,
    ) -> Self {
        let mut frame = frame.clone();
        frame.exposure = options.calibr_exposure;
        Self {
            state:             DitherCalibrState::Undefined,
            axis:              DitherCalibrAxis::Undefined,
            frame,
            start_dec:         0.0,
            start_ra:          0.0,
            mount_device:      mount_device.to_string(),
            camera_device:     camera_device.to_string(),
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
            result:            MountMoveCalibrRes::default(),
            next_mode
        }
    }

    fn start_for_axis(&mut self, indi: &indi_api::Connection, axis: DitherCalibrAxis) -> anyhow::Result<()> {
        start_taking_shots(
            indi,
            &self.frame,
            &self.camera_device,
            false
        )?;
        self.attempt_num = 0;
        self.state = DitherCalibrState::WaitForImage;
        self.axis = axis;
        self.attempts.clear();
        Ok(())
    }

    fn process_axis_results(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
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
        let move_x = move_x / DITHER_CALIBR_TEST_PERIOD;

        let move_y = y_sum / cnt;
        let move_y = move_y / DITHER_CALIBR_TEST_PERIOD;

        match self.axis {
            DitherCalibrAxis::Ra => {
                self.result.move_x_ra = move_x;
                self.result.move_y_ra = move_y;
                self.start_for_axis(indi, DitherCalibrAxis::Dec)?;
            }
            DitherCalibrAxis::Dec => {
                self.result.move_x_dec = move_x;
                self.result.move_y_dec = move_y;
                if let Some(next_mode) = &mut self.next_mode {
                    next_mode.set_value(&self.result);
                }
                self.restore_orig_coords(indi)?;
                self.state = DitherCalibrState::WaitForOrigCoords;
            }
            _ => unreachable!()
        }
        Ok(())
    }

    fn restore_orig_coords(&self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        indi.mount_set_eq_coord(
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

    fn abort(&mut self, indi: &indi_api::Connection) -> anyhow::Result<()> {
        self.restore_orig_coords(indi)?;
        Ok(())
    }

    fn cam_device(&self) -> Option<&str> {
        Some(self.camera_device.as_str())
    }

    fn get_cur_exposure(&self) -> Option<f64> {
        Some(self.frame.exposure)
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.attempt_num,
            total: DITHER_CALIBR_ATTEMPTS_CNT
        })
    }

    fn start(
        &mut self,
        indi: &indi_api::Connection
    ) -> anyhow::Result<()> {
        self.start_dec = indi.mount_get_eq_dec(&self.mount_device)?;
        self.start_ra = indi.mount_get_eq_ra(&self.mount_device)?;
        self.start_for_axis(indi, DitherCalibrAxis::Ra)?;
        Ok(())
    }

    fn notify_about_light_frame_info(
        &mut self,
        info:         &LightImageInfo,
        indi:         &indi_api::Connection,
        _subscribers: &Subscribers,
    ) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        if info.stars_fwhm_good && info.stars_ovality_good {
            self.attempts.push(DitherCalibrAtempt {
                stars: info.stars.clone(),
            });
            self.attempt_num += 1;
            result = NotifyResult::ProgressChanges;
            if self.attempt_num >= DITHER_CALIBR_ATTEMPTS_CNT {
                result = NotifyResult::ModeChanged;
                self.process_axis_results(indi)?;
            } else {
                let (ns, we) = match self.axis {
                    DitherCalibrAxis::Ra => (0.0, 1000.0 * DITHER_CALIBR_TEST_PERIOD),
                    DitherCalibrAxis::Dec => (1000.0 * DITHER_CALIBR_TEST_PERIOD, 0.0),
                    _ => unreachable!()
                };
                indi.mount_timed_guide(&self.mount_device, ns, we)?;
                self.state = DitherCalibrState::WaitForSlew;
            }
        } else {
            start_taking_shots(
                indi,
                &self.frame,
                &self.camera_device,
                false
            )?;
        }
        Ok(result)
    }

    fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi_api::PropChangeEvent,
        indi:        &indi_api::Connection,
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
                            indi,
                            &self.frame,
                            &self.camera_device,
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
