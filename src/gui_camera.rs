use std::{
    rc::Rc,
    sync::*,
    cell::{RefCell, Cell},
    path::PathBuf,
    f64::consts::PI,
    thread::JoinHandle,
};
use bitflags::bitflags;
use chrono::{DateTime, Local};
use gtk::{prelude::*, glib, glib::clone, cairo, gdk};
use serde::{Serialize, Deserialize};
use crate::{
    gui_main::*,
    indi_api,
    gtk_utils::{self, show_error_message},
    io_utils::*,
    image_processing::*,
    log_utils::*,
    image_info::*,
    image::RgbU8Data,
    image_raw::{RawAdder, FrameType},
    state::*,
    plots::*,
    math::*, stars_offset::Point,
};

pub const SET_PROP_TIMEOUT: Option<u64> = Some(2000);

bitflags! { struct DelayedFlags: u32 {
    const UPDATE_CAM_LIST        = 1 << 0;
    const START_LIVE_VIEW        = 1 << 1;
    const START_COOLING          = 1 << 2;
    const UPDATE_CTRL_WIDGETS    = 1 << 3;
    const UPDATE_RESOLUTION_LIST = 1 << 4;
    const SELECT_MAX_RESOLUTION  = 1 << 5;
    const UPDATE_FOC_LIST        = 1 << 6;
    const UPDATE_FOC_POS_NEW     = 1 << 7;
    const UPDATE_FOC_POS         = 1 << 8;
    const UPDATE_MOUNT_WIDGETS   = 1 << 9;
    const UPDATE_MOUNT_SPD_LIST  = 1 << 10;
}}

struct DelayedAction {
    countdown: u8,
    flags:     DelayedFlags,
}

impl DelayedAction {
    fn set(&mut self, flags: DelayedFlags) {
        self.flags |= flags;
        self.countdown = 2;
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
enum ImgPreviewScale {
    #[default]
    FitWindow,
    Original,
}

impl ImgPreviewScale {
    fn from_active_id(id: Option<&str>) -> Option<ImgPreviewScale> {
        match id {
            Some("fit")  => Some(ImgPreviewScale::FitWindow),
            Some("orig") => Some(ImgPreviewScale::Original),
            _            => None,
        }
    }

    fn to_active_id(&self) -> Option<&'static str> {
        match self {
            ImgPreviewScale::FitWindow => Some("fit"),
            ImgPreviewScale::Original  => Some("orig"),
        }
    }
}

impl FrameType {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("flat") => Self::Flats,
            Some("dark") => Self::Darks,
            Some("bias") => Self::Biases,
            _            => Self::Lights,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            FrameType::Lights => Some("light"),
            FrameType::Flats  => Some("flat"),
            FrameType::Darks  => Some("dark"),
            FrameType::Biases => Some("bias"),
            FrameType::Undef  => Some("light"),

        }
    }

    pub fn to_indi_frame_type(&self) -> indi_api::FrameType {
        match self {
            FrameType::Lights => indi_api::FrameType::Light,
            FrameType::Flats  => indi_api::FrameType::Flat,
            FrameType::Darks  => indi_api::FrameType::Dark,
            FrameType::Biases => indi_api::FrameType::Bias,
            FrameType::Undef  => panic!("Undefined frame type"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
pub enum Binning {
    #[default]
    Orig,
    Bin2,
    Bin3,
    Bin4,
}

impl Binning {
    fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("2") => Self::Bin2,
            Some("3") => Self::Bin3,
            Some("4") => Self::Bin4,
            _         => Self::Orig,
        }
    }

    fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::Orig => Some("1"),
            Self::Bin2 => Some("2"),
            Self::Bin3 => Some("3"),
            Self::Bin4 => Some("4"),
        }
    }

    pub fn get_ratio(&self) -> usize {
        match self {
            Self::Orig => 1,
            Self::Bin2 => 2,
            Self::Bin3 => 3,
            Self::Bin4 => 4,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone)]
pub enum Crop {
    #[default]
    None,
    P75,
    P50,
    P33,
    P25
}

impl Crop {
    fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("75") => Self::P75,
            Some("50") => Self::P50,
            Some("33") => Self::P33,
            Some("25") => Self::P25,
            _          => Self::None,
        }
    }

    fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::None => Some("100"),
            Self::P75  => Some("75"),
            Self::P50  => Some("50"),
            Self::P33  => Some("33"),
            Self::P25  => Some("25"),
        }
    }

    pub fn translate(&self, value: usize) -> usize {
        match self {
            Crop::None => value,
            Crop::P75  => 3 * value / 4,
            Crop::P50  => value / 2,
            Crop::P33  => value / 3,
            Crop::P25  => value / 4,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
struct CamCtrlOptions {
    enable_cooler: bool,
    enable_fan:    bool,
    enable_heater: bool,
    temperature:   f64,
}

impl Default for CamCtrlOptions {
    fn default() -> Self {
        Self {
            enable_cooler: false,
            enable_fan:    false,
            enable_heater: false,
            temperature:   0.0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct FrameOptions {
    pub exposure:   f64,
    pub gain:       f64,
    pub offset:     u32,
    pub frame_type: FrameType,
    pub binning:    Binning,
    pub crop:       Crop,
    pub low_noise:  bool,
    pub delay:      f64,
}

impl Default for FrameOptions {
    fn default() -> Self {
        Self {
            exposure:   5.0,
            gain:       1.0,
            offset:     0,
            frame_type: FrameType::default(),
            binning:    Binning::default(),
            crop:       Crop::default(),
            low_noise:  false,
            delay:      2.0,
        }
    }
}

impl FrameOptions {
    pub fn create_master_dark_file_name_suff(&self) -> String {
        format!("{:.1}s_g{:.0}_ofs{}", self.exposure, self.gain, self.offset)
    }

    pub fn create_master_flat_file_name_suff(&self) -> String {
        format!("g{:.0}_ofs{}", self.gain, self.offset)
    }

    pub fn have_to_use_delay(&self) -> bool {
        self.exposure < 2.0 &&
        self.delay > 0.0
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct CalibrOptions {
    dark_frame_en: bool,
    dark_frame:    Option<PathBuf>,
    flat_frame_en: bool,
    flat_frame:    Option<PathBuf>,
    hot_pixels:    bool,
}

impl Default for CalibrOptions {
    fn default() -> Self {
        Self {
            dark_frame_en: true,
            dark_frame:    None,
            flat_frame_en: true,
            flat_frame:    None,
            hot_pixels:    true,
        }
    }
}

impl CalibrOptions {
    fn into_params(&self) -> CalibrParams {
        let dark = if self.dark_frame_en {
            self.dark_frame.clone()
        } else {
            None
        };
        let flat = if self.flat_frame_en {
            self.flat_frame.clone()
        } else {
            None
        };
        CalibrParams {
            dark,
            flat,
            hot_pixels: self.hot_pixels
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RawFrameOptions {
    pub out_path:      PathBuf,
    pub frame_cnt:     usize,
    pub use_cnt:       bool,
    pub create_master: bool,
}

impl Default for RawFrameOptions {
    fn default() -> Self {
        Self {
            out_path:      PathBuf::new(),
            frame_cnt:     100,
            use_cnt:       true,
            create_master: true,
        }
    }
}

impl RawFrameOptions {
    fn check_and_correct(&mut self) -> anyhow::Result<()> {
        if self.out_path.as_os_str().is_empty() {
            let mut out_path = dirs::home_dir().unwrap();
            out_path.push("Astro");
            out_path.push("RawFrames");
            if !out_path.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            }
            self.out_path = out_path;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct LiveStackingOptions {
    pub save_orig:       bool,
    pub save_minutes:    usize,
    pub save_enabled:    bool,
    pub out_dir:         PathBuf,
}

impl Default for LiveStackingOptions {
    fn default() -> Self {
        Self {
            save_orig:       false,
            save_minutes:    5,
            save_enabled:    true,
            out_dir:         PathBuf::new(),
        }
    }
}

impl LiveStackingOptions {
    fn check_and_correct(&mut self) -> anyhow::Result<()> {
        if self.out_dir.as_os_str().is_empty() {
            let mut save_path = dirs::home_dir().unwrap();
            save_path.push("Astro");
            save_path.push("LiveStaking");
            if !save_path.is_dir() {
                std::fs::create_dir_all(&save_path)?;
            }
            self.out_dir = save_path;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct QualityOptions {
    pub use_max_fwhm:    bool,
    pub max_fwhm:        f32,
    pub use_max_ovality: bool,
    pub max_ovality:     f32,
}

impl Default for QualityOptions {
    fn default() -> Self {
        Self {
            use_max_fwhm:    false,
            max_fwhm:        20.0,
            use_max_ovality: true,
            max_ovality:     0.5,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
enum PreviewSource {
    #[default]
    OrigFrame,
    LiveStacking,
}

impl PreviewSource {
    fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("live") => Self::LiveStacking,
            _            => Self::OrigFrame,
        }
    }

    fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::OrigFrame    => Some("frame"),
            Self::LiveStacking => Some("live"),
        }
    }

}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct PreviewOptions {
    scale:         ImgPreviewScale,
    auto_black:    bool,
    gamma:         f64,
    source:        PreviewSource,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            scale:      ImgPreviewScale::default(),
            auto_black: true,
            gamma:      5.0,
            source:     PreviewSource::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct HistOptions {
    log_x:    bool,
    log_y:    bool,
    percents: bool,
}

impl Default for HistOptions {
    fn default() -> Self {
        Self {
            log_x:    false,
            log_y:    false,
            percents: true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct GuiOptions {
    paned_pos1:     i32,
    paned_pos2:     i32,
    paned_pos3:     i32,
    paned_pos4:     i32,
    cam_ctrl_exp:   bool,
    shot_exp:       bool,
    calibr_exp:     bool,
    raw_frames_exp: bool,
    live_exp:       bool,
    foc_exp:        bool,
    dith_exp:       bool,
    quality_exp:    bool,
    mount_exp:      bool,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            paned_pos1:     -1,
            paned_pos2:     -1,
            paned_pos3:     -1,
            paned_pos4:     -1,
            cam_ctrl_exp:   true,
            shot_exp:       true,
            calibr_exp:     true,
            raw_frames_exp: true,
            live_exp:       false,
            foc_exp:        false,
            dith_exp:        false,
            quality_exp:    true,
            mount_exp:      false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct FocuserOptions {
    pub device:          String,
    pub on_temp_change:  bool,
    pub max_temp_change: f64,
    pub on_fwhm_change:  bool,
    pub max_fwhm_change: u32,
    pub periodically:    bool,
    pub period_minutes:  u32,
    pub measures:        u32,
    pub step:            f64,
    pub exposure:        f64,
}

impl Default for FocuserOptions {
    fn default() -> Self {
        Self {
            device:          String::new(),
            on_temp_change:  false,
            max_temp_change: 5.0,
            on_fwhm_change:  false,
            max_fwhm_change: 20,
            periodically:    false,
            period_minutes:  120,
            measures:        11,
            step:            500.0,
            exposure:        10.0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct GuidingOptions {
    pub enabled: bool,
    pub max_error: f64,
    pub dith_period: u32, // in minutes, 0 - do not dither
    pub dith_percent: f64, // percent of image
    pub calibr_exposure: f64,
}

impl Default for GuidingOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            max_error: 5.0,
            dith_period: 0,
            dith_percent: 5.0,
            calibr_exposure: 1.0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct MountOptions {
    inv_ns: bool,
    inv_we: bool,
    speed:  Option<String>,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            inv_ns: false,
            inv_we: false,
            speed:  None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct CameraOptions {
    device:     Option<String>,
    live_view:  bool,
    ctrl:       CamCtrlOptions,
    frame:      FrameOptions,
    calibr:     CalibrOptions,
    raw_frames: RawFrameOptions,
    live:       LiveStackingOptions,
    quality:    QualityOptions,
    preview:    PreviewOptions,
    hist:       HistOptions,
    focuser:    FocuserOptions,
    guiding:    GuidingOptions,
    mount:      MountOptions,
    gui:        GuiOptions,
}

impl Default for CameraOptions {
    fn default() -> Self {
        Self {
            device:     None,
            live_view:  false,
            preview:    PreviewOptions::default(),
            ctrl:       CamCtrlOptions::default(),
            frame:      FrameOptions::default(),
            calibr:     CalibrOptions::default(),
            raw_frames: RawFrameOptions::default(),
            live:       LiveStackingOptions::default(),
            quality:    QualityOptions::default(),
            hist:       HistOptions::default(),
            focuser:    FocuserOptions::default(),
            guiding:    GuidingOptions::default(),
            mount:      MountOptions::default(),
            gui:        GuiOptions::default(),
        }
    }
}

enum MainThreadEvents {
    ShowFrameProcessingResult(FrameProcessingResult),
    StateEvent(Event),
    IndiEvent(indi_api::Event),
}


struct CameraData {
    main:               Rc<MainData>,
    cur_frame:          Arc<ResultImage>,
    ref_stars:          Arc<RwLock<Option<Vec<Point>>>>,
    calibr_images:      Arc<Mutex<CalibrImages>>,
    delayed_action:     RefCell<DelayedAction>,
    options:            RefCell<CameraOptions>,
    process_thread:     JoinHandle<()>,
    img_cmds_sender:    mpsc::Sender<Command>,
    conn_state:         RefCell<indi_api::ConnState>,
    indi_conn:          RefCell<Option<indi_api::Subscription>>,
    fn_gen:             Arc<Mutex<SeqFileNameGen>>,
    live_staking:       Arc<LiveStackingData>,
    preview_scroll_pos: RefCell<Option<((f64, f64), (f64, f64))>>,
    light_history:      RefCell<Vec<LightFrameShortInfo>>,
    raw_adder:          Arc<Mutex<RawAdder>>,
    focusing_data:      RefCell<Option<FocusingEvt>>,
    closed:             Cell<bool>,
    excl:               gtk_utils::ExclusiveCaller,
    mount_device_name:  RefCell<Option<String>>,
}

impl Drop for CameraData {
    fn drop(&mut self) {
        log::info!("CameraData dropped");
    }
}

pub fn build_ui(
    _application:   &gtk::Application,
    data:           &Rc<MainData>,
    timer_handlers: &mut TimerHandlers
) {
    let mut options = CameraOptions::default();
    gtk_utils::exec_and_show_error(&data.window, || {
        load_json_from_config_file(&mut options, "conf_camera")?;
        options.raw_frames.check_and_correct()?;
        options.live.check_and_correct()?;
        Ok(())
    });

    let delayed_action = DelayedAction{
        countdown: 0,
        flags:     DelayedFlags::empty(),
    };

    let (img_cmds_sender, process_thread) = start_process_blob_thread();
    let camera_data = Rc::new(CameraData {
        main:               Rc::clone(data),
        cur_frame:          Arc::new(ResultImage::new()),
        ref_stars:          Arc::new(RwLock::new(None)),
        calibr_images:      Arc::new(Mutex::new(CalibrImages::default())),
        delayed_action:     RefCell::new(delayed_action),
        options:            RefCell::new(options),
        process_thread,
        img_cmds_sender,
        conn_state:         RefCell::new(indi_api::ConnState::Disconnected),
        indi_conn:          RefCell::new(None),
        fn_gen:             Arc::new(Mutex::new(SeqFileNameGen::new())),
        live_staking:       Arc::new(LiveStackingData::new()),
        preview_scroll_pos: RefCell::new(None),
        light_history:      RefCell::new(Vec::new()),
        raw_adder:          Arc::new(Mutex::new(RawAdder::new())),
        focusing_data:      RefCell::new(None),
        closed:             Cell::new(false),
        excl:               gtk_utils::ExclusiveCaller::new(),
        mount_device_name:  RefCell::new(None),
    });

    connect_misc_events(&camera_data);

    connect_widgets_events_before_show_options(&camera_data);

    init_focuser_widgets(&camera_data);
    init_dithering_widgets(&camera_data);

    show_options(&camera_data);
    show_frame_options(&camera_data);

    show_total_raw_time(&camera_data);
    update_light_history_table(&camera_data);

    connect_widgets_events(&camera_data);
    connect_img_mouse_scroll_events(&camera_data);
    connect_focuser_widgets_events(&camera_data);
    connect_dithering_widgets_events(&camera_data);
    connect_mount_widgets_events(&camera_data);

    let weak_camera_data = Rc::downgrade(&camera_data);
    timer_handlers.push(Box::new(move || {
        let Some(data) = weak_camera_data.upgrade() else { return; };
        handler_timer(&data);
    }));

    data.window.connect_delete_event(clone!(@weak camera_data => @default-panic, move |_, _| {
        let res = handler_close_window(&camera_data);
        res
    }));

    update_camera_devices_list(&camera_data);
    update_focuser_devices_list(&camera_data);
    update_mount_widgets(&camera_data);
    correct_widget_properties(&camera_data);
}

fn connect_misc_events(data: &Rc<CameraData>) {
    let (main_thread_sender, main_thread_receiver) =
        glib::MainContext::channel(glib::PRIORITY_DEFAULT);

    let sender = main_thread_sender.clone();
    *data.indi_conn.borrow_mut() = Some(data.main.indi.subscribe_events(move |event| {
        sender.send(MainThreadEvents::IndiEvent(event)).unwrap();
    }));

    let mut state = data.main.state.write().unwrap();
    let sender = main_thread_sender.clone();
    state.subscribe_events(move |event| {
        sender.send(MainThreadEvents::StateEvent(event)).unwrap();
    });

    let sender = main_thread_sender.clone();
    let data = Rc::downgrade(data);
    main_thread_receiver.attach(None, move |item| {
        let Some(data) = data.upgrade() else { return Continue(false); };
        if data.closed.get() { return Continue(false); };
        match item {
            MainThreadEvents::IndiEvent(indi_api::Event::ConnChange(conn_state)) =>
                process_conn_state_event(&data, conn_state),
            MainThreadEvents::IndiEvent(indi_api::Event::PropChange(event_data)) => {
                match &event_data.change {
                    indi_api::PropChange::New(value) =>
                        process_prop_change_event(
                            &data,
                            &sender,
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            true,
                            None,
                            None,
                            &value.prop_value
                        ),
                    indi_api::PropChange::Change{value, prev_state, new_state} =>
                        process_prop_change_event(
                            &data,
                            &sender,
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            false,
                            Some(prev_state),
                            Some(new_state),
                            &value.prop_value
                        ),
                    indi_api::PropChange::Delete =>
                        process_prop_delete_event(
                            &data,
                            &event_data.device_name,
                            &event_data.prop_name,
                        ),
                };
            },

            MainThreadEvents::IndiEvent(_) => {},

            MainThreadEvents::ShowFrameProcessingResult(result) => {
                gtk_utils::exec_and_show_error(&data.main.window, || {
                    match result.data {
                        ProcessingResultData::ShotProcessingStarted(mode_type) => {
                            let mut state = data.main.state.write().unwrap();
                            if state.mode().get_type() == mode_type {
                                state.notify_about_frame_processing_started()?;
                            }
                        },
                        ProcessingResultData::ShotProcessingFinished {frame_is_ok, mode_type} => {
                            let mut state = data.main.state.write().unwrap();
                            if state.mode().get_type() == mode_type {
                                state.notify_about_frame_processing_finished(frame_is_ok)?;
                            }
                        },
                        _ => {},
                    }
                    Ok(())
                });
                show_frame_processing_result(&data, result);
            },

            MainThreadEvents::StateEvent(Event::ModeChanged) => {
                correct_widget_properties(&data);
            },

            MainThreadEvents::StateEvent(Event::ModeContinued) => {
                correct_after_continue_last_mode(&data);
            },

            MainThreadEvents::StateEvent(Event::Focusing(fdata)) => {
                *data.focusing_data.borrow_mut() = Some(fdata);
                let da_focusing = data.main.builder.object::<gtk::DrawingArea>("da_focusing").unwrap();
                da_focusing.queue_draw();
            },

            MainThreadEvents::StateEvent(Event::FocusResultValue { value }) => {
                update_focuser_position_after_focusing(&data, value);
            },

            _ => {},
        }
        Continue(true)
    });
}

fn connect_widgets_events_before_show_options(data: &Rc<CameraData>) {
    let chb_hot_pixels = data.main.builder.object::<gtk::CheckButton>("chb_hot_pixels").unwrap();
    chb_hot_pixels.connect_active_notify(clone!(@strong data => move |chb| {
        gtk_utils::enable_widgets(
            &data.main.builder,
            false,
            &[("l_hot_pixels_warn", chb.is_active())]
        )
    }));
    chb_hot_pixels.set_active(true);
    chb_hot_pixels.set_active(false);
}

fn connect_widgets_events(data: &Rc<CameraData>) {
    let bldr = &data.main.builder;
    gtk_utils::connect_action(&data.main.window, data, "take_shot",              handler_action_take_shot);
    gtk_utils::connect_action(&data.main.window, data, "stop_shot",              handler_action_stop_shot);
    gtk_utils::connect_action(&data.main.window, data, "clear_light_history",    handler_action_clear_light_history);
    gtk_utils::connect_action(&data.main.window, data, "start_save_raw_frames",  handler_action_start_save_raw_frames);
    gtk_utils::connect_action(&data.main.window, data, "stop_save_raw_frames",   handler_action_stop_save_raw_frames);
    gtk_utils::connect_action(&data.main.window, data, "continue_save_raw",      handler_action_continue_save_raw_frames);
    gtk_utils::connect_action(&data.main.window, data, "start_live_stacking",    handler_action_start_live_stacking);
    gtk_utils::connect_action(&data.main.window, data, "stop_live_stacking",     handler_action_stop_live_stacking);
    gtk_utils::connect_action(&data.main.window, data, "continue_live_stacking", handler_action_continue_live_stacking);
    gtk_utils::connect_action(&data.main.window, data, "manual_focus",           handler_action_manual_focus);
    gtk_utils::connect_action(&data.main.window, data, "stop_manual_focus",      handler_action_stop_manual_focus);
    gtk_utils::connect_action(&data.main.window, data, "start_dither_calibr",    handler_action_start_dither_calibr);
    gtk_utils::connect_action(&data.main.window, data, "stop_dither_calibr",     handler_action_stop_dither_calibr);

    let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
    cb_frame_mode.connect_active_id_notify(clone!(@strong data => move |cb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.frame.frame_type = FrameType::from_active_id(
                cb.active_id().map(|v| v.to_string()).as_deref()
            );
            drop(options);
            correct_widget_properties(&data);
        });
    }));

    let chb_cooler = bldr.object::<gtk::CheckButton>("chb_cooler").unwrap();
    chb_cooler.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.ctrl.enable_cooler = chb.is_active();
            drop(options);
            set_temperature_by_options(&data, false);
            correct_widget_properties(&data);
        });
    }));

    let chb_heater = bldr.object::<gtk::CheckButton>("chb_heater").unwrap();
    chb_heater.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.ctrl.enable_heater = chb.is_active();
            drop(options);
            set_temperature_by_options(&data, false);
            correct_widget_properties(&data);
        });
    }));

    let chb_fan = bldr.object::<gtk::CheckButton>("chb_fan").unwrap();
    chb_fan.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.ctrl.enable_fan = chb.is_active();
            drop(options);
            set_temperature_by_options(&data, false);
            correct_widget_properties(&data);
        });
    }));

    let spb_temp = bldr.object::<gtk::SpinButton>("spb_temp").unwrap();
    spb_temp.connect_value_changed(clone!(@strong data => move |spb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.ctrl.temperature = spb.value();
            drop(options);
            set_temperature_by_options(&data, false);
        });
    }));

    let chb_shots_cont = bldr.object::<gtk::CheckButton>("chb_shots_cont").unwrap();
    chb_shots_cont.connect_active_notify(clone!(@strong data => move |_| {
        data.excl.exec(|| {
            read_options_from_widgets(&data);
            correct_widget_properties(&data);
            handler_live_view_changed(&data);
        });
    }));

    let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
    cb_frame_mode.connect_active_id_notify(clone!(@strong data => move |cb| {
        data.excl.exec(|| {
            let frame_type = FrameType::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            );
            let mut state = data.main.state.write().unwrap();
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.frame_type = frame_type;
            }
        });
    }));

    let spb_exp = bldr.object::<gtk::SpinButton>("spb_exp").unwrap();
    spb_exp.connect_value_changed(clone!(@strong data => move |sb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.exposure = sb.value();
            }
            if let Ok(mut options) = data.options.try_borrow_mut() {
                options.frame.exposure = sb.value();
            }
            show_total_raw_time(&data);
        });
    }));

    let spb_gain = bldr.object::<gtk::SpinButton>("spb_gain").unwrap();
    spb_gain.connect_value_changed(clone!(@strong data => move |sb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.gain = sb.value();
            }
        });
    }));

    let spb_offset = bldr.object::<gtk::SpinButton>("spb_offset").unwrap();
    spb_offset.connect_value_changed(clone!(@strong data => move |sb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.offset = sb.value() as u32;
            }
        });
    }));

    let cb_bin = bldr.object::<gtk::ComboBoxText>("cb_bin").unwrap();
    cb_bin.connect_active_id_notify(clone!(@strong data => move |cb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            let binning = Binning::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            );
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.binning = binning;
            }
        });
    }));

    let cb_crop = bldr.object::<gtk::ComboBoxText>("cb_crop").unwrap();
    cb_crop.connect_active_id_notify(clone!(@strong data => move |cb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            let crop = Crop::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            );
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.crop = crop;
            }
        });
    }));

    let spb_delay = bldr.object::<gtk::SpinButton>("spb_delay").unwrap();
    spb_delay.connect_value_changed(clone!(@strong data => move |sb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.delay = sb.value();
            }
        });
    }));

    let chb_low_noise = bldr.object::<gtk::CheckButton>("chb_low_noise").unwrap();
    chb_low_noise.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let mut state = data.main.state.write().unwrap();
            if let Some(frame) = state.mode_mut().get_frame_options_mut() {
                frame.low_noise = chb.is_active();
            }
        });
    }));

    let spb_raw_frames_cnt = bldr.object::<gtk::SpinButton>("spb_raw_frames_cnt").unwrap();
    spb_raw_frames_cnt.connect_value_changed(clone!(@strong data => move |sb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.raw_frames.frame_cnt = sb.value() as usize;
            drop(options);
            show_total_raw_time(&data);
        });
    }));

    let da_shot_state = bldr.object::<gtk::DrawingArea>("da_shot_state").unwrap();
    da_shot_state.connect_draw(clone!(@strong data => move |area, cr| {
        handler_draw_shot_state(&data, area, cr);
        Inhibit(false)
    }));

    let cb_preview_src = bldr.object::<gtk::ComboBoxText>("cb_preview_src").unwrap();
    cb_preview_src.connect_active_id_notify(clone!(@strong data => move |cb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.preview.source = PreviewSource::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            );
            drop(options);
            show_preview_image(&data, None, None);
            repaint_histogram(&data);
            show_histogram_stat(&data);
            show_image_info(&data);
        });
    }));

    let cb_preview_scale = bldr.object::<gtk::ComboBoxText>("cb_preview_scale").unwrap();
    cb_preview_scale.connect_active_id_notify(clone!(@strong data => move |cb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.preview.scale =
                ImgPreviewScale::from_active_id(
                    cb.active_id().map(|id| id.to_string()).as_deref()
                ).unwrap_or(ImgPreviewScale::FitWindow);
            drop(options);
            show_preview_image(&data, None, None);
        });
    }));

    let chb_auto_black = bldr.object::<gtk::CheckButton>("chb_auto_black").unwrap();
    chb_auto_black.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.preview.auto_black = chb.is_active();
            drop(options);
            show_preview_image(&data, None, None);
        });
    }));

    let scl_gamma = bldr.object::<gtk::Scale>("scl_gamma").unwrap();
    scl_gamma.connect_value_changed(clone!(@strong data => move |scl| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            let new_value = (scl.value() * 10.0).round() / 10.0;
            if options.preview.gamma == new_value { return; }
            options.preview.gamma = new_value;
            drop(options);
            show_preview_image(&data, None, None);
        });
    }));

    let da_histogram = bldr.object::<gtk::DrawingArea>("da_histogram").unwrap();
    da_histogram.connect_draw(clone!(@strong data => move |area, cr| {
        handler_draw_histogram(&data, area, cr);
        Inhibit(false)
    }));

    let ch_hist_logx = bldr.object::<gtk::CheckButton>("ch_hist_logx").unwrap();
    ch_hist_logx.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.hist.log_x = chb.is_active();
            drop(options);
            repaint_histogram(&data)
        });
    }));

    let ch_hist_logy = bldr.object::<gtk::CheckButton>("ch_hist_logy").unwrap();
    ch_hist_logy.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.hist.log_y = chb.is_active();
            drop(options);
            repaint_histogram(&data)
        });
    }));

    let ch_stat_percents = bldr.object::<gtk::CheckButton>("ch_stat_percents").unwrap();
    ch_stat_percents.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Ok(mut options) = data.options.try_borrow_mut() else { return; };
            options.hist.percents = chb.is_active();
            drop(options);
            show_histogram_stat(&data)
        });
    }));

}

fn connect_img_mouse_scroll_events(data: &Rc<CameraData>) {
    let eb_preview_img = data.main.builder.object::<gtk::EventBox>("eb_preview_img").unwrap();
    let sw_preview_img = data.main.builder.object::<gtk::ScrolledWindow>("sw_preview_img").unwrap();

    eb_preview_img.connect_button_press_event(clone!(@strong data, @strong sw_preview_img => move |_, evt| {
        if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
            let hadjustment = sw_preview_img.hadjustment();
            let vadjustment = sw_preview_img.vadjustment();
            *data.preview_scroll_pos.borrow_mut() = Some((
                evt.root(),
                (hadjustment.value(), vadjustment.value())
            ));
        }
        Inhibit(false)
    }));

    eb_preview_img.connect_button_release_event(clone!(@strong data => move |_, evt| {
        if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
            *data.preview_scroll_pos.borrow_mut() = None;
        }
        Inhibit(false)
    }));

    eb_preview_img.connect_motion_notify_event(clone!(@strong data, @strong sw_preview_img => move |_, evt| {
        const SCROLL_SPEED: f64 = 2.0;
        if let Some((start_mouse_pos, start_scroll_pos)) = &*data.preview_scroll_pos.borrow() {
            let new_pos = evt.root();
            let move_x = new_pos.0 - start_mouse_pos.0;
            let move_y = new_pos.1 - start_mouse_pos.1;
            let hadjustment = sw_preview_img.hadjustment();
            hadjustment.set_value(start_scroll_pos.0 - SCROLL_SPEED*move_x);
            let vadjustment = sw_preview_img.vadjustment();
            vadjustment.set_value(start_scroll_pos.1 - SCROLL_SPEED*move_y);
        }
        Inhibit(false)
    }));
}

fn handler_close_window(data: &Rc<CameraData>) -> gtk::Inhibit {
    data.closed.set(true);

    let mut state = data.main.state.write().unwrap();
    _ = state.abort_active_mode();
    read_options_from_widgets(data);

    let options = data.options.borrow();
    _ = save_json_to_config::<CameraOptions>(&options, "conf_camera");
    drop(options);

    if let Some(indi_conn) = data.indi_conn.borrow_mut().take() {
        data.main.indi.unsubscribe(indi_conn);
    }

    gtk::Inhibit(false)
}

fn show_frame_options(data: &Rc<CameraData>) {
    data.excl.exec(|| {
        let options = data.options.borrow();
        let bld = &data.main.builder;

        gtk_utils::set_active_id(bld, "cb_frame_mode", options.frame.frame_type.to_active_id());
        gtk_utils::set_f64      (bld, "spb_exp",       options.frame.exposure);
        gtk_utils::set_f64      (bld, "spb_delay",     options.frame.delay);
        gtk_utils::set_f64      (bld, "spb_gain",      options.frame.gain);
        gtk_utils::set_f64      (bld, "spb_offset",    options.frame.offset as f64);
        gtk_utils::set_active_id(bld, "cb_bin",        options.frame.binning.to_active_id());
        gtk_utils::set_active_id(bld, "cb_crop",       options.frame.crop.to_active_id());
        gtk_utils::set_bool     (bld, "chb_low_noise", options.frame.low_noise);
    });
}

fn show_options(data: &Rc<CameraData>) {
    data.excl.exec(|| {
        let options = data.options.borrow();
        let bld = &data.main.builder;

        let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
        let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
        let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();
        let pan_cam4 = bld.object::<gtk::Paned>("pan_cam4").unwrap();

        pan_cam1.set_position(options.gui.paned_pos1);
        if options.gui.paned_pos2 != -1 {
            pan_cam2.set_position(pan_cam2.allocation().width()-options.gui.paned_pos2);
        }
        pan_cam3.set_position(options.gui.paned_pos3);
        if options.gui.paned_pos4 != -1 {
            pan_cam4.set_position(pan_cam4.allocation().height()-options.gui.paned_pos4);
        }

        gtk_utils::set_bool     (bld, "chb_shots_cont",      options.live_view);

        gtk_utils::set_bool     (bld, "chb_cooler",          options.ctrl.enable_cooler);
        gtk_utils::set_f64      (bld, "spb_temp",            options.ctrl.temperature);
        gtk_utils::set_bool     (bld, "chb_heater",          options.ctrl.enable_heater);
        gtk_utils::set_bool     (bld, "chb_fan",             options.ctrl.enable_fan);

        gtk_utils::set_bool     (bld, "chb_master_dark",     options.calibr.dark_frame_en);
        gtk_utils::set_path     (bld, "fch_master_dark",     options.calibr.dark_frame.as_deref());
        gtk_utils::set_bool     (bld, "chb_master_flat",     options.calibr.flat_frame_en);
        gtk_utils::set_path     (bld, "fch_master_flat",     options.calibr.flat_frame.as_deref());
        gtk_utils::set_bool     (bld, "chb_hot_pixels",      options.calibr.hot_pixels);

        gtk_utils::set_bool     (bld, "chb_raw_frames_cnt",  options.raw_frames.use_cnt);
        gtk_utils::set_f64      (bld, "spb_raw_frames_cnt",  options.raw_frames.frame_cnt as f64);
        gtk_utils::set_path     (bld, "fcb_raw_frames_path", Some(&options.raw_frames.out_path));
        gtk_utils::set_bool     (bld, "chb_master_frame",    options.raw_frames.create_master);

        gtk_utils::set_bool     (bld, "chb_live_save_orig",  options.live.save_orig);
        gtk_utils::set_bool     (bld, "chb_live_save",       options.live.save_enabled);
        gtk_utils::set_f64      (bld, "spb_live_minutes",    options.live.save_minutes as f64);
        gtk_utils::set_path     (bld, "fch_live_folder",     Some(&options.live.out_dir));

        gtk_utils::set_bool     (bld, "chb_max_fwhm",        options.quality.use_max_fwhm);
        gtk_utils::set_f64      (bld, "spb_max_fwhm",        options.quality.max_fwhm as f64);
        gtk_utils::set_bool     (bld, "chb_max_oval",        options.quality.use_max_ovality);
        gtk_utils::set_f64      (bld, "spb_max_oval",        options.quality.max_ovality as f64);

        gtk_utils::set_active_id(bld, "cb_preview_src",      options.preview.source.to_active_id());
        gtk_utils::set_active_id(bld, "cb_preview_scale",    options.preview.scale.to_active_id());
        gtk_utils::set_bool     (bld, "chb_auto_black",      options.preview.auto_black);
        gtk_utils::set_f64      (bld, "scl_gamma",           options.preview.gamma);

        gtk_utils::set_bool     (bld, "ch_hist_logx",        options.hist.log_x);
        gtk_utils::set_bool     (bld, "ch_hist_logy",        options.hist.log_y);
        gtk_utils::set_bool     (bld, "ch_stat_percents",    options.hist.percents);

        gtk_utils::set_bool     (bld, "chb_foc_temp",        options.focuser.on_temp_change);
        gtk_utils::set_f64      (bld, "spb_foc_temp",        options.focuser.max_temp_change);
        gtk_utils::set_bool     (bld, "chb_foc_fwhm",        options.focuser.on_fwhm_change);
        gtk_utils::set_active_id(bld, "cb_foc_fwhm",         Some(options.focuser.max_fwhm_change.to_string()).as_deref());
        gtk_utils::set_bool     (bld, "chb_foc_period",      options.focuser.periodically);
        gtk_utils::set_active_id(bld, "cb_foc_period",       Some(options.focuser.period_minutes.to_string()).as_deref());
        gtk_utils::set_f64      (bld, "spb_foc_measures",    options.focuser.measures as f64);
        gtk_utils::set_f64      (bld, "spb_foc_auto_step",   options.focuser.step);
        gtk_utils::set_f64      (bld, "spb_foc_exp",         options.focuser.exposure);

        gtk_utils::set_active_id(bld, "cb_dith_perod",     Some(options.guiding.dith_period.to_string().as_str()));
        gtk_utils::set_active_id(bld, "cb_dith_distance",  Some(format!("{:.0}", options.guiding.dith_percent * 10.0).as_str()));
        gtk_utils::set_bool     (bld, "chb_guid_enabled", options.guiding.enabled);
        gtk_utils::set_f64      (bld, "spb_guid_max_err", options.guiding.max_error);
        gtk_utils::set_f64      (bld, "spb_mnt_cal_exp",  options.guiding.calibr_exposure);

        gtk_utils::set_bool     (bld, "chb_inv_ns",      options.mount.inv_ns);
        gtk_utils::set_bool     (bld, "chb_inv_we",      options.mount.inv_we);

        gtk_utils::set_bool_prop(bld, "exp_cam_ctrl",   "expanded", options.gui.cam_ctrl_exp);
        gtk_utils::set_bool_prop(bld, "exp_shot_set",   "expanded", options.gui.shot_exp);
        gtk_utils::set_bool_prop(bld, "exp_calibr",     "expanded", options.gui.calibr_exp);
        gtk_utils::set_bool_prop(bld, "exp_raw_frames", "expanded", options.gui.raw_frames_exp);
        gtk_utils::set_bool_prop(bld, "exp_live",       "expanded", options.gui.live_exp);
        gtk_utils::set_bool_prop(bld, "exp_foc",        "expanded", options.gui.foc_exp);
        gtk_utils::set_bool_prop(bld, "exp_dith",       "expanded", options.gui.dith_exp);
        gtk_utils::set_bool_prop(bld, "exp_quality",    "expanded", options.gui.quality_exp);
        gtk_utils::set_bool_prop(bld, "exp_mount",      "expanded", options.gui.mount_exp);
    });
}

fn read_options_from_widgets(data: &Rc<CameraData>) {
    let mut options = data.options.borrow_mut();
    let bld = &data.main.builder;
    let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
    let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
    let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();
    let pan_cam4 = bld.object::<gtk::Paned>("pan_cam4").unwrap();

    options.gui.paned_pos1 = pan_cam1.position();
    options.gui.paned_pos2 = pan_cam2.allocation().width()-pan_cam2.position();
    options.gui.paned_pos3 = pan_cam3.position();
    options.gui.paned_pos4 = pan_cam4.allocation().height()-pan_cam4.position();

    options.preview.scale = {
        let preview_active_id = gtk_utils::get_active_id(bld, "cb_preview_scale");
        ImgPreviewScale::from_active_id(preview_active_id.as_deref())
            .expect("Wrong active id")
    };

    options.frame.frame_type = FrameType::from_active_id(
        gtk_utils::get_active_id(bld, "cb_frame_mode").as_deref()
    );

    options.frame.binning = Binning::from_active_id(
        gtk_utils::get_active_id(bld, "cb_bin").as_deref()
    );

    options.frame.crop = Crop::from_active_id(
        gtk_utils::get_active_id(bld, "cb_crop").as_deref()
    );

    options.preview.source = PreviewSource::from_active_id(
        gtk_utils::get_active_id(bld, "cb_preview_src").as_deref()
    );

    options.device               = gtk_utils::get_active_id(bld, "cb_camera_list");
    options.live_view            = gtk_utils::get_bool     (bld, "chb_shots_cont");

    options.ctrl.enable_cooler   = gtk_utils::get_bool     (bld, "chb_cooler");
    options.ctrl.temperature     = gtk_utils::get_f64      (bld, "spb_temp");
    options.ctrl.enable_heater   = gtk_utils::get_bool     (bld, "chb_heater");
    options.ctrl.enable_fan      = gtk_utils::get_bool     (bld, "chb_fan");

    options.frame.exposure       = gtk_utils::get_f64      (bld, "spb_exp");
    options.frame.delay          = gtk_utils::get_f64      (bld, "spb_delay");
    options.frame.gain           = gtk_utils::get_f64      (bld, "spb_gain");
    options.frame.offset         = gtk_utils::get_f64      (bld, "spb_offset") as u32;
    options.frame.low_noise      = gtk_utils::get_bool     (bld, "chb_low_noise");

    options.calibr.dark_frame_en = gtk_utils::get_bool     (bld, "chb_master_dark");
    options.calibr.dark_frame    = gtk_utils::get_pathbuf  (bld, "fch_master_dark");
    options.calibr.flat_frame_en = gtk_utils::get_bool     (bld, "chb_master_flat");
    options.calibr.flat_frame    = gtk_utils::get_pathbuf  (bld, "fch_master_flat");
    options.calibr.hot_pixels    = gtk_utils::get_bool     (bld, "chb_hot_pixels");

    options.raw_frames.use_cnt       = gtk_utils::get_bool     (bld, "chb_raw_frames_cnt");
    options.raw_frames.frame_cnt     = gtk_utils::get_f64      (bld, "spb_raw_frames_cnt") as usize;
    options.raw_frames.out_path      = gtk_utils::get_pathbuf  (bld, "fcb_raw_frames_path").unwrap_or_default();
    options.raw_frames.create_master = gtk_utils::get_bool     (bld, "chb_master_frame");

    options.live.save_orig       = gtk_utils::get_bool     (bld, "chb_live_save_orig");
    options.live.save_enabled    = gtk_utils::get_bool     (bld, "chb_live_save");
    options.live.save_minutes    = gtk_utils::get_f64      (bld, "spb_live_minutes") as usize;
    options.live.out_dir         = gtk_utils::get_pathbuf  (bld, "fch_live_folder").unwrap_or_default();

    options.quality.use_max_fwhm    = gtk_utils::get_bool     (bld, "chb_max_fwhm");
    options.quality.max_fwhm        = gtk_utils::get_f64      (bld, "spb_max_fwhm") as f32;
    options.quality.use_max_ovality = gtk_utils::get_bool     (bld, "chb_max_oval");
    options.quality.max_ovality     = gtk_utils::get_f64      (bld, "spb_max_oval") as f32;

    options.preview.auto_black   = gtk_utils::get_bool     (bld, "chb_auto_black");
    options.preview.gamma        = (gtk_utils::get_f64     (bld, "scl_gamma") * 10.0).round() / 10.0;

    options.hist.log_x           = gtk_utils::get_bool(bld, "ch_hist_logx");
    options.hist.log_y           = gtk_utils::get_bool(bld, "ch_hist_logy");
    options.hist.percents        = gtk_utils::get_bool(bld, "ch_stat_percents");

    options.focuser.on_temp_change  = gtk_utils::get_bool     (bld, "chb_foc_temp");
    options.focuser.max_temp_change = gtk_utils::get_f64      (bld, "spb_foc_temp");
    options.focuser.on_fwhm_change  = gtk_utils::get_bool     (bld, "chb_foc_fwhm");
    options.focuser.max_fwhm_change = gtk_utils::get_active_id(bld, "cb_foc_fwhm").and_then(|v| v.parse().ok()).unwrap_or(20);
    options.focuser.periodically    = gtk_utils::get_bool     (bld, "chb_foc_period");
    options.focuser.period_minutes  = gtk_utils::get_active_id(bld, "cb_foc_period").and_then(|v| v.parse().ok()).unwrap_or(120);
    options.focuser.measures        = gtk_utils::get_f64      (bld, "spb_foc_measures") as u32;
    options.focuser.step            = gtk_utils::get_f64      (bld, "spb_foc_auto_step");
    options.focuser.exposure        = gtk_utils::get_f64      (bld, "spb_foc_exp");

    options.guiding.dith_period     = gtk_utils::get_active_id(bld, "cb_dith_perod").and_then(|v| v.parse().ok()).unwrap_or(0);
    options.guiding.dith_percent    = gtk_utils::get_active_id(bld, "cb_dith_distance").and_then(|v| v.parse().ok()).unwrap_or(10.0) / 10.0;
    options.guiding.enabled         = gtk_utils::get_bool     (bld, "chb_guid_enabled");
    options.guiding.max_error       = gtk_utils::get_f64      (bld, "spb_guid_max_err");
    options.guiding.calibr_exposure = gtk_utils::get_f64      (bld, "spb_mnt_cal_exp");

    options.mount.inv_ns            = gtk_utils::get_bool     (bld, "chb_inv_ns");
    options.mount.inv_we            = gtk_utils::get_bool     (bld, "chb_inv_we");
    let mount_speed                 = gtk_utils::get_active_id(bld, "cb_mnt_speed");
    if mount_speed.is_some() {
        options.mount.speed = mount_speed;
    }

    options.gui.cam_ctrl_exp     = gtk_utils::get_bool_prop(bld, "exp_cam_ctrl",   "expanded");
    options.gui.shot_exp         = gtk_utils::get_bool_prop(bld, "exp_shot_set",   "expanded");
    options.gui.calibr_exp       = gtk_utils::get_bool_prop(bld, "exp_calibr",     "expanded");
    options.gui.raw_frames_exp   = gtk_utils::get_bool_prop(bld, "exp_raw_frames", "expanded");
    options.gui.live_exp         = gtk_utils::get_bool_prop(bld, "exp_live",       "expanded");
    options.gui.foc_exp          = gtk_utils::get_bool_prop(bld, "exp_foc",        "expanded");
    options.gui.dith_exp         = gtk_utils::get_bool_prop(bld, "exp_dith",        "expanded");
    options.gui.quality_exp      = gtk_utils::get_bool_prop(bld, "exp_quality",    "expanded");
    options.gui.mount_exp        = gtk_utils::get_bool_prop(bld, "exp_mount",      "expanded");
}

fn handler_timer(data: &Rc<CameraData>) {
    let mut delayed_action = data.delayed_action.borrow_mut();
    if delayed_action.countdown != 0 {
        delayed_action.countdown -= 1;
        if delayed_action.countdown == 0 {
            let update_cam_list_flag =
                delayed_action.flags.contains(DelayedFlags::UPDATE_CAM_LIST);
            let update_foc_list_flag =
                delayed_action.flags.contains(DelayedFlags::UPDATE_FOC_LIST);
            let start_live_view_flag =
                delayed_action.flags.contains(DelayedFlags::START_LIVE_VIEW);
            let start_cooling =
                delayed_action.flags.contains(DelayedFlags::START_COOLING);
            let update_ctrl_widgets =
                delayed_action.flags.contains(DelayedFlags::UPDATE_CTRL_WIDGETS);
            let update_res =
                delayed_action.flags.contains(DelayedFlags::UPDATE_RESOLUTION_LIST);
            let sel_max_res =
                delayed_action.flags.contains(DelayedFlags::SELECT_MAX_RESOLUTION);
            let upd_foc_pos_new_prop =
                delayed_action.flags.contains(DelayedFlags::UPDATE_FOC_POS_NEW);
            let upd_foc_pos =
                delayed_action.flags.contains(DelayedFlags::UPDATE_FOC_POS);
            let upd_mount_widgets =
                delayed_action.flags.contains(DelayedFlags::UPDATE_MOUNT_WIDGETS);
            let upd_mount_spd_list =
                delayed_action.flags.contains(DelayedFlags::UPDATE_MOUNT_SPD_LIST);
            delayed_action.flags.bits = 0;
            if update_cam_list_flag {
                update_camera_devices_list(data);
            }
            if update_foc_list_flag {
                update_focuser_devices_list(data);
            }
            if start_live_view_flag
            && data.options.borrow().live_view {
                start_live_view(data);
            }
            if start_cooling {
                set_temperature_by_options(data, true);
            }
            if update_ctrl_widgets {
                correct_widget_properties(data);
            }
            if update_res {
                update_resolution_list(data);
            }
            if sel_max_res {
                select_maximum_resolution(data);
            }
            if upd_foc_pos || upd_foc_pos_new_prop {
                update_focuser_position_widget(data, upd_foc_pos_new_prop);
            }
            if upd_mount_widgets {
                update_mount_widgets(data);
            }
            if upd_mount_spd_list {
                fill_mount_speed_list_widget(data);
            }
        }
    }
}

fn correct_widget_properties(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let bldr = &data.main.builder;
        let camera = gtk_utils::get_active_id(bldr, "cb_camera_list");
        let correct_num_adjustment_by_prop = |
            prop_info: indi_api::Result<Arc<indi_api::NumPropElemInfo>>,
            adj_name:  &str,
        | -> bool {
            if let Ok(info) = prop_info {
                let adj = bldr.object::<gtk::Adjustment>(adj_name).unwrap();
                adj.set_lower(info.min);
                adj.set_upper(info.max);
                let value = adj.value();
                if value < info.min {
                    adj.set_value(info.min);
                }
                if value > info.max {
                    adj.set_value(info.max);
                }
                let step =
                    if      info.max <= 1.0   { 0.1 }
                    else if info.max <= 10.0  { 1.0 }
                    else if info.max <= 100.0 { 10.0 }
                    else                      { 100.0 };
                adj.set_step_increment(step);
                true
            } else {
                false
            }
        };
        let temp_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            data.main.indi.camera_get_temperature_prop_info(camera),
            "adj_temp"
        )).unwrap_or(false);
        let exposure_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            data.main.indi.camera_get_exposure_prop_info(camera),
            "adj_exp"
        )).unwrap_or(false);
        let gain_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            data.main.indi.camera_get_gain_prop_info(camera),
            "adj_gain"
        )).unwrap_or(false);
        let offset_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            data.main.indi.camera_get_offset_prop_info(&camera),
            "adj_offset"
        )).unwrap_or(false);
        let bin_supported = camera.as_ref().map(|camera|
            data.main.indi.camera_is_binning_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let fan_supported = camera.as_ref().map(|camera|
            data.main.indi.camera_is_fan_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let heater_supported = camera.as_ref().map(|camera|
            data.main.indi.camera_is_heater_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let low_noise_supported = camera.as_ref().map(|camera|
            data.main.indi.camera_is_low_noise_ctrl_supported(&camera)
        ).unwrap_or(Ok(false))?;

        let indi_connected = data.main.indi.state() == indi_api::ConnState::Connected;

        let cooler_active = gtk_utils::get_bool(bldr, "chb_cooler");

        let frame_mode_str = gtk_utils::get_active_id(bldr, "cb_frame_mode");
        let frame_mode = FrameType::from_active_id(frame_mode_str.as_deref());

        let frame_mode_is_lights = frame_mode == FrameType::Lights;
        let frame_mode_is_flat = frame_mode == FrameType::Flats;
        let frame_mode_is_dark = frame_mode == FrameType::Darks;

        let state = data.main.state.read().unwrap();
        let mode_type = state.mode().get_type();
        let waiting = mode_type == ModeType::Waiting;
        let shot_active = mode_type == ModeType::SingleShot;
        let liveview_active = mode_type == ModeType::LiveView;
        let saving_frames = mode_type == ModeType::SavingRawFrames;
        let mnt_calibr = mode_type == ModeType::DitherCalibr;
        let focusing = mode_type == ModeType::Focusing;
        let saving_frames_paused = state.aborted_mode()
            .as_ref()
            .map(|mode| mode.get_type() == ModeType::SavingRawFrames)
            .unwrap_or(false);
        let live_active = mode_type == ModeType::LiveStacking;
        let livestacking_paused = state.aborted_mode()
            .as_ref()
            .map(|mode| mode.get_type() == ModeType::LiveStacking)
            .unwrap_or(false);
        let dither_calibr = mode_type == ModeType::DitherCalibr;
        drop(state);

        let focuser_sensitive =
            indi_connected &&
            gtk_utils::get_active_id(bldr, "cb_foc_list").is_some() &&
            !saving_frames &&
            !live_active &&
            !mnt_calibr &&
            !focusing;

        let dithering_sensitive =
            indi_connected &&
            data.mount_device_name.borrow().is_some() &&
            !saving_frames &&
            !live_active &&
            !mnt_calibr &&
            !focusing;

        let mount_ctrl_sensitive =
            (indi_connected &&
            data.mount_device_name.borrow().is_some() &&
            !saving_frames &&
            !live_active &&
            !mnt_calibr &&
            !focusing) ||
            (gtk_utils::get_active_id(bldr, "cb_dith_perod").as_deref() == Some("0") &&
            !gtk_utils::get_bool(bldr, "chb_guid_enabled"));

        let save_raw_btn_cap = match frame_mode {
            FrameType::Lights => "Start save\nLIGHTs",
            FrameType::Darks  => "Start save\nDARKs",
            FrameType::Biases => "Start save\nBIASes",
            FrameType::Flats  => "Start save\nFLATs",
            FrameType::Undef  => "Error :(",
        };
        gtk_utils::set_str(bldr, "btn_start_save_raw", save_raw_btn_cap);

        let can_change_cam_opts = !saving_frames && !live_active;
        let can_change_mode = waiting || shot_active;
        let can_change_frame_opts = waiting || liveview_active;
        let can_change_cal_ops = !live_active && !dither_calibr;

        gtk_utils::enable_actions(&data.main.window, &[
            ("take_shot",              exposure_supported && !shot_active && can_change_mode),
            ("stop_shot",              shot_active),

            ("start_save_raw_frames",  exposure_supported && !saving_frames && can_change_mode),
            ("stop_save_raw_frames",   saving_frames),
            ("continue_save_raw",      saving_frames_paused && can_change_mode),

            ("start_live_stacking",    exposure_supported && !live_active && can_change_mode && frame_mode_is_lights),
            ("stop_live_stacking",     live_active),
            ("continue_live_stacking", livestacking_paused && can_change_mode),

            ("manual_focus",           exposure_supported && !focusing && can_change_mode),
            ("stop_manual_focus",      focusing),

            ("start_dither_calibr",    exposure_supported && !dither_calibr && can_change_mode),
            ("stop_dither_calibr",     dither_calibr),
        ]);

        gtk_utils::show_widgets(bldr, &[
            ("chb_fan",       fan_supported),
            ("chb_heater",    heater_supported),
            ("chb_low_noise", low_noise_supported),
        ]);

        gtk_utils::enable_widgets(bldr, false, &[
            ("chb_fan",            !cooler_active),
            ("chb_cooler",         temp_supported && can_change_cam_opts),
            ("spb_temp",           cooler_active && temp_supported && can_change_cam_opts),
            ("chb_shots_cont",     (exposure_supported && liveview_active) || can_change_mode),
            ("cb_frame_mode",      can_change_frame_opts),
            ("spb_exp",            exposure_supported && can_change_frame_opts),
            ("cb_crop",            can_change_frame_opts),
            ("spb_gain",           gain_supported && can_change_frame_opts),
            ("spb_offset",         offset_supported && can_change_frame_opts && !frame_mode_is_flat),
            ("cb_bin",             bin_supported && can_change_frame_opts),
            ("chb_master_frame",   can_change_cal_ops && (frame_mode_is_flat || frame_mode_is_dark) && !saving_frames),
            ("chb_master_dark",    can_change_cal_ops),
            ("fch_master_dark",    can_change_cal_ops),
            ("chb_master_flat",    can_change_cal_ops),
            ("fch_master_flat",    can_change_cal_ops),
            ("chb_raw_frames_cnt", !saving_frames_paused),
            ("spb_raw_frames_cnt", !saving_frames_paused),

            ("grd_foc",            focuser_sensitive),
            ("grd_dither",         dithering_sensitive),
            ("bx_simple_mount",    mount_ctrl_sensitive),
            ("spb_foc_temp",       gtk_utils::get_bool(bldr, "chb_foc_temp")),
            ("cb_foc_fwhm",        gtk_utils::get_bool(bldr, "chb_foc_fwhm")),
            ("cb_foc_period",      gtk_utils::get_bool(bldr, "chb_foc_period")),
            ("spb_guid_max_err",   gtk_utils::get_bool(bldr, "chb_guid_enabled")),
        ]);

        Ok(())
    });

}

fn correct_after_continue_last_mode(data: &Rc<CameraData>) {
    read_options_from_widgets(data);

    let mut state = data.main.state.write().unwrap();
    let mut options = data.options.borrow_mut();
    let mode = state.mode_mut();
    mode.set_or_correct_value(&mut options.frame, ModeSetValueReason::Continue);
    mode.set_or_correct_value(&mut options.focuser, ModeSetValueReason::Continue);
    mode.set_or_correct_value(&mut options.guiding, ModeSetValueReason::Continue);
    drop(options);
    drop(state);

    show_frame_options(data);
    show_options(data);
}

fn update_camera_devices_list(data: &Rc<CameraData>) {
    let dev_list = data.main.indi.get_devices_list();
    let cameras = dev_list
        .iter()
        .filter(|device|
            device.interface.contains(indi_api::DriverInterface::CCD)
        );
    let cb_camera_list: gtk::ComboBoxText =
        data.main.builder.object("cb_camera_list").unwrap();
    let last_active_id = cb_camera_list.active_id().map(|s| s.to_string());
    cb_camera_list.remove_all();
    for camera in cameras {
        cb_camera_list.append(Some(&camera.name), &camera.name);
    }
    let cameras_count = gtk_utils::combobox_items_count(&cb_camera_list);
    if cameras_count == 1 {
        cb_camera_list.set_active(Some(0));
    } else if cameras_count > 1 {
        let options = data.options.borrow();
        if last_active_id.is_some() {
            cb_camera_list.set_active_id(last_active_id.as_deref());
        } else if options.device.is_some() {
            cb_camera_list.set_active_id(options.device.as_deref());
        }
        if cb_camera_list.active_id().is_none() {
            cb_camera_list.set_active(Some(0));
        }
    }
    let connected = data.main.indi.state() == indi_api::ConnState::Connected;
    gtk_utils::enable_widgets(&data.main.builder, false, &[
        ("cb_camera_list", connected && cameras_count > 1),
        ("bx_cam_ctrl",    connected && cameras_count > 0)
    ]);

    data.options.borrow_mut().device = cb_camera_list.active_id().map(|s| s.to_string());
}

fn update_resolution_list(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, ||{
        let cb_bin = data.main.builder.object::<gtk::ComboBoxText>("cb_bin").unwrap();
        let last_bin = cb_bin.active_id();
        cb_bin.remove_all();
        let options = data.options.borrow_mut();
        let Some(ref camera) = options.device else {
            return Ok(());
        };
        let (max_width, max_height) = data.main.indi.camera_get_max_frame_size(camera)?;
        let (max_hor_bin, max_vert_bin) = data.main.indi.camera_get_max_binning(camera)?;
        let max_bin = usize::min(max_hor_bin, max_vert_bin);
        let bins = [ Binning::Orig, Binning::Bin2, Binning::Bin3, Binning::Bin4 ];
        for bin in bins {
            let ratio = bin.get_ratio();
            let text = if ratio == 1 {
                format!("{} x {}", max_width, max_height)
            } else {
                format!("{} x {} (bin{})", max_width/ratio, max_height/ratio, ratio)
            };
            cb_bin.append(bin.to_active_id(), &text);
            if ratio >= max_bin { break; }
        }
        if last_bin.is_some() {
            cb_bin.set_active_id(last_bin.as_deref());
        } else {
            cb_bin.set_active_id(options.frame.binning.to_active_id());
        }
        if cb_bin.active_id().is_none() {
            cb_bin.set_active(Some(0));
        }
        Ok(())
    });
}

fn select_maximum_resolution(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let indi = &data.main.indi;
        let devices = indi.get_devices_list();
        let cameras = devices
            .iter()
            .filter(|device|
                device.interface.contains(indi_api::DriverInterface::CCD)
            );
        for camera in cameras {
            if indi.camera_is_resolution_supported(&camera.name)? {
                indi.camera_select_max_resolution(
                    &camera.name,
                    true,
                    None
                )?;
            }
        }
        Ok(())
    });
}

fn start_live_view(data: &Rc<CameraData>) {
    read_options_from_widgets(data);

    gtk_utils::exec_and_show_error(&data.main.window, || {
        let options = data.options.borrow();
        let mut state = data.main.state.write().unwrap();
        state.start_live_view(
            &options.device.as_deref().unwrap_or(""),
            &options.frame,
            &data.main.thread_timer

        )?;
        gtk_utils::set_active_id(
            &data.main.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}

fn handler_action_take_shot(data: &Rc<CameraData>) {
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        let options = data.options.borrow();
        state.start_single_shot(
            &options.device.as_deref().unwrap_or(""),
            &options.frame
        )?;
        gtk_utils::set_active_id(
            &data.main.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}

fn handler_action_stop_shot(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.abort_active_mode()?;
        Ok(())
    });
}

fn get_preview_params(
    data:    &Rc<CameraData>,
    options: &CameraOptions
) -> PreviewParams {
    let img_preview = data.main.builder.object::<gtk::Image>("img_preview").unwrap();
    let parent = img_preview
        .parent().unwrap()
        .parent().unwrap()
        .parent().unwrap();

    let (max_img_width, max_img_height) =
    if matches!(options.preview.scale, ImgPreviewScale::FitWindow) {
        let width = parent.allocation().width();
        let height = parent.allocation().height();
        (Some(width as usize), Some(height as usize))
    } else {
        (None, None)
    };

    PreviewParams {
        auto_min: options.preview.auto_black,
        gamma: options.preview.gamma,
        max_img_width,
        max_img_height,
        show_orig_frame: options.preview.source == PreviewSource::OrigFrame,
    }
}

fn show_preview_image(
    data:          &Rc<CameraData>,
    mut rgb_bytes: Option<RgbU8Data>,
    src_params:    Option<PreviewParams>,
) {
    let options = data.options.borrow();
    if src_params.is_some()
    && src_params.as_ref() != Some(&get_preview_params(data, &options)) {
        rgb_bytes = None;
    }
    let img_preview = data.main.builder.object::<gtk::Image>("img_preview").unwrap();
    let rgb_bytes = if let Some(rgb_bytes) = rgb_bytes {
        rgb_bytes
    } else {
        let preview_params = get_preview_params(data, &options);
        let result = match options.preview.source {
            PreviewSource::OrigFrame =>
                &data.cur_frame,
            PreviewSource::LiveStacking =>
                &data.live_staking.result,
        };
        let image = result.image.read().unwrap();
        if image.is_empty() {
            img_preview.clear();
            return;
        }
        let hist = result.hist.read().unwrap();
        get_rgb_bytes_from_preview_image(
            &image,
            &hist,
            &preview_params
        )
    };
    let tmr = TimeLogger::start();
    let bytes = glib::Bytes::from_owned(rgb_bytes.bytes);
    let mut pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &bytes,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        false,
        8,
        rgb_bytes.width as i32,
        rgb_bytes.height as i32,
        (rgb_bytes.width * 3) as i32,
    );
    tmr.log("Pixbuf::from_bytes");
    let pp = get_preview_params(data, &options);
    if let (Some(width), Some(height)) = (pp.max_img_width, pp.max_img_height) {
        let tmr = TimeLogger::start();
        let img_ratio = pixbuf.width() as f64 / pixbuf.height() as f64;
        let gui_ratio = width as f64 / height as f64;
        let (new_width, new_height) = if img_ratio > gui_ratio {
            (width as i32, (width as f64 / img_ratio) as i32)
        } else {
            ((height as f64 * img_ratio) as i32, height as i32)
        };
        if new_width < 42 || new_height < 42 { return; }
        pixbuf = pixbuf.scale_simple(
            new_width, new_height,
            gtk::gdk_pixbuf::InterpType::Tiles,
        ).unwrap();
        tmr.log("Pixbuf::scale_simple");
    }
    img_preview.set_pixbuf(Some(&pixbuf));
}

fn show_image_info(data: &Rc<CameraData>) {
    let options = data.options.borrow();
    let info = match options.preview.source {
        PreviewSource::OrigFrame =>
            data.cur_frame.info.read().unwrap(),
        PreviewSource::LiveStacking =>
            data.live_staking.result.info.read().unwrap(),
    };

    let bldr = &data.main.builder;
    let update_info_panel_vis = |is_light_info: bool, is_flat_info: bool, is_raw_info: bool| {
        gtk_utils::show_widgets(bldr, &[
            ("bx_light_info", is_light_info),
            ("bx_flat_info",  is_flat_info),
            ("bx_raw_info",   is_raw_info),
        ]);
    };

    match &*info {
        ResultImageInfo::LightInfo(info) => {
            gtk_utils::set_str(
                bldr,
                "e_info_exp",
                &seconds_to_total_time_str(info.exposure, true)
            );
            match info.stars_fwhm {
                Some(value) => gtk_utils::set_str(bldr, "e_fwhm", &format!("{:.1}", value)),
                None => gtk_utils::set_str(bldr, "e_fwhm", ""),
            }
            match info.stars_ovality {
                Some(value) => gtk_utils::set_str(bldr, "e_ovality", &format!("{:.1}", value)),
                None => gtk_utils::set_str(bldr, "e_ovality", ""),
            }
            let stars = info.stars.len();
            let overexp_stars = info.stars.iter().filter(|s| s.overexposured).count();
            gtk_utils::set_str(
                bldr,
                "e_stars",
                &format!("{} ({})", stars, overexp_stars)
            );
            let bg = 100_f64 * info.background as f64 / info.max_value as f64;
            gtk_utils::set_str(bldr, "e_background", &format!("{:.2}%", bg));
            let noise = 100_f64 * info.noise as f64 / info.max_value as f64;
            gtk_utils::set_str(bldr, "e_noise", &format!("{:.4}%", noise));
            update_info_panel_vis(true, false, false);
        },
        ResultImageInfo::FlatInfo(info) => {
            let show_chan = |label_id, entry_id, item: Option<&FlatInfoChan>| {
                if let Some(item) = item {
                    let text = format!(
                        "{} ({:.0}%)",
                        item.max as f64,
                        100.0 * item.max as f64 / info.max_value as f64,
                    );
                    gtk_utils::set_str(bldr, entry_id, &text);
                }
                gtk_utils::show_widgets(bldr, &[
                    (label_id, item.is_some()),
                    (entry_id, item.is_some()),
                ]);
            };
            show_chan("l_flat_r", "e_flat_r", info.r.as_ref());
            show_chan("l_flat_g", "e_flat_g", info.g.as_ref());
            show_chan("l_flat_b", "e_flat_b", info.b.as_ref());
            show_chan("l_flat_l", "e_flat_l", info.l.as_ref());
            update_info_panel_vis(false, true, false);
        },
        ResultImageInfo::RawInfo(info) => {
            gtk_utils::set_str(
                bldr, "e_aver",
                &format!(
                    "{:.1} ({:.1}%)",
                    info.aver,
                    100.0 * info.aver / info.max_value as f64
                )
            );
            gtk_utils::set_str(
                bldr, "e_median",
                &format!(
                    "{} ({:.1}%)",
                    info.median,
                    100.0 * info.median as f64 / info.max_value as f64
                )
            );
            gtk_utils::set_str(
                bldr, "e_std_dev",
                &format!(
                    "{:.1} ({:.3}%)",
                    info.std_dev,
                    100.0 * info.std_dev / info.max_value as f64
                )
            );
            update_info_panel_vis(false, false, true);
        },
        _ => {
            update_info_panel_vis(false, false, false);
        },
    }
}

fn show_frame_processing_result(
    data:   &Rc<CameraData>,
    result: FrameProcessingResult
) {
    let options = data.options.borrow();
    if options.device != Some(result.camera) { return; }
    let live_stacking_res = options.preview.source == PreviewSource::LiveStacking;
    drop(options);

    let state = data.main.state.read().unwrap();
    let mode_type = state.mode().get_type();
    drop(state);

    let show_resolution_info = |width, height| {
        gtk_utils::set_str(
            &data.main.builder,
            "e_res_info",
            &format!("{} x {}", width, height)
        );
    };

    let is_mode_current = |mode: ModeType, live_result: bool| {
        mode_type == mode &&
        live_result == live_stacking_res
    };

    match result.data {
        ProcessingResultData::Error(error_text) => {
            let mut state = data.main.state.write().unwrap();
            _ = state.abort_active_mode();
            drop(state);
            correct_widget_properties(data);
            show_error_message(&data.main.window, "Fatal Error", &error_text);
        }
        ProcessingResultData::LightShortInfo(short_info, mode) => {
            gtk_utils::exec_and_show_error(&data.main.window, || {
                let mut state = data.main.state.write().unwrap();
                if state.mode().get_type() == mode {
                    state.notify_about_light_short_info(&short_info)?;
                }
                Ok(())
            });
            data.light_history.borrow_mut().push(short_info);
            update_light_history_table(data);
        }
        ProcessingResultData::PreviewFrame(img, mode) if is_mode_current(mode, false) => {
            show_preview_image(data, Some(img.rgb_bytes), Some(img.params));
            show_resolution_info(img.image_width, img.image_height);
        }
        ProcessingResultData::PreviewLiveRes(img, mode) if is_mode_current(mode, true) => {
            show_preview_image(data, Some(img.rgb_bytes), Some(img.params));
            show_resolution_info(img.image_width, img.image_height);
        }
        ProcessingResultData::Histogram(mode) if is_mode_current(mode, false) => {
            repaint_histogram(data);
            show_histogram_stat(data);
        }
        ProcessingResultData::HistogramLiveRes(mode) if is_mode_current(mode, true) => {
            repaint_histogram(data);
            show_histogram_stat(data);
        }
        ProcessingResultData::FrameInfo(mode) => {
            gtk_utils::exec_and_show_error(&data.main.window, || {
                let info = data.cur_frame.info.read().unwrap();
                if let ResultImageInfo::LightInfo(info) = &*info {
                    let mut state = data.main.state.write().unwrap();
                    if state.mode().get_type() == mode {
                        state.notify_about_light_frame_info(info)?;
                    }
                }
                Ok(())
            });
            if is_mode_current(mode, false) {
                show_image_info(data);
            }
        }
        ProcessingResultData::FrameInfoLiveRes(mode) if is_mode_current(mode, true) => {
            show_image_info(data);
        }
        ProcessingResultData::MasterSaved { frame_type: FrameType::Flats, file_name } => {
            gtk_utils::set_path(
                &data.main.builder,
                "fch_master_flat",
                Some(&file_name)
            );
        }
        ProcessingResultData::MasterSaved { frame_type: FrameType::Darks, file_name } => {
            gtk_utils::set_path(
                &data.main.builder,
                "fch_master_dark",
                Some(&file_name)
            );
        }
        _ => {}
    }
}

fn set_temperature_by_options(
    data:      &Rc<CameraData>,
    force_set: bool,
) {
    let options = data.options.borrow();
    let Some(ref camera_name) = options.device else {
        return;
    };
    gtk_utils::exec_and_show_error(&data.main.window, || {
        // Cooler + Temperature
        if data.main.indi.camera_is_cooler_supported(camera_name)? {
            data.main.indi.camera_enable_cooler(
                camera_name,
                options.ctrl.enable_cooler,
                true,
                SET_PROP_TIMEOUT
            )?;
            if options.ctrl.enable_cooler {
                data.main.indi.camera_set_temperature(
                    camera_name,
                    options.ctrl.temperature
                )?;
            }
        }
        // Fan
        if data.main.indi.camera_is_fan_supported(camera_name)? {
            data.main.indi.camera_control_fan(
                camera_name,
                options.ctrl.enable_fan || options.ctrl.enable_cooler,
                force_set,
                SET_PROP_TIMEOUT
            )?;
        }
        // Window heater
        if data.main.indi.camera_is_heater_supported(camera_name)? {
            data.main.indi.camera_control_heater(
                camera_name,
                options.ctrl.enable_heater,
                force_set,
                SET_PROP_TIMEOUT
            )?;
        }
        Ok(())
    });
}

fn show_cur_temperature_value(
    data:        &Rc<CameraData>,
    device_name: &str,
    temparature: f64
) {
    let cur_camera = gtk_utils::get_active_id(&data.main.builder, "cb_camera_list");
    if cur_camera.as_deref() == Some(device_name) {
        gtk_utils::set_str(
            &data.main.builder,
            "l_temp_value",
            &format!("{:.1}°C", temparature)
        );
    }
}

fn handler_live_view_changed(data: &Rc<CameraData>) {
    if data.options.borrow().live_view {
        read_options_from_widgets(data);
        start_live_view(data);
    } else {
        gtk_utils::exec_and_show_error(&data.main.window, || {
            let mut state = data.main.state.write().unwrap();
            state.abort_active_mode()?;
            Ok(())
        });
    }
}

fn process_conn_state_event(
    data:       &Rc<CameraData>,
    conn_state: indi_api::ConnState
) {
    let update_devices_list =
        conn_state == indi_api::ConnState::Disconnected ||
        conn_state == indi_api::ConnState::Disconnecting;
    *data.conn_state.borrow_mut() = conn_state;
    if update_devices_list {
        update_camera_devices_list(data);
        update_focuser_devices_list(data);
    }
    correct_widget_properties(data);
}

fn process_prop_change_event(
    data:        &Rc<CameraData>,
    main_thread_sender: &glib::Sender<MainThreadEvents>,
    device_name: &str,
    prop_name:   &str,
    elem_name:   &str,
    new_prop:    bool,
    prev_state:  Option<&indi_api::PropState>,
    new_state:   Option<&indi_api::PropState>,
    value:       &indi_api::PropValue,
) {
    if let indi_api::PropValue::Blob(blob) = value {
        process_blob_event(
            data,
            main_thread_sender,
            device_name,
            prop_name,
            elem_name,
            new_prop,
            blob
        );
    } else {
        process_simple_prop_change_event(
            data,
            device_name,
            prop_name,
            elem_name,
            new_prop,
            prev_state,
            new_state,
            value
        );
    }
}

fn process_prop_delete_event(
    data:        &Rc<CameraData>,
    device_name: &str,
    prop_name:   &str,
){
    let mut mount_device_name = data.mount_device_name.borrow_mut();
    if Some(device_name) == mount_device_name.as_deref()
    && prop_name == "" {
        data.delayed_action.borrow_mut().set(
            DelayedFlags::UPDATE_MOUNT_WIDGETS
        );
        *mount_device_name = None;
        data.delayed_action.borrow_mut().set(
            DelayedFlags::UPDATE_MOUNT_WIDGETS
        );
    }
}

fn process_simple_prop_change_event(
    data:        &Rc<CameraData>,
    device_name: &str,
    prop_name:   &str,
    elem_name:   &str,
    new_prop:    bool,
    _prev_state:  Option<&indi_api::PropState>,
    _new_state:   Option<&indi_api::PropState>,
    value:       &indi_api::PropValue,
) {
    match (prop_name, elem_name, value) {
        ("DRIVER_INFO", "DRIVER_INTERFACE", _) => {
            let flag_bits = value.as_i32().unwrap_or(0);
            let flags = indi_api::DriverInterface::from_bits_truncate(flag_bits as u32);
            if flags.contains(indi_api::DriverInterface::CCD) {
                data.delayed_action.borrow_mut().set(
                    DelayedFlags::UPDATE_CAM_LIST
                );
            }
            if flags.contains(indi_api::DriverInterface::FOCUSER) {
                data.delayed_action.borrow_mut().set(
                    DelayedFlags::UPDATE_FOC_LIST
                );
            }
            if flags.contains(indi_api::DriverInterface::TELESCOPE) {
                *data.mount_device_name.borrow_mut() = Some(device_name.to_string());
                data.delayed_action.borrow_mut().set(
                    DelayedFlags::UPDATE_MOUNT_WIDGETS
                );
            }
        },
        ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE", indi_api::PropValue::Num(temp)) => {
            if new_prop {
                data.delayed_action.borrow_mut().set(
                    DelayedFlags::START_COOLING
                );
            }
            show_cur_temperature_value(data, device_name, *temp);
        },
        ("CCD_COOLER", ..)
        if new_prop => {
            data.delayed_action.borrow_mut().set(
                DelayedFlags::START_COOLING|
                DelayedFlags::UPDATE_CTRL_WIDGETS
            );
        },
        ("CCD_OFFSET", ..) | ("CCD_GAIN", ..) | ("CCD_CONTROLS", ..)
        if new_prop => {
            data.delayed_action.borrow_mut().set(
                DelayedFlags::UPDATE_CTRL_WIDGETS
            );
        },
        ("CCD_EXPOSURE", ..) => {
            if new_prop {
                data.delayed_action.borrow_mut().set(
                    DelayedFlags::START_LIVE_VIEW
                );
            } else {
                update_shot_state(data);
            }
        },

        ("CCD_RESOLUTION", ..)
        if new_prop => {
            data.delayed_action.borrow_mut().set(
                DelayedFlags::SELECT_MAX_RESOLUTION
            );
        },

        ("CCD_BINNING", ..) |
        ("CCD_INFO", "CCD_MAX_X", ..) |
        ("CCD_INFO", "CCD_MAX_Y", ..) =>
            data.delayed_action.borrow_mut().set(
                DelayedFlags::UPDATE_RESOLUTION_LIST
            ),

        ("ABS_FOCUS_POSITION", ..) => {
            show_cur_focuser_value(&data);
                data.delayed_action.borrow_mut().set(
                    if new_prop { DelayedFlags::UPDATE_FOC_POS_NEW }
                    else { DelayedFlags::UPDATE_FOC_POS }
                );
        },
        ("FOCUS_MAX", ..) => {
            data.delayed_action.borrow_mut().set(
                DelayedFlags::UPDATE_FOC_POS_NEW
            );
        },

        ("CONNECTION", ..) => {
            data.delayed_action.borrow_mut().set(
                DelayedFlags::UPDATE_MOUNT_WIDGETS
            );
        }
        ("TELESCOPE_SLEW_RATE", ..) if new_prop => {
            data.delayed_action.borrow_mut().set(
                DelayedFlags::UPDATE_MOUNT_SPD_LIST
            );
        }
        ("TELESCOPE_TRACK_STATE", "TRACK_ON", indi_api::PropValue::Switch(tracking)) => {
            show_mount_tracking_state(data, *tracking);
        }
        ("TELESCOPE_PARK", "PARK", indi_api::PropValue::Switch(parked)) => {
            show_mount_parked_state(data, *parked);
        }
        _ => {},
    }
}

fn process_blob_event(
    data:               &Rc<CameraData>,
    main_thread_sender: &glib::Sender<MainThreadEvents>,
    device_name:        &str,
    _prop_name:         &str,
    _elem_name:         &str,
    _new_prop:          bool,
    blob:               &Arc<indi_api::BlobPropValue>,
) {
    if blob.data.is_empty() { return; }
    log::debug!("process_blob_event, dl_time = {:.2}s", blob.dl_time);
    let state = data.main.state.read().unwrap();
    let Some(mode_cam_name) = state.mode().cam_device() else {
        return;
    };

    if device_name != mode_cam_name { return; }

    read_options_from_widgets(data);
    let options = data.options.borrow();
    let mut command = ProcessImageCommand{
        mode_type:       state.mode().get_type(),
        camera:          device_name.to_string(),
        flags:           ProcessImageFlags::empty(),
        blob:            Arc::clone(blob),
        frame:           Arc::clone(&data.cur_frame),
        ref_stars:       Arc::clone(&data.ref_stars),
        calibr_params:   options.calibr.into_params(),
        calibr_images:   Arc::clone(&data.calibr_images),
        fn_gen:          Arc::clone(&data.fn_gen),
        view_options:    get_preview_params(data, &options),
        frame_options:   options.frame.clone(),
        quality_options: Some(options.quality.clone()),
        save_path:       None,
        raw_adder:       None,
        live_stacking:   None,
    };
    let last_in_seq = if let Some(progress) = state.mode().progress() {
        progress.cur + 1 == progress.total
    } else {
        false
    };
    match state.mode().get_type() {
        ModeType::SavingRawFrames => {
            command.save_path = Some(options.raw_frames.out_path.clone());
            if options.raw_frames.create_master {
                command.raw_adder = Some(RawAdderParams {
                    adder: Arc::clone(&data.raw_adder),
                    save: last_in_seq,
                });
            }
            let mount_device = data.mount_device_name.borrow();
            let mount_device = mount_device.as_deref().unwrap_or_default();
            if options.frame.frame_type == FrameType::Lights
            && !mount_device.is_empty() && options.guiding.enabled {
                command.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
            }
        }
        ModeType::LiveStacking => {
            command.save_path = Some(options.live.out_dir.clone());
            command.live_stacking = Some(LiveStackingParams {
                data:    Arc::clone(&data.live_staking),
                options: options.live.clone(),
            });
            command.flags |= ProcessImageFlags::CALC_STARS_OFFSET;
        }
        ModeType::Focusing => {
            if let Some(quality_options) = &mut command.quality_options {
                quality_options.use_max_fwhm = false;
            }
        }
        _ => {}
    };
    let result_fun = {
        let main_thread_sender = main_thread_sender.clone();
        move |res: FrameProcessingResult| {
            let send_err = main_thread_sender.send(
                MainThreadEvents::ShowFrameProcessingResult(res)
            );
            if let Err(err) = send_err {
                log::error!("process_blob_event: err={:?}", err);
            }
        }
    };
    data.img_cmds_sender.send(Command::ProcessImage{
        command,
        result_fun: Box::new(result_fun),
    }).unwrap();
}

fn repaint_histogram(data: &Rc<CameraData>) {
    let da_histogram = data.main.builder.object::<gtk::DrawingArea>("da_histogram").unwrap();
    da_histogram.queue_draw();
}

fn show_histogram_stat(data: &Rc<CameraData>) {
    let options = data.options.borrow();
    let hist = match options.preview.source {
        PreviewSource::OrigFrame =>
            data.cur_frame.hist.read().unwrap(),
        PreviewSource::LiveStacking =>
            data.live_staking.result.hist.read().unwrap(),
    };
    let bldr = &data.main.builder;
    let max = hist.max as f64;
    let show_chan_data = |chan: &Option<HistogramChan>, l_cap, l_mean, l_median, l_dev| {
        if let Some(chan) = chan.as_ref() {
            let median = chan.get_nth_element(chan.count/2);

            if options.hist.percents {
                gtk_utils::set_str(
                    bldr, l_mean,
                    &format!("{:.1}%", 100.0 * chan.mean / max)
                );
                gtk_utils::set_str(
                    bldr, l_median,
                    &format!("{:.1}%", 100.0 * median as f64 / max)
                );
                gtk_utils::set_str(
                    bldr, l_dev,
                    &format!("{:.1}%", 100.0 * chan.std_dev / max)
                );
            } else {
                gtk_utils::set_str(
                    bldr, l_mean,
                    &format!("{:.1}", chan.mean)
                );
                gtk_utils::set_str(
                    bldr, l_median,
                    &format!("{:.1}", median)
                );
                gtk_utils::set_str(
                    bldr, l_dev,
                    &format!("{:.1}", chan.std_dev)
                );
            }
        }
        gtk_utils::show_widgets(bldr, &[
            (l_cap,    chan.is_some()),
            (l_mean,   chan.is_some()),
            (l_median, chan.is_some()),
            (l_dev,    chan.is_some()),
        ]);
    };
    show_chan_data(&hist.r, "l_hist_r_cap", "l_hist_r_mean", "l_hist_r_median", "l_hist_r_dev");
    show_chan_data(&hist.g, "l_hist_g_cap", "l_hist_g_mean", "l_hist_g_median", "l_hist_g_dev");
    show_chan_data(&hist.b, "l_hist_b_cap", "l_hist_b_mean", "l_hist_b_median", "l_hist_b_dev");
    show_chan_data(&hist.l, "l_hist_l_cap", "l_hist_l_mean", "l_hist_l_median", "l_hist_l_dev");
}

fn handler_draw_histogram(
    data: &Rc<CameraData>,
    area: &gtk::DrawingArea,
    cr:   &cairo::Context
) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let options = data.options.borrow();
        let hist = match options.preview.source {
            PreviewSource::OrigFrame =>
                data.cur_frame.hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                data.live_staking.result.hist.read().unwrap(),
        };
        paint_histogram(
            &hist,
            area,
            cr,
            area.allocated_width(),
            area.allocated_height(),
            options.hist.log_x,
            options.hist.log_y,
        )?;
        Ok(())
    });
}

fn paint_histogram(
    hist:   &Histogram,
    area:   &gtk::DrawingArea,
    cr:     &cairo::Context,
    width:  i32,
    height: i32,
    log_x:  bool,
    log_y:  bool,
) -> anyhow::Result<()> {
    if width == 0 { return Ok(()); }

    let p0 = "0%";
    let p25 = "25%";
    let p50 = "50%";
    let p75 = "75%";
    let p100 = "100%";

    let fg = area.style_context().color(gtk::StateFlags::NORMAL);
    let bg = area.style_context().lookup_color("theme_base_color").unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));

    cr.set_font_size(12.0);

    let max_y_text_te = cr.text_extents(p100)?;
    let left_margin = max_y_text_te.width() + 5.0;
    let right_margin = 3.0;
    let top_margin = 3.0;
    let bottom_margin = cr.text_extents(p0)?.width() + 3.0;
    let area_width = width as f64 - left_margin - right_margin;
    let area_height = height as f64 - top_margin - bottom_margin;

    cr.set_line_width(1.0);

    cr.set_source_rgb(bg.red(), bg.green(), bg.blue());
    cr.rectangle(left_margin, top_margin, area_width, area_height);
    cr.fill()?;

    cr.set_source_rgba(fg.red(), fg.green(), fg.blue(), 0.5);
    cr.rectangle(left_margin, top_margin, area_width, area_height);
    cr.stroke()?;

    let hist_chans = [ hist.r.as_ref(), hist.g.as_ref(), hist.b.as_ref(), hist.l.as_ref() ];

    let max_count = hist_chans.iter()
        .filter_map(|v| v.map(|v| v.count))
        .max()
        .unwrap_or(0);

    let total_max_v = hist_chans.iter()
        .filter_map(|v| v.map(|v| (v.count, v.freq.iter().max())))
        .map(|(cnt, v)| *v.unwrap_or(&0) as u64 * max_count as u64 / cnt as u64)
        .max()
        .unwrap_or(0);

    if total_max_v != 0 && max_count != 0 {
        let mut total_max_v = total_max_v as f64;
        if log_y {
            total_max_v = f64::log10(total_max_v);
        }

        let paint_channel = |chan: &Option<HistogramChan>, r, g, b, a| -> anyhow::Result<()> {
            let Some(chan) = chan.as_ref() else { return Ok(()); };
            let k = max_count as f64 / chan.count as f64;
            let max_x = if !log_x { hist.max as f64 } else { f64::log10(hist.max as f64) };
            cr.set_source_rgba(r, g, b, a);
            cr.set_line_width(2.0);
            let div = hist.max as usize / width as usize;
            let mut idx_sum = 0_usize;
            let mut max_v = 0_usize;
            let mut cnt = 0_usize;
            let last_idx = chan.freq.len()-1;
            cr.move_to(left_margin, top_margin + area_height);
            for (idx, v) in chan.freq.iter().enumerate() {
                idx_sum += idx;
                if *v as usize > max_v {
                    max_v = *v as usize;
                }
                cnt += 1;
                if (cnt == div || idx == last_idx) && cnt != 0 {
                    let mut max_v_f = k * max_v as f64;
                    let mut aver_idx = (idx_sum / cnt) as f64;
                    if log_x && aver_idx != 0.0 {
                        aver_idx = f64::log10(aver_idx);
                    }
                    if log_y && max_v_f != 0.0 {
                        max_v_f = f64::log10(max_v_f);
                    }

                    let x = area_width * aver_idx / max_x;
                    let y = area_height - area_height * max_v_f / total_max_v;
                    cr.line_to(x + left_margin, y + top_margin);
                    idx_sum = 0;
                    max_v = 0;
                    cnt = 0;
                }
            }
            cr.line_to(left_margin + area_width, top_margin + area_height);
            cr.close_path();
            cr.fill_preserve()?;
            cr.stroke()?;
            Ok(())
        };

        paint_channel(&hist.r, 1.0, 0.0, 0.0, 1.0)?;
        paint_channel(&hist.g, 0.0, 2.0, 0.0, 0.5)?;
        paint_channel(&hist.b, 0.0, 0.0, 3.3, 0.33)?;
        paint_channel(&hist.l, 0.5, 0.5, 0.5, 1.0)?;
    }

    cr.set_line_width(1.0);
    cr.set_source_rgb(fg.red(), fg.green(), fg.blue());
    cr.move_to(0.0, top_margin+max_y_text_te.height());
    cr.show_text(p100)?;
    cr.move_to(0.0, height as f64 - bottom_margin);
    cr.show_text(p0)?;

    let paint_x_percent = |x, text| -> anyhow::Result<()> {
        let te = cr.text_extents(text)?;
        let mut tx = x-te.width()/2.0;
        if tx + te.width() > width as f64 {
            tx = width as f64 - te.width();
        }

        cr.move_to(x, top_margin+area_height-3.0);
        cr.line_to(x, top_margin+area_height+3.0);
        cr.stroke()?;

        cr.move_to(tx, top_margin+area_height-te.y_bearing()+3.0);
        cr.show_text(text)?;
        Ok(())
    };

    paint_x_percent(left_margin, p0)?;
    paint_x_percent(left_margin+area_width*0.25, p25)?;
    paint_x_percent(left_margin+area_width*0.50, p50)?;
    paint_x_percent(left_margin+area_width*0.75, p75)?;
    paint_x_percent(left_margin+area_width, p100)?;

    Ok(())
}

fn handler_action_start_live_stacking(data: &Rc<CameraData>) {
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        let options = data.options.borrow();
        let mount_device_name = data.mount_device_name.borrow();
        state.start_live_stacking(
            options.device.as_deref().unwrap_or(""),
            mount_device_name.as_deref().unwrap_or(""),
            &data.ref_stars,
            &data.live_staking,
            &options.frame,
            &options.focuser,
            &options.guiding,
            &options.live,
            &data.main.thread_timer
        )?;
        gtk_utils::set_active_id(
            &data.main.builder,
            "cb_preview_src",
            Some("live")
        );
        Ok(())
    });
}

fn handler_action_stop_live_stacking(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.abort_active_mode()?;
        Ok(())
    });
}

fn handler_action_continue_live_stacking(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.continue_prev_mode()?;
        Ok(())
    });
}

fn update_shot_state(data: &Rc<CameraData>) {
    let draw_area = data.main.builder.object::<gtk::DrawingArea>("da_shot_state").unwrap();
    draw_area.queue_draw();
}

fn handler_draw_shot_state(
    data: &Rc<CameraData>,
    area: &gtk::DrawingArea,
    cr:   &cairo::Context
) {
    let state = data.main.state.read().unwrap();
    let Some(cur_exposure) = state.mode().get_cur_exposure() else {
        return;
    };
    if cur_exposure < 1.0 { return; };
    let options = data.options.borrow();
    let Some(camera) = options.device.as_ref() else { return; };
    let Ok(exposure) = data.main.indi.camera_get_exposure(camera) else { return ; };
    let progress = ((cur_exposure - exposure) / cur_exposure).max(0.0).min(1.0);
    let text_to_show = format!("{:.0} / {:.0}", cur_exposure - exposure, cur_exposure);
    gtk_utils::exec_and_show_error(&data.main.window, || {
        gtk_utils::draw_progress_bar(area, cr, progress, &text_to_show)
    });
}

fn update_light_history_table(data: &Rc<CameraData>) {
    let tree: gtk::TreeView = data.main.builder.object("tv_light_history").unwrap();
    let model = match tree.model() {
        Some(model) => {
            model.downcast::<gtk::ListStore>().unwrap()
        },
        None => {
            let model = gtk::ListStore::new(&[
                String::static_type(),
                String::static_type(), String::static_type(),
                u32   ::static_type(), String::static_type(),
                String::static_type(), String::static_type(),
                String::static_type(), String::static_type(),
            ]);
            let columns = [
                /* 0 */ "Time",
                /* 1 */ "FWHM",
                /* 2 */ "Ovality",
                /* 3 */ "Stars",
                /* 4 */ "Noise",
                /* 5 */ "Background",
                /* 6 */ "Offs.X",
                /* 7 */ "Offs.Y",
                /* 8 */ "Rot."
            ];
            for (idx, col_name) in columns.into_iter().enumerate() {
                let cell_text = gtk::CellRendererText::new();
                let col = gtk::TreeViewColumn::builder()
                    .title(col_name)
                    .resizable(true)
                    .clickable(true)
                    .visible(true)
                    .build();
                TreeViewColumnExt::pack_start(&col, &cell_text, true);
                TreeViewColumnExt::add_attribute(&col, &cell_text, "markup", idx as i32);
                tree.append_column(&col);
            }
            tree.set_model(Some(&model));
            model
        },
    };
    let items = data.light_history.borrow();
    if gtk_utils::get_model_row_count(model.upcast_ref()) > items.len() {
        model.clear();
    }
    let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
    let to_index = items.len();
    let make_bad_str = |s: &str| -> String {
        format!(r##"<span color="#FF4040"><b>{}</b></span>"##, s)
    };
    for item in &items[models_row_cnt..to_index] {
        let local_time_str =  {
            let local_time: DateTime<Local> = DateTime::from(item.time);
            local_time.format("%H:%M:%S").to_string()
        };
        let mut fwhm_str = item.stars_fwhm
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(String::new);
        if item.flags.contains(LightFrameShortInfoFlags::BAD_STARS_FWHM) {
            fwhm_str = make_bad_str(&fwhm_str);
        }
        let mut ovality_str = item.stars_ovality
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(String::new);
        if item.flags.contains(LightFrameShortInfoFlags::BAD_STARS_OVAL) {
            ovality_str = make_bad_str(&ovality_str);
        }
        let stars_cnt = item.stars_count as u32;
        let noise_str = format!("{:.3}%", item.noise);
        let bg_str = format!("{:.1}%", item.background);
        let bad_offset = item.flags.contains(LightFrameShortInfoFlags::BAD_OFFSET);
        let mut x_str = item.offset_x
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(||if bad_offset {"???".to_string()} else {"".to_string()});
        let mut y_str = item.offset_y
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(||if bad_offset {"???".to_string()} else {"".to_string()});
        let mut angle_str = item.angle
            .map(|v| format!("{:.1}°", 180.0 * v / PI))
            .unwrap_or_else(||if bad_offset {"???".to_string()} else {"".to_string()});
        if bad_offset {
            x_str = make_bad_str(&x_str);
            y_str = make_bad_str(&y_str);
            angle_str = make_bad_str(&angle_str);
        }
        let last_is_selected =
            gtk_utils::get_list_view_selected_row(&tree).map(|v| v+1) ==
            Some(models_row_cnt as i32);
        let last = model.insert_with_values(None, &[
            (0, &local_time_str),
            (1, &fwhm_str),
            (2, &ovality_str),
            (3, &stars_cnt),
            (4, &noise_str),
            (5, &bg_str),
            (6, &x_str),
            (7, &y_str),
            (8, &angle_str),
        ]);
        if last_is_selected || models_row_cnt == 0 {
            // Select and scroll to last row
            tree.selection().select_iter(&last);
            if let [path] = tree.selection().selected_rows().0.as_slice() {
                tree.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
            }
        }
    }
}

fn handler_action_start_save_raw_frames(data: &Rc<CameraData>) {
    let mut fn_gen = data.fn_gen.lock().unwrap();
    fn_gen.clear();

    read_options_from_widgets(data);
    let options = data.options.borrow();

    let mut adder = data.raw_adder.lock().unwrap();
    adder.clear();

    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        let mount_device_name = data.mount_device_name.borrow();
        state.start_saving_raw_frames(
            options.device.as_deref().unwrap_or(""),
            mount_device_name.as_deref().unwrap_or(""),
            &data.ref_stars,
            &options.frame,
            &options.focuser,
            &options.guiding,
            &options.raw_frames,
            &data.main.thread_timer
        )?;
        gtk_utils::set_active_id(
            &data.main.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}


fn handler_action_continue_save_raw_frames(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.continue_prev_mode()?;
        gtk_utils::set_active_id(
            &data.main.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}

fn handler_action_stop_save_raw_frames(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.abort_active_mode()?;
        Ok(())
    });
}

fn handler_action_clear_light_history(data: &Rc<CameraData>) {
    data.light_history.borrow_mut().clear();
    update_light_history_table(data);
}

fn show_total_raw_time(data: &Rc<CameraData>) {
    let options = data.options.borrow();
    let total_time = options.frame.exposure * options.raw_frames.frame_cnt as f64;
    let text = format!(
        "{:.1}s x {} = {}",
        options.frame.exposure,
        options.raw_frames.frame_cnt,
        seconds_to_total_time_str(total_time, false)
    );
    gtk_utils::set_str(&data.main.builder, "l_raw_time_info", &text);
}

///////////////////////////////////////////////////////////////////////////////

fn init_focuser_widgets(data: &Rc<CameraData>) {
    let spb_foc_temp = data.main.builder.object::<gtk::SpinButton>("spb_foc_temp").unwrap();
    spb_foc_temp.set_range(1.0, 20.0);
    spb_foc_temp.set_digits(0);
    spb_foc_temp.set_increments(1.0, 5.0);

    let spb_foc_measures = data.main.builder.object::<gtk::SpinButton>("spb_foc_measures").unwrap();
    spb_foc_measures.set_range(7.0, 42.0);
    spb_foc_measures.set_digits(0);
    spb_foc_measures.set_increments(1.0, 10.0);

    let spb_foc_auto_step = data.main.builder.object::<gtk::SpinButton>("spb_foc_auto_step").unwrap();
    spb_foc_auto_step.set_range(1.0, 1_000_000.0);
    spb_foc_auto_step.set_digits(0);
    spb_foc_auto_step.set_increments(100.0, 1000.0);

    let spb_foc_exp = data.main.builder.object::<gtk::SpinButton>("spb_foc_exp").unwrap();
    spb_foc_exp.set_range(0.5, 60.0);
    spb_foc_exp.set_digits(1);
    spb_foc_exp.set_increments(0.5, 5.0);
}

fn update_focuser_devices_list(data: &Rc<CameraData>) {
    let dev_list = data.main.indi.get_devices_list();
    let focusers = dev_list
        .iter()
        .filter(|device|
            device.interface.contains(indi_api::DriverInterface::FOCUSER)
        );
    let cb_foc_list: gtk::ComboBoxText =
        data.main.builder.object("cb_foc_list").unwrap();
    let last_active_id = cb_foc_list.active_id().map(|s| s.to_string());
    cb_foc_list.remove_all();
    for camera in focusers {
        cb_foc_list.append(Some(&camera.name), &camera.name);
    }
    let focusers_count = gtk_utils::combobox_items_count(&cb_foc_list);
    if focusers_count == 1 {
        cb_foc_list.set_active(Some(0));
    } else if focusers_count > 1 {
        let options = data.options.borrow();
        if last_active_id.is_some() {
            cb_foc_list.set_active_id(last_active_id.as_deref());
        } else if !options.focuser.device.is_empty() {
            cb_foc_list.set_active_id(Some(options.focuser.device.as_str()));
        }
        if cb_foc_list.active_id().is_none() {
            cb_foc_list.set_active(Some(0));
        }
    }
    let connected = data.main.indi.state() == indi_api::ConnState::Connected;
    gtk_utils::enable_widgets(&data.main.builder, false, &[
        ("cb_foc_list", connected && focusers_count > 1),
    ]);
    data.options.borrow_mut().focuser.device =
        cb_foc_list.active_id().map(|s| s.to_string()).unwrap_or_else(String::new);
    correct_widget_properties(data);
}

fn update_focuser_position_widget(data: &Rc<CameraData>, new_prop: bool) {
    data.excl.exec(|| {
        let Some(foc_device) = gtk_utils::get_active_id(&data.main.builder, "cb_foc_list") else {
            return;
        };
        let Ok(prop_info) = data.main.indi.focuser_get_abs_value_prop_info(&foc_device) else {
            return;
        };
        let spb_foc_val = data.main.builder.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        if new_prop || spb_foc_val.value() == 0.0 {
            spb_foc_val.set_range(0.0, prop_info.max);
            spb_foc_val.set_digits(0);
            let step = prop_info.step.unwrap_or(1.0);
            spb_foc_val.set_increments(step, step * 10.0);
            let Ok(value) = data.main.indi.focuser_get_abs_value(&foc_device) else {
                return;
            };
            spb_foc_val.set_value(value);
        }
    });
    show_cur_focuser_value(data);
}

fn show_cur_focuser_value(data: &Rc<CameraData>) {
    let Some(foc_device) = gtk_utils::get_active_id(&data.main.builder, "cb_foc_list") else {
        return;
    };
    let Ok(value) = data.main.indi.focuser_get_abs_value(&foc_device) else {
        return;
    };
    let l_foc_value = data.main.builder.object::<gtk::Label>("l_foc_value").unwrap();
    l_foc_value.set_label(&format!("{:.0}", value));
}

fn update_focuser_position_after_focusing(data: &Rc<CameraData>, pos: f64) {
    data.excl.exec(|| {
        let spb_foc_val = data.main.builder.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        spb_foc_val.set_value(pos);
    });
}

fn connect_focuser_widgets_events(data: &Rc<CameraData>) {
    let bldr = &data.main.builder;
    let spb_foc_val = bldr.object::<gtk::SpinButton>("spb_foc_val").unwrap();
    spb_foc_val.connect_value_changed(clone!(@strong data => move |sb| {
        data.excl.exec(|| {
            let Some(foc_device) = gtk_utils::get_active_id(&data.main.builder, "cb_foc_list") else {
                return;
            };
            gtk_utils::exec_and_show_error(&data.main.window, || {
                data.main.indi.focuser_set_abs_value(&foc_device, sb.value(), true, None)?;
                Ok(())
            })
        });
    }));

    let chb_foc_temp = bldr.object::<gtk::CheckButton>("chb_foc_temp").unwrap();
    chb_foc_temp.connect_active_notify(clone!(@strong data => move |_| {
        correct_widget_properties(&data);
    }));

    let chb_foc_fwhm = bldr.object::<gtk::CheckButton>("chb_foc_fwhm").unwrap();
    chb_foc_fwhm.connect_active_notify(clone!(@strong data => move |_| {
        correct_widget_properties(&data);
    }));

    let chb_foc_period = bldr.object::<gtk::CheckButton>("chb_foc_period").unwrap();
    chb_foc_period.connect_active_notify(clone!(@strong data => move |_| {
        correct_widget_properties(&data);
    }));

    let da_focusing = data.main.builder.object::<gtk::DrawingArea>("da_focusing").unwrap();
    da_focusing.connect_draw(clone!(@strong data => move |da, ctx| {
        _ = draw_focusing_samples(&data, da, ctx);
        Inhibit(true)
    }));
}

fn draw_focusing_samples(
    data: &Rc<CameraData>,
    da:   &gtk::DrawingArea,
    ctx:  &gdk::cairo::Context
) -> anyhow::Result<()> {
    let Some(focusing_data) = &*data.focusing_data.borrow() else {
        return Ok(());
    };
    const PARABOLA_POINTS: usize = 101;
    let get_plot_points_cnt = |plot_idx: usize| {
        match plot_idx {
            0 => focusing_data.samples.len(),
            1 => if focusing_data.coeffs.is_some() { PARABOLA_POINTS } else { 0 },
            2 => if focusing_data.result.is_some() && focusing_data.coeffs.is_some() { 1 } else { 0 },
            _ => unreachable!(),
        }
    };
    let get_plot_style = |plot_idx| -> PlotLineStyle {
        match plot_idx {
            0 => PlotLineStyle {
                line_width: 2.0,
                line_color: gdk::RGBA::new(0.0, 0.3, 1.0, 1.0),
                point_style: PlotPointStyle::Round(8.0),
            },
            1 => PlotLineStyle {
                line_width: 1.0,
                line_color: gdk::RGBA::new(0.0, 1.0, 0.0, 1.0),
                point_style: PlotPointStyle::None,
            },
            2 => PlotLineStyle {
                line_width: 1.0,
                line_color: gdk::RGBA::new(0.0, 1.0, 0.0, 1.0),
                point_style: PlotPointStyle::Round(10.0),
            },
            _ => unreachable!(),
        }
    };
    let min_pos = focusing_data.samples.iter().map(|s| s.focus_pos).min_by(cmp_f64).unwrap_or(0.0);
    let max_pos = focusing_data.samples.iter().map(|s| s.focus_pos).max_by(cmp_f64).unwrap_or(0.0);
    let get_plot_point = |plot_idx: usize, point_idx: usize| -> (f64, f64) {
        match plot_idx {
            0 => {
                let sample = &focusing_data.samples[point_idx];
                (sample.focus_pos, sample.stars_fwhm as f64)
            }
            1 => {
                if let Some(coeffs) = &focusing_data.coeffs {
                    let x = linear_interpolate(
                        point_idx as f64,
                        0.0,
                        PARABOLA_POINTS as f64,
                        min_pos,
                        max_pos,
                    );
                    let y = coeffs.calc(x);
                    (x, y)
                } else {
                    unreachable!();
                }
            }
            2 => {
                if let (Some(coeffs), Some(x)) = (&focusing_data.coeffs, &focusing_data.result) {
                    let y = coeffs.calc(*x);
                    (*x, y)
                } else {
                    unreachable!();
                }
            }
            _ => unreachable!()
        }
    };
    let mut plots = Plots {
        plot_count: 3,
        get_plot_points_cnt: Box::new(get_plot_points_cnt),
        get_plot_style: Box::new(get_plot_style),
        get_plot_point: Box::new(get_plot_point),
        area: PlotAreaStyle::default(),
        left_axis: AxisStyle::default(),
        bottom_axis: AxisStyle::default(),
    };
    plots.left_axis.dec_digits = 2;
    plots.bottom_axis.dec_digits = 0;
    draw_plots(&plots, da, ctx)?;
    Ok(())
}

fn handler_action_manual_focus(data: &Rc<CameraData>) {
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        let options = data.options.borrow();
        state.start_focusing(
            &options.focuser,
            &options.frame,
            options.device.as_deref().unwrap_or_default()
        )?;
        Ok(())
    });
}

fn handler_action_stop_manual_focus(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.abort_active_mode()?;
        Ok(())
    });
}

///////////////////////////////////////////////////////////////////////////////

fn init_dithering_widgets(data: &Rc<CameraData>) {
    let spb_guid_max_err = data.main.builder.object::<gtk::SpinButton>("spb_guid_max_err").unwrap();
    spb_guid_max_err.set_range(3.0, 50.0);
    spb_guid_max_err.set_digits(0);
    spb_guid_max_err.set_increments(1.0, 10.0);

    let spb_mnt_cal_exp = data.main.builder.object::<gtk::SpinButton>("spb_mnt_cal_exp").unwrap();
    spb_mnt_cal_exp.set_range(0.5, 10.0);
    spb_mnt_cal_exp.set_digits(1);
    spb_mnt_cal_exp.set_increments(0.5, 5.0);
}

fn connect_dithering_widgets_events(data: &Rc<CameraData>) {
    let chb_guid_enabled = data.main.builder.object::<gtk::CheckButton>("chb_guid_enabled").unwrap();
    chb_guid_enabled.connect_active_notify(clone!(@strong data => move |_| {
        correct_widget_properties(&data);
    }));
}

fn handler_action_start_dither_calibr(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let options = data.options.borrow();
        let mut state = data.main.state.write().unwrap();
        let cam_device = options.device.as_deref().unwrap_or_default();
        let mount_device = data.mount_device_name.borrow();
        let mount_device = mount_device.as_deref().unwrap_or_default();
        state.start_mount_calibr(
            &options.frame,
            &options.guiding,
            mount_device,
            cam_device
        )?;
        Ok(())
    });
}

fn handler_action_stop_dither_calibr(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.main.state.write().unwrap();
        state.abort_active_mode()?;
        Ok(())
    });
}

///////////////////////////////////////////////////////////////////////////////

const MOUNT_NAV_BUTTON_NAMES: &[&'static str] = &[
    "btn_left_top",    "btn_top",        "btn_right_top",
    "btn_left",        "btn_stop_mount", "btn_right",
    "btn_left_bottom", "btn_bottom",     "btn_right_bottom",
];

fn connect_mount_widgets_events(data: &Rc<CameraData>) {
    for &btn_name in MOUNT_NAV_BUTTON_NAMES {
        let btn = data.main.builder.object::<gtk::Button>(btn_name).unwrap();
        btn.connect_button_press_event(clone!(@strong data => move |_, eb| {
            if eb.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                handler_nav_mount_btn_pressed(&data, btn_name);
            }
            Inhibit(false)
        }));
        btn.connect_button_release_event(clone!(@strong data => move |_, _| {
            handler_nav_mount_btn_released(&data, btn_name);
            Inhibit(false)
        }));
    }

    let chb_tracking = data.main.builder.object::<gtk::CheckButton>("chb_tracking").unwrap();
    chb_tracking.connect_active_notify(clone!(@strong data => move |chb| {
        data.excl.exec(|| {
            let Some(mount_device_name) = &*data.mount_device_name.borrow() else {
                return;
            };
            gtk_utils::exec_and_show_error(&data.main.window, || {
                data.main.indi.mount_set_tracking(mount_device_name, chb.is_active(), true, None)?;
                Ok(())
            });
        });
    }));

    let chb_parked = data.main.builder.object::<gtk::CheckButton>("chb_parked").unwrap();
    chb_parked.connect_active_notify(clone!(@strong data => move |chb| {
        let Some(mount_device_name) = &*data.mount_device_name.borrow() else {
            return;
        };
        let parked = chb.is_active();
        gtk_utils::enable_widgets(&data.main.builder, true, &[
            ("chb_tracking", !parked),
            ("cb_mnt_speed", !parked),
            ("chb_inv_ns", !parked),
            ("chb_inv_we", !parked),
        ]);
        for &btn_name in MOUNT_NAV_BUTTON_NAMES {
            gtk_utils::set_bool_prop(&data.main.builder, btn_name, "sensitive", !parked);
        }
        data.excl.exec(|| {
            gtk_utils::exec_and_show_error(&data.main.window, || {
                data.main.indi.mount_set_parked(mount_device_name, parked, true, None)?;
                Ok(())
            });
        });
    }));
}

fn handler_nav_mount_btn_pressed(data: &Rc<CameraData>, button_name: &str) {
    let Some(mount_device_name) = &*data.mount_device_name.borrow() else {
        return;
    };
    gtk_utils::exec_and_show_error(&data.main.window, || {
        if button_name != "btn_stop_mount" {
            let inv_ns = gtk_utils::get_bool(&data.main.builder, "chb_inv_ns");
            let inv_we = gtk_utils::get_bool(&data.main.builder, "chb_inv_we");
            data.main.indi.mount_reverse_motion(
                mount_device_name,
                inv_ns,
                inv_we,
                false,
                SET_PROP_TIMEOUT
            )?;
            let speed = gtk_utils::get_active_id(&data.main.builder, "cb_mnt_speed");
            if let Some(speed) = speed {
                data.main.indi.mount_set_slew_speed(
                    mount_device_name,
                    &speed,
                    false, Some(100)
                )?
            }
        }
        match button_name {
            "btn_left_top" => {
                data.main.indi.mount_start_move_west(mount_device_name)?;
                data.main.indi.mount_start_move_north(mount_device_name)?;
            }
            "btn_top" => {
                data.main.indi.mount_start_move_north(mount_device_name)?;
            }
            "btn_right_top" => {
                data.main.indi.mount_start_move_east(mount_device_name)?;
                data.main.indi.mount_start_move_north(mount_device_name)?;
            }
            "btn_left" => {
                data.main.indi.mount_start_move_west(mount_device_name)?;
            }
            "btn_right" => {
                data.main.indi.mount_start_move_east(mount_device_name)?;
            }
            "btn_left_bottom" => {
                data.main.indi.mount_start_move_west(mount_device_name)?;
                data.main.indi.mount_start_move_south(mount_device_name)?;
            }
            "btn_bottom" => {
                data.main.indi.mount_start_move_south(mount_device_name)?;
            }
            "btn_right_bottom" => {
                data.main.indi.mount_start_move_south(mount_device_name)?;
                data.main.indi.mount_start_move_east(mount_device_name)?;
            }
            "btn_stop_mount" => {
                data.main.indi.mount_abort_motion(mount_device_name)?;
                data.main.indi.mount_stop_move(mount_device_name)?;
            }
            _ => {},
        };
        Ok(())
    });
}

fn handler_nav_mount_btn_released(data: &Rc<CameraData>, button_name: &str) {
    let Some(mount_device_name) = &*data.mount_device_name.borrow() else {
        return;
    };
    gtk_utils::exec_and_show_error(&data.main.window, || {
        if button_name != "btn_stop_mount" {
            data.main.indi.mount_stop_move(mount_device_name)?;
        }
        Ok(())
    });
}

fn update_mount_widgets(data: &Rc<CameraData>) {
    let mount_active = data.mount_device_name.borrow()
        .as_ref()
        .map(|device_name| {
            data.main.indi.is_device_enabled(&device_name).unwrap_or(false)
        })
        .unwrap_or(false);
    gtk_utils::enable_widgets(
        &data.main.builder,
        false,
        &[("bx_simple_mount", mount_active)]
    );
}

fn fill_mount_speed_list_widget(data: &Rc<CameraData>) {
    let Some(mount_device_name) = &*data.mount_device_name.borrow() else {
        return;
    };
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let list = data.main.indi.mount_get_slew_speed_list(mount_device_name)?;
        let cb_mnt_speed = data.main.builder.object::<gtk::ComboBoxText>("cb_mnt_speed").unwrap();
        cb_mnt_speed.remove_all();
        cb_mnt_speed.append(None, "---");
        for (id, text) in list {
            cb_mnt_speed.append(Some(&id), &text);
        }
        let options = data.options.borrow();
        if options.mount.speed.is_some() {
            cb_mnt_speed.set_active_id(options.mount.speed.as_deref());
        } else {
            cb_mnt_speed.set_active(Some(0));
        }
        Ok(())
    });
}

fn show_mount_tracking_state(data: &Rc<CameraData>, tracking: bool) {
    if data.mount_device_name.borrow().is_none() {
        return;
    };
    data.excl.exec(|| {
        gtk_utils::set_bool_prop(&data.main.builder, "chb_tracking", "active", tracking);
    });
}

fn show_mount_parked_state(data: &Rc<CameraData>, parked: bool) {
    if data.mount_device_name.borrow().is_none() {
        return;
    };
    data.excl.exec(|| {
        gtk_utils::set_bool_prop(&data.main.builder, "chb_parked", "active", parked);
    });
}
