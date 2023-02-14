use std::{
    rc::Rc,
    sync::{*, atomic::AtomicI64},
    cell::RefCell,
    path::PathBuf,
    f32::consts::PI,
    thread::JoinHandle,
};
use bitflags::bitflags;
use gtk::{prelude::*, glib, glib::clone, cairo};
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
    image_raw::RawAdder,
};

const SET_PROP_TIMEOUT: Option<u64> = Some(2000);

bitflags! { struct DelayedFlags: u32 {
    const UPDATE_CAM_LIST        = 1;
    const START_LIVE_VIEW        = 2;
    const START_COOLING          = 4;
    const UPDATE_CTRL_WIDGETS    = 8;
    const UPDATE_RESOLUTION_LIST = 16;
    const SELECT_MAX_RESOLUTION  = 32;
}}

struct DelayedAction {
    countdown:  u8,
    flags:      DelayedFlags,
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

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq)]
enum FrameType {
    #[default]
    Lights,
    Flats,
    Darks,
}

impl FrameType {
    fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("flat") => Self::Flats,
            Some("dark") => Self::Darks,
            _            => Self::Lights,
        }
    }

    fn to_active_id(&self) -> Option<&'static str> {
        match self {
            FrameType::Lights => Some("light"),
            FrameType::Flats => Some("flat"),
            FrameType::Darks => Some("dark"),
        }
    }

    fn to_indi_frame_type(&self) -> indi_api::FrameType {
        match self {
            FrameType::Lights => indi_api::FrameType::Light,
            FrameType::Flats => indi_api::FrameType::Flat,
            FrameType::Darks => indi_api::FrameType::Dark,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone, PartialEq)]
enum Binning {
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

    fn get_ratio(&self) -> usize {
        match self {
            Self::Orig => 1,
            Self::Bin2 => 2,
            Self::Bin3 => 3,
            Self::Bin4 => 4,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Copy, Clone)]
enum Crop {
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

    fn translate(&self, value: usize) -> usize {
        match self {
            Crop::None => value,
            Crop::P75 => 3 * value / 4,
            Crop::P50 => value / 2,
            Crop::P33 => value / 3,
            Crop::P25 => value / 4,
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
struct FrameOptions {
    exposure:   f64,
    gain:       f64,
    offset:     u32,
    frame_type: FrameType,
    binning:    Binning,
    crop:       Crop,
    low_noise:  bool,
    delay:      f64,
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
    fn create_master_dark_file_name(&self) -> String {
        format!("{:.1}s_g{:.0}_ofs{}", self.exposure, self.gain, self.offset)
    }

    fn create_master_flat_file_name(&self) -> String {
        String::new()
    }

    fn have_to_use_delay(&self) -> bool {
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
}

impl Default for CalibrOptions {
    fn default() -> Self {
        Self {
            dark_frame_en: true,
            dark_frame: None,
            flat_frame_en: true,
            flat_frame: None,
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
        CalibrParams { dark, flat }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
struct RawFrameOptions {
    out_path:      PathBuf,
    frame_cnt:     usize,
    use_cnt:       bool,
    create_master: bool,
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

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct LiveStackingOptions {
    save_orig:       bool,
    save_minutes:    usize,
    save_enabled:    bool,
    use_max_fwhm:    bool,
    max_fwhm:        f32,
    use_max_ovality: bool,
    max_ovality:     f32,
    use_min_stars:   bool,
    min_stars:       usize,
    out_dir:         PathBuf,
}

impl Default for LiveStackingOptions {
    fn default() -> Self {
        Self {
            save_orig:       false,
            save_minutes:    5,
            save_enabled:    true,
            use_max_fwhm:    true,
            max_fwhm:        20.0,
            use_max_ovality: true,
            max_ovality:     0.2,
            use_min_stars:   false,
            min_stars:       1000,
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
struct CameraOptions {
    camera_name:    Option<String>,
    live_view:      bool,
    ctrl:           CamCtrlOptions,
    frame:          FrameOptions,
    calibr:         CalibrOptions,
    raw_frames:     RawFrameOptions,
    live:           LiveStackingOptions,
    preview:        PreviewOptions,
    paned_pos1:     i32,
    paned_pos2:     i32,
    paned_pos3:     i32,
    hist_log_x:     bool,
    hist_log_y:     bool,
    cam_ctrl_exp:   bool,
    shot_exp:       bool,
    calibr_exp:     bool,
    raw_frames_exp: bool,
    live_exp:       bool,
}

impl Default for CameraOptions {
    fn default() -> Self {
        Self {
            camera_name:    None,
            live_view:      false,
            preview:        PreviewOptions::default(),
            ctrl:           CamCtrlOptions::default(),
            frame:          FrameOptions::default(),
            calibr:         CalibrOptions::default(),
            raw_frames:     RawFrameOptions::default(),
            live:           LiveStackingOptions::default(),
            paned_pos1:     -1,
            paned_pos2:     -1,
            paned_pos3:     -1,
            hist_log_x:     false,
            hist_log_y:     true,
            cam_ctrl_exp:   true,
            shot_exp:       true,
            calibr_exp:     false,
            raw_frames_exp: false,
            live_exp:       false,
        }
    }
}

enum MainThreadCommands {
    ShowFrameProcessingResult(FrameProcessingResult),
    Exit,
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    SingleShot,
    LiveView,
    LiveStacking,
    SavingRawFrames,
}


#[derive(Clone, Debug)]
struct FramesCounter {
    to_go: usize,
    total: usize,
}

enum State {
    Waiting,
    Active{
        mode:         Mode,
        camera:       String,
        frame:        FrameOptions,
        counter:      Option<FramesCounter>,
        thread_timer: Arc<ThreadTimer>,
    },
}

#[derive(Debug)]
struct SavingRawPause {
    camera:  String,
    frame:   FrameOptions,
    counter: FramesCounter,
}

struct CameraData {
    main:               Rc<MainData>,
    delayed_action:     RefCell<DelayedAction>,
    options:            RefCell<CameraOptions>,
    process_thread:     JoinHandle<()>,
    img_cmds_sender:    mpsc::Sender<Command>,
    main_thread_sender: glib::Sender<MainThreadCommands>,
    conn_state:         RefCell<indi_api::ConnState>,
    indi_conn:          RefCell<Option<indi_api::Subscription>>,
    state:              Arc<RwLock<State>>,
    fn_gen:             Arc<Mutex<SeqFileNameGen>>,
    live_staking:       Arc<LiveStackingData>,
    preview_scroll_pos: RefCell<Option<((f64, f64), (f64, f64))>>,
    last_exposure:      Arc<AtomicI64>, // to show exposure progress
    light_history:      RefCell<Vec<LightFileShortInfo>>,
    raw_adder:          Arc<Mutex<Option<RawAdder>>>,
    save_raw_pause:     RefCell<Option<SavingRawPause>>,
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
    let (main_thread_sender, main_thread_receiver) =
        glib::MainContext::channel(glib::PRIORITY_DEFAULT);
    let delayed_action = DelayedAction{
        countdown: 0,
        flags:     DelayedFlags::empty(),
    };

    let (img_cmds_sender, process_thread) = start_process_blob_thread();
    let camera_data = Rc::new(CameraData {
        main:               Rc::clone(data),
        delayed_action:     RefCell::new(delayed_action),
        options:            RefCell::new(options),
        process_thread,
        img_cmds_sender,
        main_thread_sender,
        conn_state:         RefCell::new(indi_api::ConnState::Disconnected),
        indi_conn:          RefCell::new(None),
        state:              Arc::new(RwLock::new(State::Waiting)),
        fn_gen:             Arc::new(Mutex::new(SeqFileNameGen::new())),
        live_staking:       Arc::new(LiveStackingData::new()),
        preview_scroll_pos: RefCell::new(None),
        last_exposure:      Arc::new(AtomicI64::new(0)),
        light_history:      RefCell::new(Vec::new()),
        raw_adder:          Arc::new(Mutex::new(None)),
        save_raw_pause:     RefCell::new(None),
    });
    main_thread_receiver.attach(None, clone! (@strong camera_data => move |cmd| {
        if matches!(cmd, MainThreadCommands::Exit) {  }
        match cmd {
            MainThreadCommands::ShowFrameProcessingResult(result) =>
                show_frame_processing_result(&camera_data, result),
            MainThreadCommands::Exit =>
                return Continue(false),
        }
        Continue(true)
    }));
    show_options(&camera_data);
    show_total_raw_time(&camera_data);
    update_light_history_table(&camera_data);
    connect_indi_events(&camera_data);
    correct_ctrl_widgets_properties(&camera_data);
    connect_widgets_events(&camera_data);
    connect_img_mouse_scroll_events(&camera_data);

    let weak_camera_data = Rc::downgrade(&camera_data);
    timer_handlers.push(Box::new(move || {
        let Some(data) = weak_camera_data.upgrade() else { return; };
        handler_timer(&data);
    }));

    data.window.connect_delete_event(clone!(@weak camera_data => @default-panic, move |_, _| {
        let res = handler_close_window(&camera_data);
        if res == Inhibit(false) {
            camera_data.main_thread_sender.send(MainThreadCommands::Exit).unwrap();
        }
        res
    }));
}

fn connect_widgets_events(data: &Rc<CameraData>) {
    let bldr = &data.main.builder;
    gtk_utils::connect_action(&data.main.window, data, "take_shot",             handler_action_take_shot);
    gtk_utils::connect_action(&data.main.window, data, "stop_shot",             handler_action_stop_shot);
    gtk_utils::connect_action(&data.main.window, data, "start_live_stacking",   handler_action_start_live_stacking);
    gtk_utils::connect_action(&data.main.window, data, "stop_live_stacking",    handler_action_stop_live_stacking);
    gtk_utils::connect_action(&data.main.window, data, "clear_light_history",   handler_action_clear_light_history);
    gtk_utils::connect_action(&data.main.window, data, "start_save_raw_frames", handler_action_start_save_raw_frames);
    gtk_utils::connect_action(&data.main.window, data, "stop_save_raw_frames",  handler_action_stop_save_raw_frames);
    gtk_utils::connect_action(&data.main.window, data, "pause_save_raw_frames", handler_action_pause_save_raw_frames);

    let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
    cb_frame_mode.connect_active_id_notify(clone!(@strong data => move |cb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.frame.frame_type = FrameType::from_active_id(
            cb.active_id().map(|v| v.to_string()).as_deref()
        );
        drop(options);
        correct_ctrl_widgets_properties(&data);
    }));

    let chb_cooler = bldr.object::<gtk::CheckButton>("chb_cooler").unwrap();
    chb_cooler.connect_active_notify(clone!(@strong data => move |chb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.ctrl.enable_cooler = chb.is_active();
        drop(options);
        set_temperature_by_options(&data, false);
        correct_ctrl_widgets_properties(&data);
    }));

    let chb_heater = bldr.object::<gtk::CheckButton>("chb_heater").unwrap();
    chb_heater.connect_active_notify(clone!(@strong data => move |chb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.ctrl.enable_heater = chb.is_active();
        drop(options);
        set_temperature_by_options(&data, false);
        correct_ctrl_widgets_properties(&data);
    }));

    let chb_fan = bldr.object::<gtk::CheckButton>("chb_fan").unwrap();
    chb_fan.connect_active_notify(clone!(@strong data => move |chb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.ctrl.enable_fan = chb.is_active();
        drop(options);
        set_temperature_by_options(&data, false);
        correct_ctrl_widgets_properties(&data);
    }));

    let spb_temp = bldr.object::<gtk::SpinButton>("spb_temp").unwrap();
    spb_temp.connect_value_changed(clone!(@strong data => move |spb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.ctrl.temperature = spb.value();
        drop(options);
        set_temperature_by_options(&data, false);
    }));

    let chb_shots_cont = bldr.object::<gtk::CheckButton>("chb_shots_cont").unwrap();
    chb_shots_cont.connect_active_notify(clone!(@strong data => move |_| {
        read_options_from_widgets(&data);
        correct_ctrl_widgets_properties(&data);
        handler_live_view_changed(&data);
    }));

    let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
    cb_frame_mode.connect_active_id_notify(clone!(@strong data => move |cb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.frame_type = FrameType::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            )
        }
    }));

    let spb_exp = bldr.object::<gtk::SpinButton>("spb_exp").unwrap();
    spb_exp.connect_value_changed(clone!(@strong data => move |sb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.exposure = sb.value();
        }
        if let Ok(mut options) = data.options.try_borrow_mut() {
            options.frame.exposure = sb.value();
        }
        show_total_raw_time(&data);
    }));

    let spb_gain = bldr.object::<gtk::SpinButton>("spb_gain").unwrap();
    spb_gain.connect_value_changed(clone!(@strong data => move |sb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.gain = sb.value();
        }
    }));

    let spb_offset = bldr.object::<gtk::SpinButton>("spb_offset").unwrap();
    spb_offset.connect_value_changed(clone!(@strong data => move |sb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.offset = sb.value() as u32;
        }
    }));

    let cb_bin = bldr.object::<gtk::ComboBoxText>("cb_bin").unwrap();
    cb_bin.connect_active_id_notify(clone!(@strong data => move |cb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.binning = Binning::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            );
        }
    }));

    let cb_crop = bldr.object::<gtk::ComboBoxText>("cb_crop").unwrap();
    cb_crop.connect_active_id_notify(clone!(@strong data => move |cb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.crop = Crop::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            );
        }
    }));

    let spb_delay = bldr.object::<gtk::SpinButton>("spb_delay").unwrap();
    spb_delay.connect_value_changed(clone!(@strong data => move |sb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.delay = sb.value();
        }
    }));

    let chb_low_noise = bldr.object::<gtk::CheckButton>("chb_low_noise").unwrap();
    chb_low_noise.connect_active_notify(clone!(@strong data => move |chb| {
        let mut state = data.state.write().unwrap();
        if let State::Active{ frame, .. } = &mut *state {
            frame.low_noise = chb.is_active();
        }
    }));

    let spb_raw_frames_cnt = bldr.object::<gtk::SpinButton>("spb_raw_frames_cnt").unwrap();
    spb_raw_frames_cnt.connect_value_changed(clone!(@strong data => move |sb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.raw_frames.frame_cnt = sb.value() as usize;
        drop(options);
        show_total_raw_time(&data);
    }));

    let da_shot_state = bldr.object::<gtk::DrawingArea>("da_shot_state").unwrap();
    da_shot_state.connect_draw(clone!(@strong data => move |area, cr| {
        handler_draw_shot_state(&data, area, cr);
        Inhibit(false)
    }));

    let cb_preview_src = bldr.object::<gtk::ComboBoxText>("cb_preview_src").unwrap();
    cb_preview_src.connect_active_id_notify(clone!(@strong data => move |cb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.preview.source = PreviewSource::from_active_id(
            cb.active_id().map(|id| id.to_string()).as_deref()
        );
        drop(options);
        show_preview_image(&data, None, None);
        repaint_histogram(&data);
        show_histogram_stat(&data);
        show_image_info(&data);
    }));

    let cb_preview_scale = bldr.object::<gtk::ComboBoxText>("cb_preview_scale").unwrap();
    cb_preview_scale.connect_active_id_notify(clone!(@strong data => move |cb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.preview.scale =
            ImgPreviewScale::from_active_id(
                cb.active_id().map(|id| id.to_string()).as_deref()
            ).unwrap_or(ImgPreviewScale::FitWindow);
        drop(options);
        show_preview_image(&data, None, None);
    }));

    let chb_auto_black = bldr.object::<gtk::CheckButton>("chb_auto_black").unwrap();
    chb_auto_black.connect_active_notify(clone!(@strong data => move |chb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.preview.auto_black = chb.is_active();
        drop(options);
        show_preview_image(&data, None, None);
    }));

    let scl_gamma = bldr.object::<gtk::Scale>("scl_gamma").unwrap();
    scl_gamma.connect_value_changed(clone!(@strong data => move |scl| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        let new_value = (scl.value() * 10.0).round() / 10.0;
        if options.preview.gamma == new_value { return; }
        options.preview.gamma = new_value;
        drop(options);
        show_preview_image(&data, None, None);
    }));

    let da_histogram = bldr.object::<gtk::DrawingArea>("da_histogram").unwrap();
    da_histogram.connect_draw(clone!(@strong data => move |area, cr| {
        handler_draw_histogram(&data, area, cr);
        Inhibit(false)
    }));

    let ch_hist_logx = bldr.object::<gtk::CheckButton>("ch_hist_logx").unwrap();
    ch_hist_logx.connect_active_notify(clone!(@strong data => move |chb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.hist_log_x = chb.is_active();
        drop(options);
        repaint_histogram(&data)
    }));

    let ch_hist_logy = bldr.object::<gtk::CheckButton>("ch_hist_logy").unwrap();
    ch_hist_logy.connect_active_notify(clone!(@strong data => move |chb| {
        let Ok(mut options) = data.options.try_borrow_mut() else { return; };
        options.hist_log_y = chb.is_active();
        drop(options);
        repaint_histogram(&data)
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

fn abort_current_shot(data: &Rc<CameraData>, state: &State) -> bool {
    let result = match state {
        State::Active { camera, .. } => {
            _ = data.main.indi.camera_abort_exposure(camera);
            true
        },
        _ => {
            false
        },
    };
    result
}

fn handler_close_window(data: &Rc<CameraData>) -> gtk::Inhibit {
    abort_current_shot(data, &data.state.read().unwrap());
    read_options_from_widgets(data);

    let options = data.options.borrow();
    _ = save_json_to_config::<CameraOptions>(&options, "conf_camera");

    if let Some(indi_conn) = data.indi_conn.borrow_mut().take() {
        data.main.indi.unsubscribe(indi_conn);
    }

    gtk::Inhibit(false)
}

fn connect_indi_events(data: &Rc<CameraData>) {
    let (sender, receiver) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
    let state = Arc::clone(&data.state);
    let indi_clone = Arc::clone(&data.main.indi);
    let last_exposure = Arc::clone(&data.last_exposure);
    *data.indi_conn.borrow_mut() = Some(data.main.indi.subscribe_events(move |event| {
        match event {
            indi_api::Event::BlobStart(blob_start) =>
                process_blob_start_event(
                    &state,
                    &indi_clone,
                    &blob_start,
                    &last_exposure
                ),
            _ =>
                sender.send(event).unwrap(),
        }
    }));
    let data = Rc::downgrade(data);
    receiver.attach(None, move |item| {
        let Some(data) = data.upgrade() else { return Continue(false); };
        match item {
            indi_api::Event::ConnChange(conn_state) =>
                process_conn_state_event(&data, conn_state),
            indi_api::Event::PropChange(event_data) => {
                match &event_data.change {
                    indi_api::PropChange::New(value) =>
                        process_prop_change_event(
                            &data,
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            true,
                            &value.prop_value
                        ),
                    indi_api::PropChange::Change(value) =>
                        process_prop_change_event(
                            &data,
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            false,
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
            _ =>
                {},
        }
        Continue(true)
    });
}

fn show_options(data: &Rc<CameraData>) {
    let options = data.options.borrow();
    let bld = &data.main.builder;
    let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
    let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
    let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();

    pan_cam1.set_position(options.paned_pos1);
    if options.paned_pos2 != -1 {
        pan_cam2.set_position(pan_cam2.allocation().width()-options.paned_pos2);
    }
    pan_cam3.set_position(options.paned_pos3);

    gtk_utils::set_bool     (bld, "chb_shots_cont",      options.live_view);

    gtk_utils::set_bool     (bld, "chb_cooler",          options.ctrl.enable_cooler);
    gtk_utils::set_f64      (bld, "spb_temp",            options.ctrl.temperature);
    gtk_utils::set_bool     (bld, "chb_heater",          options.ctrl.enable_heater);
    gtk_utils::set_bool     (bld, "chb_fan",             options.ctrl.enable_fan);

    gtk_utils::set_active_id(bld, "cb_frame_mode",       options.frame.frame_type.to_active_id());
    gtk_utils::set_f64      (bld, "spb_exp",             options.frame.exposure);
    gtk_utils::set_f64      (bld, "spb_delay",           options.frame.delay);
    gtk_utils::set_f64      (bld, "spb_gain",            options.frame.gain);
    gtk_utils::set_f64      (bld, "spb_offset",          options.frame.offset as f64);
    gtk_utils::set_active_id(bld, "cb_bin",              options.frame.binning.to_active_id());
    gtk_utils::set_active_id(bld, "cb_crop",             options.frame.crop.to_active_id());
    gtk_utils::set_bool     (bld, "chb_low_noise",       options.frame.low_noise);

    gtk_utils::set_bool     (bld, "chb_master_dark",     options.calibr.dark_frame_en);
    gtk_utils::set_path     (bld, "fch_master_dark",     options.calibr.dark_frame.as_deref());
    gtk_utils::set_bool     (bld, "chb_master_flat",     options.calibr.flat_frame_en);
    gtk_utils::set_path     (bld, "fch_master_flat",     options.calibr.flat_frame.as_deref());

    gtk_utils::set_bool     (bld, "chb_raw_frames_cnt",  options.raw_frames.use_cnt);
    gtk_utils::set_f64      (bld, "spb_raw_frames_cnt",  options.raw_frames.frame_cnt as f64);
    gtk_utils::set_path     (bld, "fcb_raw_frames_path", Some(&options.raw_frames.out_path));
    gtk_utils::set_bool     (bld, "chb_master_frame",    options.raw_frames.create_master);

    gtk_utils::set_bool     (bld, "chb_live_save_orig",  options.live.save_orig);
    gtk_utils::set_bool     (bld, "chb_live_save",       options.live.save_enabled);
    gtk_utils::set_f64      (bld, "spb_live_minutes",    options.live.save_minutes as f64);
    gtk_utils::set_bool     (bld, "chb_max_fwhm",        options.live.use_max_fwhm);
    gtk_utils::set_f64      (bld, "spb_max_fwhm",        options.live.max_fwhm as f64);
    gtk_utils::set_bool     (bld, "chb_max_oval",        options.live.use_max_ovality);
    gtk_utils::set_f64      (bld, "spb_max_oval",        options.live.max_ovality as f64);
    gtk_utils::set_bool     (bld, "chb_min_stars",       options.live.use_min_stars);
    gtk_utils::set_f64      (bld, "spb_min_stars",       options.live.min_stars as f64);
    gtk_utils::set_path     (bld, "fch_live_folder",     Some(&options.live.out_dir));

    gtk_utils::set_active_id(bld, "cb_preview_src",      options.preview.source.to_active_id());
    gtk_utils::set_active_id(bld, "cb_preview_scale",    options.preview.scale.to_active_id());
    gtk_utils::set_bool     (bld, "chb_auto_black",      options.preview.auto_black);
    gtk_utils::set_f64      (bld, "scl_gamma",           options.preview.gamma);
    gtk_utils::set_bool     (bld, "ch_hist_logx",        options.hist_log_x);
    gtk_utils::set_bool     (bld, "ch_hist_logy",        options.hist_log_y);
    gtk_utils::set_bool     (bld, "exp_cam_ctrl",        options.cam_ctrl_exp);
    gtk_utils::set_bool     (bld, "exp_shot_set",        options.shot_exp);
    gtk_utils::set_bool     (bld, "exp_calibr",          options.calibr_exp);
    gtk_utils::set_bool     (bld, "exp_raw_frames",      options.raw_frames_exp);
    gtk_utils::set_bool     (bld, "exp_live",            options.live_exp);
}

fn read_options_from_widgets(data: &Rc<CameraData>) {
    let mut options = data.options.borrow_mut();
    let bld = &data.main.builder;
    let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
    let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
    let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();

    options.paned_pos1 = pan_cam1.position();
    options.paned_pos2 = pan_cam2.allocation().width()-pan_cam2.position();
    options.paned_pos3 = pan_cam3.position();

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

    options.camera_name          = gtk_utils::get_active_id(bld, "cb_camera_list");
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

    options.raw_frames.use_cnt       = gtk_utils::get_bool     (bld, "chb_raw_frames_cnt");
    options.raw_frames.frame_cnt     = gtk_utils::get_f64      (bld, "spb_raw_frames_cnt") as usize;
    options.raw_frames.out_path      = gtk_utils::get_pathbuf  (bld, "fcb_raw_frames_path").unwrap_or_default();
    options.raw_frames.create_master = gtk_utils::get_bool     (bld, "chb_master_frame");

    options.live.save_orig       = gtk_utils::get_bool     (bld, "chb_live_save_orig");
    options.live.save_enabled    = gtk_utils::get_bool     (bld, "chb_live_save");
    options.live.save_minutes    = gtk_utils::get_f64      (bld, "spb_live_minutes") as usize;
    options.live.use_max_fwhm    = gtk_utils::get_bool     (bld, "chb_max_fwhm");
    options.live.max_fwhm        = gtk_utils::get_f64      (bld, "spb_max_fwhm") as f32;
    options.live.use_max_ovality = gtk_utils::get_bool     (bld, "chb_max_oval");
    options.live.max_ovality     = gtk_utils::get_f64      (bld, "spb_max_oval") as f32;
    options.live.use_min_stars   = gtk_utils::get_bool     (bld, "chb_min_stars");
    options.live.min_stars       = gtk_utils::get_f64      (bld, "spb_min_stars") as usize;
    options.live.out_dir         = gtk_utils::get_pathbuf  (bld, "fch_live_folder").unwrap_or_default();
    options.preview.auto_black   = gtk_utils::get_bool     (bld, "chb_auto_black");
    options.preview.gamma        = (gtk_utils::get_f64     (bld, "scl_gamma") * 10.0).round() / 10.0;
    options.hist_log_x           = gtk_utils::get_bool     (bld, "ch_hist_logx");
    options.hist_log_y           = gtk_utils::get_bool     (bld, "ch_hist_logy");
    options.cam_ctrl_exp         = gtk_utils::get_bool     (bld, "exp_cam_ctrl");
    options.shot_exp             = gtk_utils::get_bool     (bld, "exp_shot_set");
    options.calibr_exp           = gtk_utils::get_bool     (bld, "exp_calibr");
    options.raw_frames_exp       = gtk_utils::get_bool     (bld, "exp_raw_frames");
    options.live_exp             = gtk_utils::get_bool     (bld, "exp_live");
}

fn handler_timer(data: &Rc<CameraData>) {
    let mut delayed_action = data.delayed_action.borrow_mut();
    if delayed_action.countdown != 0 {
        delayed_action.countdown -= 1;
        if delayed_action.countdown == 0 {
            let update_cam_list_flag =
                delayed_action.flags.contains(DelayedFlags::UPDATE_CAM_LIST);
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
            delayed_action.flags.bits = 0;
            if update_cam_list_flag {
                update_camera_list(data);
                correct_ctrl_widgets_properties(data);
            }
            if start_live_view_flag
            && data.options.borrow().live_view {
                start_taking_shots(data, Mode::LiveView, None);
                correct_ctrl_widgets_properties(data);
            }
            if start_cooling {
                set_temperature_by_options(data, true);
            }
            if update_ctrl_widgets {
                correct_ctrl_widgets_properties(data);
            }
            if update_res {
                update_resolution_list(data);
            }
            if sel_max_res {
                select_maximum_resolution(data);
            }
        }
    }
}

fn correct_ctrl_widgets_properties(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let bldr = &data.main.builder;
        let cb_camera_list = bldr.object::<gtk::ComboBoxText>("cb_camera_list").unwrap();
        let cam_count = gtk_utils::combobox_items_count(&cb_camera_list);
        let tab_active =
            matches!(*data.conn_state.borrow(), indi_api::ConnState::Connected)
            && cam_count != 0;
        gtk_utils::enable_widgets(bldr, &[
            ("bx_cam_ctrl", tab_active),
            ("l_camera",    tab_active)
        ]);
        if !tab_active { return Ok(()); }
        let camera = gtk_utils::get_active_id(bldr, "cb_camera_list").unwrap();

        cb_camera_list.set_sensitive(cam_count >= 2);

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

        let temp_supported = correct_num_adjustment_by_prop(
            data.main.indi.camera_get_temperature_prop_info(&camera),
            "adj_temp"
        );
        let exposure_supported = correct_num_adjustment_by_prop(
            data.main.indi.camera_get_exposure_prop_info(&camera),
            "adj_exp"
        );
        let gain_supported = correct_num_adjustment_by_prop(
            data.main.indi.camera_get_gain_prop_info(&camera),
            "adj_gain"
        );
        let offset_supported = correct_num_adjustment_by_prop(
            data.main.indi.camera_get_offset_prop_info(&camera),
            "adj_offset"
        );
        let bin_supported = data.main.indi.camera_is_binning_supported(&camera)?;
        let fan_supported = data.main.indi.camera_is_fan_supported(&camera)?;
        let heater_supported = data.main.indi.camera_is_heater_supported(&camera)?;
        let low_noise_supported = data.main.indi.camera_is_low_noise_ctrl_supported(&camera)?;

        let cooler_active = gtk_utils::get_bool(bldr, "chb_cooler");

        let frame_mode_str = gtk_utils::get_active_id(bldr, "cb_frame_mode");
        let frame_mode = FrameType::from_active_id(frame_mode_str.as_deref());

        let frame_mode_is_flat = frame_mode == FrameType::Flats;
        let frame_mode_is_dark = frame_mode == FrameType::Darks;

        let state = data.state.read().unwrap();
        let waiting = matches!(*state, State::Waiting);
        let shot_active = matches!(*state, State::Active{mode: Mode::SingleShot, ..});
        let liveview_active = matches!(*state, State::Active{mode: Mode::LiveView, ..});
        let saving_frames = matches!(*state, State::Active{mode: Mode::SavingRawFrames, ..});
        let saving_frames_paused = data.save_raw_pause.borrow().is_some();
        let live_active = matches!(*state, State::Active{mode: Mode::LiveStacking, ..});
        drop(state);

        let save_raw_btn_cap = match (frame_mode, saving_frames_paused) {
            (FrameType::Lights, false) => "Start save\nLIGHTs",
            (FrameType::Lights, true)  => "Continue\nLIGHTs",
            (FrameType::Darks,  false) => "Start save\nDARKs",
            (FrameType::Darks,  true)  => "Continue\nDARKs",
            (FrameType::Flats,  false) => "Start save\nFLATs",
            (FrameType::Flats,  true)  => "Continue\nFLATs",
        };
        gtk_utils::set_str(bldr, "btn_start_save_raw", save_raw_btn_cap);

        let can_change_cam_opts = !saving_frames && !live_active;
        let can_change_mode = waiting || shot_active;
        let can_change_frame_opts = waiting || liveview_active;
        let can_change_cal_ops = !live_active;

        gtk_utils::enable_actions(&data.main.window, &[
            ("take_shot",             exposure_supported && !shot_active && can_change_mode),
            ("stop_shot",             shot_active),
            ("start_live_stacking",   exposure_supported && !live_active && can_change_mode),
            ("stop_live_stacking",    live_active),
            ("start_save_raw_frames", exposure_supported && !saving_frames && can_change_mode),
            ("pause_save_raw_frames", saving_frames),
            ("stop_save_raw_frames",  saving_frames || saving_frames_paused),
        ]);

        gtk_utils::show_widgets(bldr, &[
            ("chb_fan",       fan_supported),
            ("chb_heater",    heater_supported),
            ("chb_low_noise", low_noise_supported),
        ]);

        gtk_utils::enable_widgets(bldr, &[
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
        ]);

        Ok(())
    });

}

fn update_camera_list(data: &Rc<CameraData>) {
    let dev_list = data.main.indi.get_devices_list();
    let cameras = dev_list
        .iter()
        .filter(|device|
            device.interface.contains(indi_api::DriverInterface::CCD_INTERFACE)
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
        } else if options.camera_name.is_some() {
            cb_camera_list.set_active_id(options.camera_name.as_deref());
        }
        if cb_camera_list.active_id().is_none() {
            cb_camera_list.set_active(Some(0));
        }
    }
    data.options.borrow_mut().camera_name = cb_camera_list.active_id().map(|s| s.to_string());
}

fn update_resolution_list(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, ||{
        let cb_bin = data.main.builder.object::<gtk::ComboBoxText>("cb_bin").unwrap();
        let last_bin = cb_bin.active_id();
        cb_bin.remove_all();
        let options = data.options.borrow_mut();
        let Some(ref camera) = options.camera_name else {
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
                device.interface.contains(indi_api::DriverInterface::CCD_INTERFACE)
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

fn start_taking_shots(
    data:    &Rc<CameraData>,
    mode:    Mode,
    counter: Option<FramesCounter>,
) {
    gtk_utils::exec_and_show_error(&data.main.window, move || {
        data.main.show_progress(0.0, String::new());
        let live_stacking_mode = mode == Mode::LiveStacking;
        let saving_frames_mode = mode == Mode::SavingRawFrames;
        let options = data.options.borrow();
        let Some(ref camera_name) = options.camera_name else {
            return Ok(());
        };
        let mut state = data.state.write().unwrap();
        data.main.indi.command_enable_blob(
            camera_name,
            None,
            indi_api::BlobEnable::Also
        )?;
        if data.main.indi.camera_is_fast_toggle_supported(camera_name)? {
            let use_fast_toggle =
                saving_frames_mode &&
                !options.frame.have_to_use_delay();
            data.main.indi.camera_enable_fast_toggle(
                camera_name,
                use_fast_toggle,
                true,
                SET_PROP_TIMEOUT,
            )?;
            if use_fast_toggle {
                let frames_count = if let Some(counter) = &counter {
                    counter.to_go
                } else if saving_frames_mode && !options.raw_frames.use_cnt {
                    100000
                } else {
                    1
                };
                data.main.indi.camera_set_fast_frames_count(
                    camera_name,
                    frames_count,
                    true,
                    SET_PROP_TIMEOUT,
                )?;
            }
        }
        apply_camera_options_and_take_shot(
            &data.main.indi,
            camera_name,
            &options.frame,
            &data.last_exposure
        )?;
        *state = State::Active {
            mode,
            camera: camera_name.clone(),
            frame:  options.frame.clone(),
            counter,
            thread_timer: Arc::clone(&data.main.thread_timer),
        };

        match mode {
            Mode::SingleShot => {
                data.main.set_cur_action_text("");
            },
            Mode::LiveView => {
                data.main.set_cur_action_text("Live view mode");
            },
            Mode::LiveStacking => {
                data.main.set_cur_action_text("Live stacking");
            },
            Mode::SavingRawFrames => {
                let text = match options.frame.frame_type {
                    FrameType::Lights => "Saving LIGHT frames",
                    FrameType::Flats => "Saving FLAT frames",
                    FrameType::Darks => "Saving DARK frames",
                };
                data.main.set_cur_action_text(text);
            },
        };

        drop(options);
        drop(state);
        if live_stacking_mode {
            gtk_utils::set_active_id(
                &data.main.builder,
                "cb_preview_src",
                Some("live")
            );
        } else {
            gtk_utils::set_active_id(
                &data.main.builder,
                "cb_preview_src",
                Some("frame")
            );
        }
        correct_ctrl_widgets_properties(data);

        Ok(())
    });
}

fn handler_action_take_shot(data: &Rc<CameraData>) {
    read_options_from_widgets(data);
    abort_current_shot(data, &data.state.read().unwrap());
    start_taking_shots(data, Mode::SingleShot, None);
}

fn handler_action_stop_shot(data: &Rc<CameraData>) {
    let mut state = data.state.write().unwrap();
    abort_current_shot(data, &state);
    *state = State::Waiting;
    drop(state);
    correct_ctrl_widgets_properties(data);
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
                &data.main.cur_frame,
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
            data.main.cur_frame.info.read().unwrap(),
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
                Some(value) => gtk_utils::set_str(bldr, "e_ovality", &format!("{:.3}", value)),
                None => gtk_utils::set_str(bldr, "e_ovality", ""),
            }
            gtk_utils::set_str(bldr, "e_stars", &info.stars.len().to_string());
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
    let state = data.state.read().unwrap();
    let is_single_shot = matches!(&*state, State::Active{mode: Mode::SingleShot, ..});
    let is_cont_shots = matches!(&*state, State::Active{mode: Mode::LiveView, ..});
    let is_live_staking = matches!(&*state, State::Active{mode: Mode::LiveStacking, ..});
    let is_raw_frames = matches!(&*state, State::Active{mode: Mode::SavingRawFrames, ..});
    drop(state);
    let orig_frame = data.options.borrow().preview.source == PreviewSource::OrigFrame;

    let show_resolution_info = |width, height| {
        gtk_utils::set_str(
            &data.main.builder,
            "e_res_info",
            &format!("{} x {}", width, height)
        );
    };

    let is_mode_current = |mode: ResultMode| {
           (is_single_shot && orig_frame && mode == ResultMode::OneShot)
        || (is_cont_shots && orig_frame && mode == ResultMode::LiveView)
        || (is_live_staking && orig_frame && mode == ResultMode::LiveFrame)
        || (is_live_staking && !orig_frame && mode == ResultMode::LiveResult)
        || (is_raw_frames && orig_frame && mode == ResultMode::RawFrame)
    };

    match result.data {
        ProcessingResultData::Error(error_text) => {
            let mut state = data.state.write().unwrap();
            abort_current_shot(data, &state);
            *state = State::Waiting;
            drop(state);
            correct_ctrl_widgets_properties(data);
            show_error_message(&data.main.window, "Fatal Error", &error_text);
        },
        ProcessingResultData::LightShortInfo(short_info) => {
            data.light_history.borrow_mut().push(short_info);
            update_light_history_table(data);
        },
        ProcessingResultData::SingleShotFinished => {
            if is_single_shot {
                *data.state.write().unwrap() = State::Waiting;
                correct_ctrl_widgets_properties(data);
                return;
            }
            show_shots_progress(data);
        },
        ProcessingResultData::Preview(img, mode) if is_mode_current(mode) => {
            show_preview_image(data, Some(img.rgb_bytes), Some(img.params));
            show_resolution_info(img.image_width, img.image_height);
        },
        ProcessingResultData::Histogram(mode) if is_mode_current(mode) => {
            repaint_histogram(data);
            show_histogram_stat(data);
        },
        ProcessingResultData::FrameInfo(mode) if is_mode_current(mode) => {
            show_image_info(data);
        },
        _ => {},
    }
}

fn show_shots_progress(data: &Rc<CameraData>) {
    let state = data.state.read().unwrap();
    let State::Active { counter, .. } = &*state else {
        return;
    };
    if let Some(counter) = counter {
        let progress = (counter.total - counter.to_go) as f64 / counter.total as f64;
        let text = format!("{} / {}", counter.total - counter.to_go, counter.total);
        data.main.show_progress(progress, text);
    }
    if matches!(counter, &Some(FramesCounter { to_go: 0, .. })) {
        drop(state);
        *data.state.write().unwrap() = State::Waiting;
        save_master_frame(data);
        correct_ctrl_widgets_properties(data);
    }
}

fn set_temperature_by_options(
    data:      &Rc<CameraData>,
    force_set: bool,
) {
    let options = data.options.borrow();
    let Some(ref camera_name) = options.camera_name else {
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
        abort_current_shot(data, &data.state.read().unwrap());
        start_taking_shots(data, Mode::LiveView, None);
    } else {
        stop_live_view(data);
    }
}

fn apply_camera_options_and_take_shot(
    indi:          &indi_api::Connection,
    camera_name:   &str,
    frame:         &FrameOptions,
    last_exposure: &Arc<AtomicI64>
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

    last_exposure.store((frame.exposure * 1000.0) as i64, atomic::Ordering::Relaxed);
    Ok(())
}

fn stop_live_view(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut state = data.state.write().unwrap();
        if let State::Active {
            mode: Mode::LiveView,
            camera,
            ..
        } = &*state {
            data.main.indi.camera_abort_exposure(camera)?;
            *state = State::Waiting;
        }
        drop(state);
        correct_ctrl_widgets_properties(data);
        Ok(())
    });
}

fn process_conn_state_event(
    data:       &Rc<CameraData>,
    conn_state: indi_api::ConnState
) {
    *data.conn_state.borrow_mut() = conn_state;
    correct_ctrl_widgets_properties(data);
}

fn process_prop_change_event(
    data:        &Rc<CameraData>,
    device_name: &str,
    prop_name:   &str,
    elem_name:   &str,
    new_prop:    bool,
    value:       &indi_api::PropValue,
) {
    if let indi_api::PropValue::Blob(blob) = value {
        process_blob_event(data, device_name, prop_name, elem_name, new_prop, blob);
    } else {
        process_simple_prop_change_event(data, device_name, prop_name, elem_name, new_prop, value);
    }
}

fn process_prop_delete_event(
    data:         &Rc<CameraData>,
    _device_name: &str,
    _prop_name:   &str,
){
    data.delayed_action.borrow_mut().set(
        DelayedFlags::UPDATE_CTRL_WIDGETS
    );
}

fn process_simple_prop_change_event(
    data:        &Rc<CameraData>,
    device_name: &str,
    prop_name:   &str,
    elem_name:   &str,
    new_prop:    bool,
    value:       &indi_api::PropValue,
) {
    match (prop_name, elem_name, value)
    {
        ("DRIVER_INFO", "DRIVER_INTERFACE", _) => {
            let flag_bits = value.as_i32().unwrap_or(0);
            let flags = indi_api::DriverInterface::from_bits_truncate(flag_bits as u32);
            if flags.contains(indi_api::DriverInterface::CCD_INTERFACE) {
                data.delayed_action.borrow_mut().set(
                    DelayedFlags::UPDATE_CAM_LIST
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

        _ => {},
    }
}

fn process_blob_event(
    data:        &Rc<CameraData>,
    device_name: &str,
    _prop_name:  &str,
    _elem_name:  &str,
    _new_prop:   bool,
    blob:        &Arc<indi_api::BlobPropValue>,
) {
    if blob.data.is_empty() { return; }
    log::debug!("process_blob_event, dl_time = {:.2}s", blob.dl_time);
    let state = data.state.read().unwrap();
    let State::Active { camera, mode,.. } = &*state else { return; };

    if device_name != camera { return; }

    read_options_from_widgets(data);
    let options = data.options.borrow();
    let preview_params = get_preview_params(data, &options);
    let result_fun = {
        let main_thread_sender = data.main_thread_sender.clone();
        move |res: FrameProcessingResult| {
            let send_err = main_thread_sender.send(
                MainThreadCommands::ShowFrameProcessingResult(res)
            );
            if let Err(err) = send_err {
                log::error!("process_blob_event: err={:?}", err);
            }
        }
    };

    let command = match mode {
        Mode::SingleShot | Mode::LiveView | Mode::SavingRawFrames => {
            let save_path = if *mode == Mode::SavingRawFrames {
                Some(options.raw_frames.out_path.clone())
            } else {
                None
            };
            Command::PreviewImage(PreviewImageCommand{
                camera:     device_name.to_string(),
                blob:       Arc::clone(blob),
                frame:      Arc::clone(&data.main.cur_frame),
                calibr:     options.calibr.into_params(),
                fn_gen:     Arc::clone(&data.fn_gen),
                options:    preview_params,
                save_path,
                live_view:  *mode == Mode::LiveView,
                raw_adder:  Arc::clone(&data.raw_adder),
                result_fun: Box::new(result_fun),
            })
        },
        Mode::LiveStacking => {
            let max_fwhm = if options.live.use_max_fwhm { Some(options.live.max_fwhm) } else { None };
            let max_ovality = if options.live.use_max_ovality { Some(options.live.max_ovality) } else { None };
            let min_stars = if options.live.use_min_stars { Some(options.live.min_stars) } else { None };
            let save_res_interv = if options.live.save_enabled { Some(options.live.save_minutes*60) } else { None };
            Command::LiveStacking(LiveStackingCommand{
                camera:           device_name.to_string(),
                blob:             Arc::clone(blob),
                frame:            Arc::clone(&data.main.cur_frame),
                calibr:           options.calibr.into_params(),
                data:             Arc::clone(&data.live_staking),
                fn_gen:           Arc::clone(&data.fn_gen),
                preview_params,
                max_fwhm,
                max_ovality,
                min_stars,
                save_path:        options.live.out_dir.clone(),
                save_orig_frames: options.live.save_orig,
                save_res_interv,
                result_fun:       Box::new(result_fun),
            })
        },
    };
    data.img_cmds_sender.send(command).unwrap();
}

fn process_blob_start_event(
    state:         &Arc<RwLock<State>>,
    indi:          &Arc<indi_api::Connection>,
    _event:        &Arc<indi_api::BlobStartEvent>,
    last_exposure: &Arc<AtomicI64>
) {
    let mut state = state.write().unwrap();
    let State::Active { mode, camera, frame, counter, thread_timer, .. } = &mut *state else {
        return
    };
    if *mode == Mode::SingleShot { return; }
    if let Some(counter) = counter {
        if counter.to_go != 0 {
            counter.to_go -= 1;
            if counter.to_go == 0 { return; }
        }
    }
    let fast_mode_enabled =
        indi.camera_is_fast_toggle_supported(camera).unwrap_or(false) &&
        indi.camera_is_fast_toggle_enabled(camera).unwrap_or(false);
    if !fast_mode_enabled {
        if !frame.have_to_use_delay() {
            let res = apply_camera_options_and_take_shot(
                indi,
                camera,
                frame,
                last_exposure
            );
            if let Err(err) = res {
                log::error!("{} during trying start next shot", err.to_string());
                // TODO: show error!!!
            }
        } else {
            let indi = Arc::clone(indi);
            let camera = camera.clone();
            let frame = frame.clone();
            let last_exposure = Arc::clone(last_exposure);
            thread_timer.exec((frame.delay * 1000.0) as u32, move || {
                let res = apply_camera_options_and_take_shot(
                    &indi,
                    &camera,
                    &frame,
                    &last_exposure
                );
                if let Err(err) = res {
                    log::error!("{} during trying start next shot", err.to_string());
                    // TODO: show error!!!
                }
            });
        }
    }
}

fn repaint_histogram(data: &Rc<CameraData>) {
    let da_histogram = data.main.builder.object::<gtk::DrawingArea>("da_histogram").unwrap();
    da_histogram.queue_draw();
}

fn show_histogram_stat(data: &Rc<CameraData>) {
    let options = data.options.borrow();
    let hist = match options.preview.source {
        PreviewSource::OrigFrame =>
            data.main.cur_frame.hist.read().unwrap(),
        PreviewSource::LiveStacking =>
            data.live_staking.result.hist.read().unwrap(),
    };
    let bldr = &data.main.builder;
    let max = hist.max as f64;
    let show_chan_data = |chan: &Option<HistogramChan>, l_cap, l_mean, l_median, l_dev| {
        if let Some(chan) = chan.as_ref() {
            gtk_utils::set_str(
                bldr, l_mean,
                &format!("{:.1} ({:.1}%)", chan.mean, 100.0 * chan.mean / max)
            );
            let median = chan.get_nth_element(chan.count/2);
            gtk_utils::set_str(
                bldr, l_median,
                &format!("{:.1} ({:.1}%)", median, 100.0 * median as f64 / max)
            );
            gtk_utils::set_str(
                bldr, l_dev,
                &format!("{:.1} ({:.1}%)", chan.std_dev, 100.0 * chan.std_dev / max)
            );
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
                data.main.cur_frame.hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                data.live_staking.result.hist.read().unwrap(),
        };
        paint_histogram(
            &hist,
            area,
            cr,
            area.allocated_width(),
            area.allocated_height(),
            options.hist_log_x,
            options.hist_log_y,
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

    let max_y_text = "100%";
    let min_y_text = "0%";

    let fg = area.style_context().color(gtk::StateFlags::ACTIVE);

    cr.set_font_size(12.0);

    let max_y_text_te = cr.text_extents(max_y_text)?;
    let left_margin = max_y_text_te.width() + 5.0;
    let right_margin = 3.0;
    let top_margin = 3.0;
    let bottom_margin = cr.text_extents(min_y_text)?.width() + 3.0;
    let area_width = width as f64 - left_margin - right_margin;
    let area_height = height as f64 - top_margin - bottom_margin;

    cr.set_source_rgb(0.5, 0.5, 0.5);
    cr.set_line_width(1.0);
    cr.rectangle(left_margin, top_margin, area_width, area_height);
    cr.stroke()?;

    cr.set_source_rgb(fg.red(), fg.green(), fg.blue());
    cr.move_to(0.0, top_margin+max_y_text_te.height());
    cr.show_text(max_y_text)?;
    cr.move_to(0.0, height as f64 - bottom_margin);
    cr.show_text(min_y_text)?;

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

    if total_max_v == 0 || max_count == 0 {
        return Ok(());
    }

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

    Ok(())
}

fn handler_action_start_live_stacking(data: &Rc<CameraData>) {
    *data.raw_adder.lock().unwrap() = None;
    read_options_from_widgets(data);
    abort_current_shot(data, &data.state.read().unwrap());
    start_taking_shots(data, Mode::LiveStacking, None);
}

fn handler_action_stop_live_stacking(data: &Rc<CameraData>) {
    let mut state = data.state.write().unwrap();
    abort_current_shot(data, &state);
    *state = State::Waiting;
    drop(state);
    correct_ctrl_widgets_properties(data);
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
    let last_exposure = data.last_exposure.load(atomic::Ordering::Relaxed) as f64 / 1000.0;
    if last_exposure < 1.0 { return; };
    let options = data.options.borrow();
    let Some(camera) = options.camera_name.as_ref() else { return; };
    let Ok(exposure) = data.main.indi.camera_get_exposure(camera) else { return ; };
    let progress = ((last_exposure - exposure) / last_exposure).max(0.0).min(1.0);
    let text_to_show = format!("{:.0} / {:.0}", last_exposure - exposure, last_exposure);
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
                String::static_type(), String::static_type(),
                u32   ::static_type(), String::static_type(),
                String::static_type(), String::static_type(),
                String::static_type(), String::static_type(),
            ]);
            let columns = [
                /* 0 */ "FWHM",
                /* 1 */ "Ovality",
                /* 2 */ "Stars",
                /* 3 */ "Noise",
                /* 4 */ "Background",
                /* 5 */ "Offs.X",
                /* 6 */ "Offs.Y",
                /* 7 */ "Rot."
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
    for item in &items[models_row_cnt..to_index] {
        let fwhm_str = item.stars_fwhm
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(String::new);
        let ovality_str = item.stars_ovality
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(String::new);
        let stars_cnt = item.stars_count as u32;
        let noise_str = format!("{:.3}%", item.noise);
        let bg_str = format!("{:.1}%", item.background);
        let x_str = item.offset_x
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(String::new);
        let y_str = item.offset_y
            .map(|v| format!("{:.1}", v))
            .unwrap_or_else(String::new);
        let angle_str = item.angle
            .map(|v| format!("{:.1}°", 180.0 * v / PI))
            .unwrap_or_else(String::new);
        let last_is_selected =
            gtk_utils::get_list_view_selected_row(&tree).map(|v| v+1) ==
            Some(models_row_cnt as i32);
        let last = model.insert_with_values(None, &[
            (0, &fwhm_str),
            (1, &ovality_str),
            (2, &stars_cnt),
            (3, &noise_str),
            (4, &bg_str),
            (5, &x_str),
            (6, &y_str),
            (7, &angle_str),
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
    abort_current_shot(data, &data.state.read().unwrap());

    let mut options = data.options.borrow_mut();
    if options.frame.frame_type != FrameType::Lights
    && options.raw_frames.create_master {
        let mut adder = data.raw_adder.lock().unwrap();
        *adder = Some(RawAdder::new());
    }

    let save_raw_pause = data.save_raw_pause.borrow();
    let counter = if let Some(save_raw_pause) = &*save_raw_pause {
        options.frame = save_raw_pause.frame.clone();
        Some(save_raw_pause.counter.clone())
    } else {
        Some(FramesCounter{
            to_go: options.raw_frames.frame_cnt,
            total: options.raw_frames.frame_cnt
        })
    };
    drop(options);

    if save_raw_pause.is_some() {
        show_options(data);
    }

    drop(save_raw_pause);

    start_taking_shots(data, Mode::SavingRawFrames, counter);
    show_shots_progress(data);
}

fn handler_action_pause_save_raw_frames(data: &Rc<CameraData>) {
    let mut state = data.state.write().unwrap();
    let State::Active {
        mode: Mode::SavingRawFrames,
        camera,
        frame,
        counter,
        ..
    } = &*state else {
        return;
    };
    let Some(counter) = counter else { return; };
    let pause_data = SavingRawPause {
        camera: camera.clone(),
        frame: frame.clone(),
        counter: counter.clone(),
    };
    log::info!("Saving raw paused with data {:?}", pause_data);
    *data.save_raw_pause.borrow_mut() = Some(pause_data);
    abort_current_shot(data, &state);
    *state = State::Waiting;
    drop(state);
    correct_ctrl_widgets_properties(data);
}

fn save_master_frame(data: &Rc<CameraData>) { // TODO: move to image_processing.rs
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let mut adder = data.raw_adder.lock().unwrap();
        if let Some(adder) = &mut *adder {
            let options = data.options.borrow_mut();
            let raw_image = adder.get()?;
            let (prefix, file_name) = match options.frame.frame_type {
                FrameType::Flats => ("flat", options.frame.create_master_flat_file_name()),
                FrameType::Darks => ("dark", options.frame.create_master_dark_file_name()),
                _ => unreachable!(),
            };
            let file_name = format!("{}_{}x{}-{}.fits", prefix, adder.width(), adder.height(), file_name);
            let full_file_name = options.raw_frames.out_path.join(file_name);
            raw_image.save_to_fits_file(&full_file_name)?;
            match options.frame.frame_type {
                FrameType::Flats => {
                    gtk_utils::set_path(
                        &data.main.builder,
                        "fch_master_flat",
                        Some(&full_file_name)
                    );
                },
                FrameType::Darks => {
                    gtk_utils::set_path(
                        &data.main.builder,
                        "fch_master_dark",
                        Some(&full_file_name)
                    );
                },
                _ => {},
            };
        }
        *adder = None;
        Ok(())
    });
}


fn handler_action_stop_save_raw_frames(data: &Rc<CameraData>) {
    *data.save_raw_pause.borrow_mut() = None;
    let mut state = data.state.write().unwrap();
    abort_current_shot(data, &state);
    *state = State::Waiting;
    drop(state);
    *data.raw_adder.lock().unwrap() = None;
    correct_ctrl_widgets_properties(data);
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