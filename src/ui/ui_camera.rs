use std::{rc::Rc, sync::{Arc, RwLock}, cell::{RefCell, Cell}};
use gtk::{cairo, glib::{self, clone}, prelude::*};
use macros::FromBuilder;
use crate::{
    core::{consts::*, core::*, events::*, frame_processing::*, utils::{FileNameArg, FileNameUtils}},
    image::{info::*, raw::{CalibrMethods, FrameType}},
    indi,
    options::*,
};
use super::{gtk_utils::*, module::*, ui_main::*, ui_start_dialog::StartDialog, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets {
        info:    InfoWidgets   ::from_builder_str(include_str!(r"resources/cam_info.ui")),
        common:  CommonWidgets ::from_builder_str(include_str!(r"resources/cam_common.ui")),
        ctrl:    ControlWidgets::from_builder_str(include_str!(r"resources/cam_ctrl.ui")),
        frame:   FrameWidgets  ::from_builder_str(include_str!(r"resources/cam_frame.ui")),
        calibr:  CalibrWidgets ::from_builder_str(include_str!(r"resources/cam_calibr.ui")),
        raw:     RawWidgets    ::from_builder_str(include_str!(r"resources/cam_raw.ui")),
        live_st: LiveStWidgets ::from_builder_str(include_str!(r"resources/cam_live.ui")),
        quality: QualityWidgets::from_builder_str(include_str!(r"resources/cam_quality.ui")),
    };

    let obj = Rc::new(CameraUi {
        widgets,
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        options:         Arc::clone(options),
        delayed_actions: DelayedActions::new(500),
        conn_state:      RefCell::new(indi::ConnState::Disconnected),
        indi_evt_conn:   RefCell::new(None),
        fn_utils:        RefCell::new(FileNameUtils::default()),
        closed:          Cell::new(false),
    });

    obj.init_cam_ctrl_widgets();
    obj.init_cam_widgets();
    obj.init_raw_widgets();
    obj.init_live_stacking_widgets();
    obj.init_frame_quality_widgets();

    obj.connect_common_events();
    obj.connect_widgets_events();

    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.handler_delayed_action(action);
        })
    );
    obj
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    UpdateCamList,
    StartLiveView,
    StartCooling,
    UpdateCtrlWidgets,
    UpdateResolutionList,
    SelectMaxResolution,
    FillHeaterItems,
    FillConvGainItems,
}

enum MainThreadEvent {
    Core(Event),
    Indi(indi::Event),
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
}

impl Gain {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("same") => Self::Same,
            Some("min")  => Self::Min,
            Some("25%")  => Self::P25,
            Some("50%")  => Self::P50,
            Some("75%")  => Self::P75,
            Some("max")  => Self::Max,
            _            => Self::Same,
        }
    }

    pub fn to_active_id(&self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Min  => "min",
            Self::P25  => "25%",
            Self::P50  => "50%",
            Self::P75  => "75%",
            Self::Max  => "max",
        }
    }
}

impl Binning {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("2") => Self::Bin2,
            Some("3") => Self::Bin3,
            Some("4") => Self::Bin4,
            _         => Self::Orig,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
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

impl Crop {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("75") => Self::P75,
            Some("50") => Self::P50,
            Some("33") => Self::P33,
            Some("25") => Self::P25,
            _          => Self::None,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::None => Some("100"),
            Self::P75  => Some("75"),
            Self::P50  => Some("50"),
            Self::P33  => Some("33"),
            Self::P25  => Some("25"),
        }
    }
}

impl StarRecognSensivity {
    pub fn from_active_id(active_id: Option<&str>) -> Self {
        match active_id {
            Some("low")    => Self::Low,
            Some("normal") => Self::Normal,
            Some("High")   => Self::High,
            _              => Self::Normal,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::Low    => Some("low"),
            Self::Normal => Some("normal"),
            Self::High   => Some("high"),
        }
    }
}

#[derive(FromBuilder)]
struct InfoWidgets {
    bx:              gtk::Box,
    l_temp_value:    gtk::Label,
    l_coolpwr_value: gtk::Label,
    da_shot_state:   gtk::DrawingArea,
}

#[derive(FromBuilder)]
struct CommonWidgets {
    bx:            gtk::Box,
    l_cam_list:    gtk::Label,
    cb_cam_list:   gtk::ComboBoxText,
    chb_live_view: gtk::CheckButton,
}

#[derive(FromBuilder)]
struct ControlWidgets {
    grid:          gtk::Grid,
    chb_cooler:    gtk::CheckButton,
    spb_temp:      gtk::SpinButton,
    chb_fan:       gtk::CheckButton,
    l_heater:      gtk::Label,
    cb_heater:     gtk::ComboBoxText,
    chb_low_noise: gtk::CheckButton,
    l_conv_gain:   gtk::Label,
    cb_conv_gain:  gtk::ComboBoxText,
    chb_high_fw:   gtk::CheckButton,
}

#[derive(FromBuilder)]
struct FrameWidgets {
    grid:       gtk::Grid,
    cb_mode:    gtk::ComboBoxText,
    spb_exp:    gtk::SpinButton,
    spb_gain:   gtk::SpinButton,
    spb_offset: gtk::SpinButton,
    cb_bin:     gtk::ComboBoxText,
    cb_crop:    gtk::ComboBoxText,
    l_calibr:   gtk::Label,
}

#[derive(FromBuilder)]
struct CalibrWidgets {
    grid:           gtk::Grid,
    chb_dark:       gtk::CheckButton,
    chb_flat:       gtk::CheckButton,
    fch_flat:       gtk::FileChooserButton,
    chb_hot_pixels: gtk::CheckButton,
    l_hot_px_warn:  gtk::Label,
}

#[derive(FromBuilder)]
struct RawWidgets {
    grid:            gtk::Grid,
    l_time_info:     gtk::Label,
    btn_start:       gtk::Button,
    chb_frames_cnt:  gtk::CheckButton,
    spb_frames_cnt:  gtk::SpinButton,
    fcb_path:        gtk::FileChooserButton,
    chb_save_master: gtk::CheckButton,
}

#[derive(FromBuilder)]
struct LiveStWidgets {
    grid:            gtk::Grid,
    chb_save_period: gtk::CheckButton,
    spb_save_period: gtk::SpinButton,
    chb_save_orig:   gtk::CheckButton,
    chb_no_tracks:   gtk::CheckButton,
    l_no_tracks:     gtk::Label,
    fch_path:        gtk::FileChooserButton,
}

#[derive(FromBuilder)]
struct QualityWidgets {
    bx:                   gtk::Box,
    chb_max_fwhm:         gtk::CheckButton,
    spb_max_fwhm:         gtk::SpinButton,
    chb_max_oval:         gtk::CheckButton,
    spb_max_oval:         gtk::SpinButton,
    chb_ignore_3px_stars: gtk::CheckButton,
    cbx_stars_sens:       gtk::ComboBox,
}

struct Widgets {
    info:    InfoWidgets,
    common:  CommonWidgets,
    ctrl:    ControlWidgets,
    frame:   FrameWidgets,
    calibr:  CalibrWidgets,
    raw:     RawWidgets,
    live_st: LiveStWidgets,
    quality: QualityWidgets,
}

struct CameraUi {
    widgets:         Widgets,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    delayed_actions: DelayedActions<DelayedAction>,
    conn_state:      RefCell<indi::ConnState>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    fn_utils:        RefCell<FileNameUtils>,
    closed:          Cell<bool>,
}

impl UiModule for CameraUi {
    fn show_options(&self, options: &Options) {
        self.show_common_options(options);
        self.show_frame_options(options);
        self.show_calibr_options(options);
        self.show_ctrl_options(options);
        self.show_raw_options(options);
        self.show_live_stacking_options(options);
        self.show_frame_quality_options(options);
    }

    fn get_options(&self, options: &mut Options) {
        self.get_common_options(options);
        self.get_ctrl_options(options);
        self.get_frame_options(options);
        self.get_calibr_options(options);
        self.get_raw_options(options);
        self.get_live_stacking_options(options);
        self.get_frame_quality_options(options);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "common",
                name:   String::new(),
                widget: self.widgets.common.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "cam_ctrl",
                name:   "Camera control".to_string(),
                widget: self.widgets.ctrl.grid.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::EXPANDED,
            },
            Panel {
                str_id: "cam_frame",
                name:   "Frame settings".to_string(),
                widget: self.widgets.frame.grid.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::EXPANDED,
            },
            Panel {
                str_id: "cam_calibr",
                name:   "Calibration & hot pixels".to_string(),
                widget: self.widgets.calibr.grid.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "cam_raw",
                name:   "Saving raw frames".to_string(),
                widget: self.widgets.raw.grid.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::EXPANDED,
            },
            Panel {
                str_id: "cam_live_st",
                name:   "Live stacking".to_string(),
                widget: self.widgets.live_st.grid.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::EXPANDED,
            },
            Panel {
                str_id: "cam_quality",
                name:   "Light frame quality".to_string(),
                widget: self.widgets.quality.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "cam_info",
                name:   String::new(),
                widget: self.widgets.info.bx.clone().upcast(),
                pos:    PanelPosition::BottomLeft,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER,
            },
        ]
    }

    fn process_event(&self, event: &UiModuleEvent) {
        match event {
            UiModuleEvent::AfterFirstShowOptions => {
                self.correct_widgets_props();
            }
            UiModuleEvent::ProgramClosing => {
                self.handler_closing();
            }
            _ => {}
        }
    }
}

impl Drop for CameraUi {
    fn drop(&mut self) {
        log::info!("CameraUi dropped");
    }
}

impl CameraUi {
    fn init_cam_ctrl_widgets(&self) {
        self.widgets.ctrl.spb_temp.set_range(-100.0, 100.0);
        self.widgets.info.l_temp_value.set_text("");
        self.widgets.info.l_coolpwr_value.set_text("");
    }

    fn init_cam_widgets(&self) {
        self.widgets.frame.spb_exp.set_range(0.0, 100_000.0);
        self.widgets.frame.spb_gain.set_range(0.0, 1_000_000.0);
        self.widgets.frame.spb_offset.set_range(0.0, 1_000_000.0);
    }

    fn init_raw_widgets(&self) {
        self.widgets.raw.spb_frames_cnt.set_range(1.0, 100_000.0);
        self.widgets.raw.spb_frames_cnt.set_digits(0);
        self.widgets.raw.spb_frames_cnt.set_increments(10.0, 100.0);
    }

    fn init_live_stacking_widgets(&self) {
        self.widgets.live_st.spb_save_period.set_range(1.0, 60.0);
        self.widgets.live_st.spb_save_period.set_digits(0);
        self.widgets.live_st.spb_save_period.set_increments(1.0, 10.0);
    }

    fn init_frame_quality_widgets(&self) {
        self.widgets.quality.spb_max_fwhm.set_range(1.0, 100.0);
        self.widgets.quality.spb_max_fwhm.set_digits(1);
        self.widgets.quality.spb_max_fwhm.set_increments(0.1, 1.0);

        self.widgets.quality.spb_max_oval.set_range(0.2, 2.0);
        self.widgets.quality.spb_max_oval.set_digits(1);
        self.widgets.quality.spb_max_oval.set_increments(0.1, 1.0);
    }

    fn connect_common_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();

        // INDI

        let sender = main_thread_sender.clone();
        *self.indi_evt_conn.borrow_mut() = Some(self.indi.subscribe_events(move |event| {
            sender.send_blocking(MainThreadEvent::Indi(event)).unwrap();
        }));

        // Core

        let sender = main_thread_sender.clone();
        self.core.event_subscriptions().subscribe(move |event| {
            sender.send_blocking(MainThreadEvent::Core(event)).unwrap();
        });

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                if self_.closed.get() { return; }
                self_.process_indi_or_core_event(event);
            }
        }));
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action   (&self.window, self, "take_shot",              Self::handler_action_take_shot);
        connect_action   (&self.window, self, "stop_shot",              Self::handler_action_stop_shot);
        connect_action_rc(&self.window, self, "start_save_raw_frames",  Self::handler_action_start_save_raw_frames);
        connect_action   (&self.window, self, "stop_save_raw_frames",   Self::handler_action_stop_save_raw_frames);
        connect_action   (&self.window, self, "continue_save_raw",      Self::handler_action_continue_save_raw_frames);
        connect_action_rc(&self.window, self, "start_live_stacking",    Self::handler_action_start_live_stacking);
        connect_action   (&self.window, self, "stop_live_stacking",     Self::handler_action_stop_live_stacking);
        connect_action   (&self.window, self, "continue_live_stacking", Self::handler_action_continue_live_stacking);

        self.widgets.info.da_shot_state.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |area, cr| {
                self_.handler_draw_shot_state(area, cr);
                glib::Propagation::Proceed
            })
        );

        self.widgets.common.cb_cam_list.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let Some(cur_id) = cb.active_id() else { return; };
                let new_device = DeviceAndProp::new(&cur_id);
                let old_device = options.cam.device.clone();
                if old_device.as_ref() == Some(&new_device) {
                    return;
                }
                options.cam.device = Some(new_device.clone());
                drop(options);

                self_.core.event_subscriptions().notify(
                    Event::CameraDeviceChanged {
                        from: old_device,
                        to:   new_device,
                    }
                );
            })
        );

        self.widgets.common.chb_live_view.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.live_view = chb.is_active();
                drop(options);
                self_.handler_live_view_changed();
            })
        );

        self.widgets.ctrl.chb_cooler.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.enable_cooler = chb.is_active();
                self_.show_calibr_file_for_frame(&options);
                drop(options);
                self_.control_camera_by_options(false);
                self_.correct_widgets_props();
            })
        );

        self.widgets.ctrl.cb_heater.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.heater_str = cb.active_id().map(|id| id.to_string());
                drop(options);
                self_.control_camera_by_options(false);
                self_.correct_widgets_props();
            })
        );

        self.widgets.ctrl.chb_fan.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.enable_fan = chb.is_active();
                drop(options);
                self_.control_camera_by_options(false);
                self_.correct_widgets_props();
            })
        );

        self.widgets.ctrl.spb_temp.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.temperature = spb.value();
                self_.show_calibr_file_for_frame(&options);
                drop(options);
                self_.control_camera_by_options(false);
            })
        );

        self.widgets.ctrl.cb_conv_gain.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.conv_gain_str = cb.active_id().map(|id| id.to_string());
            })
        );

        self.widgets.ctrl.chb_low_noise.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.low_noise = chb.is_active();
            })
        );

        self.widgets.ctrl.chb_high_fw.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.high_fullwell = chb.is_active();
            })
        );

        self.widgets.frame.cb_mode.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let frame_type = FrameType::from_active_id(cb.active_id().as_deref());
                options.cam.frame.frame_type = frame_type;
                self_.widgets.frame.spb_exp.set_value(options.cam.frame.exposure());
                self_.show_calibr_file_for_frame(&options);
                drop(options);
                self_.correct_widgets_props();
                self_.show_total_raw_time();
            })
        );

        self.widgets.frame.spb_exp.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.set_exposure(sb.value());
                self_.show_calibr_file_for_frame(&options);
                drop(options);
                self_.show_total_raw_time();
            })
        );

        self.widgets.frame.spb_gain.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.gain = sb.value();
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.frame.spb_offset.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.offset = sb.value() as i32;
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.frame.cb_bin.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let binning = Binning::from_active_id(cb.active_id().as_deref());
                options.cam.frame.binning = binning;
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.frame.cb_crop.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let crop = Crop::from_active_id(cb.active_id().as_deref());
                options.cam.frame.crop = crop;
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.raw.spb_frames_cnt.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.raw_frames.frame_cnt = sb.value() as usize;
            drop(options);
            self_.show_total_raw_time();
        }));

        self.widgets.quality.chb_max_fwhm.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.quality.use_max_fwhm = chb.is_active();
                drop(options);
                self_.correct_frame_quality_widgets_props();
            })
        );

        self.widgets.quality.chb_max_oval.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.quality.use_max_ovality = chb.is_active();
                self_.correct_frame_quality_widgets_props();
            })
        );

        self.widgets.quality.spb_max_fwhm.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.quality.max_fwhm = sb.value() as f32;
            })
        );

        self.widgets.quality.spb_max_oval.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.quality.max_ovality = sb.value() as f32;
            })
        );

        self.widgets.quality.chb_ignore_3px_stars.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.quality.ignore_3px_stars = chb.is_active();
            })
        );

        self.widgets.quality.cbx_stars_sens.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.quality.star_recgn_sens = StarRecognSensivity::from_active_id(
                    cb.active_id().as_deref()
                )
            })
        );

        self.widgets.calibr.chb_dark.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.dark_frame_en = chb.is_active();
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.calibr.chb_flat.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.flat_frame_en = chb.is_active();
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.calibr.fch_flat.connect_file_set(
            clone!(@weak self as self_ => move |fch| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.flat_frame_fname = fch.filename().unwrap_or_default();
                self_.show_calibr_file_for_frame(&options);
            })
        );

        self.widgets.calibr.chb_hot_pixels.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.widgets.calibr.l_hot_px_warn.set_visible(chb.is_active());
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.hot_pixels = chb.is_active();
                drop(options);
            })
        );

        self.widgets.live_st.chb_no_tracks.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.widgets.live_st.l_no_tracks.set_visible(chb.is_active());
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.live.remove_tracks = chb.is_active();
                drop(options);
            })
        );

    }

    fn process_indi_or_core_event(&self, event: MainThreadEvent) {
        match event {
            MainThreadEvent::Indi(indi::Event::ConnChange(conn_state)) =>
                self.process_indi_conn_state_event(conn_state),
            MainThreadEvent::Indi(indi::Event::PropChange(event_data)) => {
                match &event_data.change {
                    indi::PropChange::New(value) =>
                        self.process_indi_prop_change(
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            true,
                            None,
                            None,
                            &value.prop_value
                        ),
                    indi::PropChange::Change{ value, prev_state, new_state } =>
                        self.process_indi_prop_change(
                            &event_data.device_name,
                            &event_data.prop_name,
                            &value.elem_name,
                            false,
                            Some(prev_state),
                            Some(new_state),
                            &value.prop_value
                        ),
                    indi::PropChange::Delete => {}
                };
            },

            MainThreadEvent::Indi(
                indi::Event::DeviceConnected(_)|
                indi::Event::DeviceDelete(_)|
                indi::Event::NewDevice(_)
            ) => {
                self.delayed_actions.schedule(DelayedAction::UpdateCtrlWidgets);
            }
            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_widgets_props();
            }
            MainThreadEvent::Core(Event::ModeContinued) => {
                let options = self.options.read().unwrap();
                self.show_frame_options(&options);
            }
            MainThreadEvent::Core(Event::FrameProcessing(result)) => {
                self.show_frame_processing_result(result);
            }
            MainThreadEvent::Core(Event::CameraDeviceChanged { from, to }) => {
                self.handler_camera_changed(&from, &to);
            }
            _ => {},
        }
    }

    fn show_common_options(&self, options: &Options) {
        self.widgets.common.chb_live_view.set_active(options.cam.live_view);
        let cb_cam_list = &self.widgets.common.cb_cam_list;
        if let Some(device) = &options.cam.device {
            let id = device.to_string();
            cb_cam_list.set_active_id(Some(&id));
            if cb_cam_list.active_id().map(|v| v.as_str() != &id).unwrap_or(true) {
                cb_cam_list.append(Some(&id), &id);
                cb_cam_list.set_active_id(Some(&id));
            }
        } else {
            cb_cam_list.set_active_id(None);
        }
    }

    fn show_frame_options(&self, options: &Options) {
        let frame = &self.widgets.frame;
        frame.cb_mode.set_active_id(options.cam.frame.frame_type.to_active_id());
        frame.cb_bin.set_active_id(options.cam.frame.binning.to_active_id());
        frame.cb_crop.set_active_id(options.cam.frame.crop.to_active_id());

        set_spb_value(&frame.spb_exp, options.cam.frame.exposure());
        set_spb_value(&frame.spb_gain, options.cam.frame.gain);
        set_spb_value(&frame.spb_offset, options.cam.frame.offset as f64);
    }

    fn show_calibr_options(&self, options: &Options) {
        let calibr = &self.widgets.calibr;
        calibr.chb_dark.set_active(options.calibr.dark_frame_en);
        calibr.chb_flat.set_active(options.calibr.flat_frame_en);
        calibr.fch_flat.set_filename(&options.calibr.flat_frame_fname);
        calibr.chb_hot_pixels.set_active(options.calibr.hot_pixels);

        calibr.l_hot_px_warn.set_sensitive(options.calibr.hot_pixels);
    }

    fn show_ctrl_options(&self, options: &Options) {
        let ctrl = &self.widgets.ctrl;
        ctrl.chb_cooler.set_active(options.cam.ctrl.enable_cooler);
        ctrl.spb_temp.set_value(options.cam.ctrl.temperature);
        ctrl.chb_fan.set_active(options.cam.ctrl.enable_fan);
        ctrl.chb_high_fw.set_active(options.cam.ctrl.high_fullwell);
        ctrl.chb_low_noise.set_active(options.cam.ctrl.low_noise);
    }

    fn show_raw_options(&self, options: &Options) {
        let raw = &self.widgets.raw;
        raw.chb_frames_cnt.set_active(options.raw_frames.use_cnt);
        raw.spb_frames_cnt.set_value(options.raw_frames.frame_cnt as f64);
        raw.fcb_path.set_filename(&options.raw_frames.out_path);
        raw.chb_save_master.set_active(options.raw_frames.create_master);
    }

    fn show_live_stacking_options(&self, options: &Options) {
        let live = &self.widgets.live_st;
        live.chb_save_orig.set_active(options.live.save_orig);
        live.chb_save_period.set_active(options.live.save_enabled);
        live.spb_save_period.set_value(options.live.save_minutes as f64);
        live.fch_path.set_filename(&options.live.out_dir);
        live.chb_no_tracks.set_active(options.live.remove_tracks);
    }

    fn show_frame_quality_options(&self, options: &Options) {
        let qual = &self.widgets.quality;
        qual.chb_max_fwhm.set_active(options.quality.use_max_fwhm);
        qual.spb_max_fwhm.set_value(options.quality.max_fwhm as f64);
        qual.chb_max_oval.set_active(options.quality.use_max_ovality);
        qual.spb_max_oval.set_value(options.quality.max_ovality as f64);
        qual.chb_ignore_3px_stars.set_active(options.quality.ignore_3px_stars);
        qual.cbx_stars_sens.set_active_id(options.quality.star_recgn_sens.to_active_id());
    }

    pub fn get_common_options(&self, options: &mut Options) {
        options.cam.live_view = self.widgets.common.chb_live_view.is_active();
        options.cam.device    = self.widgets.common.cb_cam_list.active_id().map(|str| DeviceAndProp::new(&str));
    }

    pub fn get_ctrl_options(&self, options: &mut Options) {
        let ctrl = &self.widgets.ctrl;
        options.cam.ctrl.enable_cooler = ctrl.chb_cooler.is_active();
        options.cam.ctrl.temperature   = ctrl.spb_temp.value();
        options.cam.ctrl.enable_fan    = ctrl.chb_fan.is_active();
        options.cam.ctrl.low_noise     = ctrl.chb_low_noise.is_active();
        options.cam.ctrl.high_fullwell = ctrl.chb_high_fw.is_active();
    }

    pub fn get_frame_options(&self, options: &mut Options) {
        let frame = &self.widgets.frame;
        options.cam.frame.frame_type   = FrameType::from_active_id(frame.cb_mode.active_id().as_deref());
        options.cam.frame.set_exposure   (frame.spb_exp.value());
        options.cam.frame.gain         = frame.spb_gain.value();
        options.cam.frame.offset       = frame.spb_offset.value() as i32;
        options.cam.frame.binning      = Binning::from_active_id(frame.cb_bin.active_id().as_deref());
        options.cam.frame.crop         = Crop::from_active_id(frame.cb_crop.active_id().as_deref());
    }

    pub fn get_calibr_options(&self, options: &mut Options) {
        let calibr = &self.widgets.calibr;
        options.calibr.dark_frame_en     = calibr.chb_dark.is_active();
        options.calibr.flat_frame_en     = calibr.chb_flat.is_active();
        options.calibr.flat_frame_fname  = calibr.fch_flat.filename().unwrap_or_default();
        options.calibr.hot_pixels        = calibr.chb_hot_pixels.is_active();
    }

    pub fn get_raw_options(&self, options: &mut Options) {
        let raw = &self.widgets.raw;
        options.raw_frames.use_cnt       = raw.chb_frames_cnt.is_active();
        options.raw_frames.frame_cnt     = raw.spb_frames_cnt.value() as usize;
        options.raw_frames.out_path      = raw.fcb_path.filename().unwrap_or_default();
        options.raw_frames.create_master = raw.chb_save_master.is_active();
    }

    pub fn get_live_stacking_options(&self, options: &mut Options) {
        let live = &self.widgets.live_st;
        options.live.save_orig     = live.chb_save_orig.is_active();
        options.live.save_enabled  = live.chb_save_period.is_active();
        options.live.save_minutes  = live.spb_save_period.value() as usize;
        options.live.out_dir       = live.fch_path.filename().unwrap_or_default();
        options.live.remove_tracks = live.chb_no_tracks.is_active();
    }

    pub fn get_frame_quality_options(&self, options: &mut Options) {
        let qual = &self.widgets.quality;
        options.quality.use_max_fwhm     = qual.chb_max_fwhm.is_active();
        options.quality.max_fwhm         = qual.spb_max_fwhm.value() as f32;
        options.quality.use_max_ovality  = qual.chb_max_oval.is_active();
        options.quality.max_ovality      = qual.spb_max_oval.value() as f32;
        options.quality.ignore_3px_stars = qual.chb_ignore_3px_stars.is_active();
        options.quality.star_recgn_sens  = StarRecognSensivity::from_active_id(
            qual.cbx_stars_sens.active_id().as_deref()
        );
    }

    fn store_options_for_camera(
        &self,
        device:  Option<DeviceAndProp>,
        options: &mut Options
    ) {
        let Some(device) = device else { return; };
        let key = device.to_file_name_part();
        let sep_options = options.sep_cam.entry(key).or_insert(Default::default());
        sep_options.frame = options.cam.frame.clone();
        sep_options.ctrl = options.cam.ctrl.clone();
        sep_options.calibr = options.calibr.clone();
    }

    fn restore_options_for_camera(
        &self,
        device:  &DeviceAndProp,
        options: &mut Options
    ) {
        let key = device.to_file_name_part();
        if let Some(sep_options) = options.sep_cam.get(&key) {
            options.cam.frame = sep_options.frame.clone();
            options.cam.ctrl = sep_options.ctrl.clone();
            options.calibr = sep_options.calibr.clone();
        }
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        _ = self.core.stop_img_process_thread();

        _ = self.core.abort_active_mode();

        // Stores current camera options for current camera

        let mut options = self.options.write().unwrap();
        self.store_options_for_camera(options.cam.device.clone(), &mut *options);
        drop(options);

        // Unsubscribe events

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::UpdateCamList => {
                self.update_devices_list();
                self.correct_widgets_props();
            }
            DelayedAction::StartLiveView => {
                let live_view_flag = self.options.read().unwrap().cam.live_view;
                let mode = self.core.mode_data().mode.get_type();
                if live_view_flag && mode == ModeType::Waiting {
                    self.start_live_view();
                }
            }
            DelayedAction::StartCooling => {
                self.control_camera_by_options(true);
            }
            DelayedAction::UpdateCtrlWidgets => {
                self.correct_widgets_props();
            }
            DelayedAction::UpdateResolutionList => {
                self.update_resolution_list();
                let options = self.options.read().unwrap();
                self.init_fn_utils(options.cam.device.as_ref());
                self.show_calibr_file_for_frame(&options);
                drop(options);
            }
            DelayedAction::SelectMaxResolution => { // TODO: move to Core
                self.select_maximum_resolution();
            }
            DelayedAction::FillHeaterItems => {
                self.fill_heater_items_list();
            }
            DelayedAction::FillConvGainItems => {
                self.fill_conv_gain_items_list();
            }
        }
    }

    fn correct_widgets_props_impl(&self, camera: Option<&DeviceAndProp>) {
        let widgets = &self.widgets;

        let temp_supported = camera.as_ref().map(|camera| {
            let temp_value = self.indi.camera_get_temperature_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&widgets.ctrl.spb_temp, &temp_value, 0, Some(1.0))
        }).unwrap_or(false);
        let exposure_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&camera.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&widgets.frame.spb_exp, &exp_value, 4, Some(1.0))
        }).unwrap_or(false);
        let gain_supported = camera.as_ref().map(|camera| {
            let gain_value = self.indi.camera_get_gain_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&widgets.frame.spb_gain, &gain_value, 0, None)
        }).unwrap_or(false);
        let offset_supported = camera.as_ref().map(|camera| {
            let offset_value = self.indi.camera_get_offset_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&widgets.frame.spb_offset, &offset_value, 0, None)
        }).unwrap_or(false);
        let bin_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            self.indi.camera_is_binning_supported(&camera.name, cam_ccd).unwrap_or(false)
        }).unwrap_or(false);
        let fan_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_fan_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let heater_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_heater_str_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let low_noise_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_low_noise_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let crop_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            self.indi.camera_is_frame_supported(&camera.name, cam_ccd).unwrap_or(false)
        }).unwrap_or(false);
        let conv_gain_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_conversion_gain_str_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let high_fullwell_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_conversion_gain_str_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);

        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let cooler_active = widgets.ctrl.chb_cooler.is_active();
        let frame_mode_str = widgets.frame.cb_mode.active_id();

        let frame_mode = FrameType::from_active_id(frame_mode_str.as_deref());

        let frame_mode_is_lights = frame_mode == FrameType::Lights;
        let frame_mode_is_flat = frame_mode == FrameType::Flats;
        let frame_mode_is_dark = frame_mode == FrameType::Darks;

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let single_shot = mode_type == ModeType::SingleShot;
        let liveview_active = mode_type == ModeType::LiveView;
        let saving_frames = mode_type == ModeType::SavingRawFrames;
        let saving_frames_paused = mode_data.aborted_mode
            .as_ref()
            .map(|mode| mode.get_type() == ModeType::SavingRawFrames)
            .unwrap_or(false);
        let live_active = mode_type == ModeType::LiveStacking;
        let livestacking_paused = mode_data.aborted_mode
            .as_ref()
            .map(|mode| mode.get_type() == ModeType::LiveStacking)
            .unwrap_or(false);
        drop(mode_data);

        let save_raw_btn_cap = match frame_mode {
            FrameType::Lights => "Start save\nLIGHTs",
            FrameType::Darks  => "Start save\nDARKs",
            FrameType::Biases => "Start save\nBIASes",
            FrameType::Flats  => "Start save\nFLATs",
            FrameType::Undef  => "Error :(",
        };
        self.widgets.raw.btn_start.set_label(save_raw_btn_cap);

        let cam_active = self.indi
            .is_device_enabled(camera.as_ref().map(|c| c.name.as_str()).unwrap_or(""))
            .unwrap_or(false);

        let can_change_cam_opts = !saving_frames && !live_active;
        let can_change_mode = waiting || single_shot;
        let can_change_frame_opts = waiting || liveview_active;
        let can_change_live_stacking_opts = waiting || liveview_active;
        let can_change_cal_ops = !liveview_active;
        let cam_sensitive =
            indi_connected &&
            cam_active &&
            camera.is_some();

        enable_actions(&self.window, &[
            ("take_shot",              exposure_supported && !single_shot && can_change_mode),
            ("stop_shot",              single_shot),

            ("start_save_raw_frames",  exposure_supported && !saving_frames && can_change_mode),
            ("stop_save_raw_frames",   saving_frames),
            ("continue_save_raw",      saving_frames_paused && can_change_mode),

            ("start_live_stacking",    exposure_supported && !live_active && can_change_mode && frame_mode_is_lights),
            ("stop_live_stacking",     live_active),
            ("continue_live_stacking", livestacking_paused && can_change_mode),
            ("load_image",             waiting),
        ]);

        widgets.common.l_cam_list   .set_sensitive(waiting && indi_connected);
        widgets.common.cb_cam_list  .set_sensitive(waiting && indi_connected);
        widgets.common.chb_live_view.set_sensitive((exposure_supported && liveview_active) || can_change_mode);

        widgets.ctrl.grid         .set_sensitive(cam_sensitive);
        widgets.ctrl.chb_fan      .set_visible(fan_supported);
        widgets.ctrl.l_heater     .set_visible(heater_supported);
        widgets.ctrl.cb_heater    .set_visible(heater_supported);
        widgets.ctrl.l_conv_gain  .set_visible(conv_gain_supported);
        widgets.ctrl.cb_conv_gain .set_visible(conv_gain_supported);
        widgets.ctrl.chb_high_fw  .set_visible(high_fullwell_supported);
        widgets.ctrl.chb_low_noise.set_visible(low_noise_supported);
        widgets.ctrl.chb_fan      .set_sensitive(!cooler_active);
        widgets.ctrl.chb_cooler   .set_sensitive(temp_supported && can_change_cam_opts);
        widgets.ctrl.spb_temp     .set_sensitive(cooler_active && temp_supported && can_change_cam_opts);

        widgets.frame.grid      .set_sensitive(cam_sensitive);
        widgets.frame.cb_mode   .set_sensitive(can_change_frame_opts);
        widgets.frame.spb_exp   .set_sensitive(exposure_supported && can_change_frame_opts);
        widgets.frame.cb_crop   .set_sensitive(crop_supported && can_change_frame_opts);
        widgets.frame.spb_gain  .set_sensitive(gain_supported && can_change_frame_opts);
        widgets.frame.spb_offset.set_sensitive(offset_supported && can_change_frame_opts);
        widgets.frame.cb_bin    .set_sensitive(bin_supported && can_change_frame_opts);

        widgets.calibr.grid    .set_sensitive(cam_sensitive);
        widgets.calibr.chb_dark.set_sensitive(can_change_cal_ops);
        widgets.calibr.chb_flat.set_sensitive(can_change_cal_ops);
        widgets.calibr.fch_flat.set_sensitive(can_change_cal_ops);

        widgets.raw.grid           .set_sensitive(cam_sensitive);
        widgets.raw.chb_save_master.set_sensitive(can_change_cal_ops && (frame_mode_is_flat || frame_mode_is_dark) && !saving_frames);
        widgets.raw.chb_frames_cnt .set_sensitive(!saving_frames && can_change_mode);
        widgets.raw.spb_frames_cnt .set_sensitive(!saving_frames && can_change_mode);

        widgets.live_st.grid           .set_sensitive(cam_sensitive);
        widgets.live_st.chb_save_period.set_sensitive(can_change_live_stacking_opts);
        widgets.live_st.spb_save_period.set_sensitive(can_change_live_stacking_opts);
        widgets.live_st.chb_save_orig  .set_sensitive(can_change_live_stacking_opts);
        widgets.live_st.fch_path       .set_sensitive(can_change_live_stacking_opts);

        widgets.quality.bx.set_sensitive(cam_sensitive);
    }

    fn handler_camera_changed(&self, from: &Option<DeviceAndProp>, to: &DeviceAndProp) {
        let Ok(mut options) = self.options.try_write() else { return; };

        // Read options from widgets

        self.get_frame_options(&mut options);
        self.get_ctrl_options(&mut options);
        self.get_calibr_options(&mut options);

        // Store previous camera options

        self.store_options_for_camera(from.clone(), &mut *options);

        // Change some lists for new camera

        _ = self.update_resolution_list_impl(to, &options);
        self.fill_heater_items_list_impl(&options);
        self.fill_conv_gain_items_list_impl(&options);

        // Restore some options for specific camera

        self.restore_options_for_camera(to, &mut options);

        // Show some options for specific camera

        self.show_frame_options(&options);
        self.show_ctrl_options(&options);
        self.show_calibr_options(&options);

        // Show new total time

        self.show_total_raw_time_impl(&options);

        // Init fn_utils and show calibtarion files

        self.init_fn_utils(Some(&to));
        self.show_calibr_file_for_frame(&options);

        drop(options);

        self.correct_widgets_props_impl(Some(to));
        self.correct_frame_quality_widgets_props();
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let camera = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(camera.as_ref());
        self.correct_frame_quality_widgets_props();
    }

    fn correct_frame_quality_widgets_props(&self) {
        let qual = &self.widgets.quality;
        qual.spb_max_fwhm.set_sensitive(qual.chb_max_fwhm.is_active());
        qual.spb_max_oval.set_sensitive(qual.chb_max_oval.is_active());
    }

    fn init_fn_utils(&self, cam_device: Option<&DeviceAndProp>) {
        let Some(cam_device) = cam_device else { return; };
        let mut fn_utils = self.fn_utils.borrow_mut();
        fn_utils.init(&self.indi, cam_device);
    }

    fn show_calibr_file_for_frame(&self, options: &Options) {
        let mut result_str = String::new();
        if matches!(options.cam.frame.frame_type, FrameType::Lights|FrameType::Flats) {
            let to_calibrate = FileNameArg::Options(&options.cam);
            let fn_utils = self.fn_utils.borrow();
            let defect_pixel_file = fn_utils.defect_pixels_file_name(
                &to_calibrate,
                &options.calibr.dark_library_path
            );
            let (subtrack_fname, subtrack_method) = fn_utils.get_subtrack_master_fname(
                &to_calibrate,
                &options.calibr.dark_library_path
            );
            if options.calibr.dark_frame_en && subtrack_fname.is_file() {
                if subtrack_method.contains(CalibrMethods::BY_DARK) {
                    result_str += "Dark";
                } else if subtrack_method.contains(CalibrMethods::BY_BIAS) {
                    result_str += "Bias";
                }
            }

            if options.cam.frame.frame_type == FrameType::Lights
            && options.calibr.flat_frame_en
            && options.calibr.flat_frame_fname.is_file() {
                if !result_str.is_empty() { result_str += ", "; }
                result_str += "Flat";
            }

            if options.calibr.dark_frame_en && defect_pixel_file.is_file() {
                if !result_str.is_empty() { result_str += ", "; }
                result_str += "Hot pixels";
            }
        }
        if result_str.is_empty() {
            result_str += "---";
        }
        self.widgets.frame.l_calibr.set_label(&result_str);
    }

    fn update_devices_list(&self) {
        let options = self.options.read().unwrap();
        let cur_cam_device = options.cam.device.clone();
        drop(options);

        let cameras = self.indi.get_devices_list_by_interface(indi::DriverInterface::CCD);

        let mut list = Vec::new();
        for camera in cameras {
            for prop in ["CCD1", "CCD2", "CCD3"] {
                if self.indi.property_exists(&camera.name, prop, None).unwrap_or(false) {
                    let dev_and_prop = DeviceAndProp {
                        name: camera.name.to_string(),
                        prop: prop.to_string()
                    };
                    list.push(dev_and_prop.to_string());
                }
            }
        }

        let connected = self.indi.state() == indi::ConnState::Connected;

        let camera_selected = fill_devices_list_into_combobox(
            &list,
            &self.widgets.common.cb_cam_list,
            cur_cam_device.as_ref().map(|d| d.name.as_str()),
            connected,
            |id| {
                let Ok(mut options) = self.options.try_write() else { return; };
                options.cam.device = Some(DeviceAndProp::new(id));
            }
        );

        if camera_selected {
            let options = self.options.read().unwrap();
            let cur_cam_device = options.cam.device.clone();
            drop(options);

            if let Some(cur_cam_device) = &cur_cam_device {
                let Ok(mut options) = self.options.try_write() else { return; };
                self.restore_options_for_camera(&cur_cam_device, &mut options);
                drop(options);

                let options = self.options.read().unwrap();
                self.show_frame_options(&options);
                self.show_calibr_options(&options);
                self.show_ctrl_options(&options);
            }

            self.correct_widgets_props();
        }
    }

    fn update_resolution_list_impl(
        &self,
        cam_dev: &DeviceAndProp,
        options: &Options
    ) {
        let cb_bin = &self.widgets.frame.cb_bin;
        let last_bin = cb_bin.active_id();
        cb_bin.remove_all();
        let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_dev.prop);
        let Ok((max_width, max_height)) = self.indi.camera_get_max_frame_size(&cam_dev.name, cam_ccd) else {
            return;
        };
        let Ok((max_hor_bin, max_vert_bin)) = self.indi.camera_get_max_binning(&cam_dev.name, cam_ccd) else {
            return;
        };
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
            cb_bin.set_active_id(options.cam.frame.binning.to_active_id());
        }
        if cb_bin.active_id().is_none() {
            cb_bin.set_active(Some(0));
        }
    }

    fn update_resolution_list(&self) {
        let options = self.options.read().unwrap();
        let Some(cur_cam_device) = &options.cam.device else { return; };
        self.update_resolution_list_impl(cur_cam_device, &options);
    }

    fn fill_heater_items_list(&self) {
        let options = self.options.read().unwrap();
        self.fill_heater_items_list_impl(&options);
    }

    fn fill_heater_items_list_impl(&self, options: &Options) {
        exec_and_show_error(Some(&self.window), ||{
            let cb = &self.widgets.ctrl.cb_heater;
            let last_value = cb.active_id();
            cb.remove_all();
            let Some(device) = &options.cam.device else { return Ok(()); };
            if device.name.is_empty() { return Ok(()); };
            if !self.indi.camera_is_heater_str_supported(&device.name)? { return Ok(()) }
            let Some(items) = self.indi.camera_get_heater_items(&device.name)? else { return Ok(()); };
            for (id, label) in items {
                cb.append(Some(id.as_str()), &label);
            }
            if last_value.is_some() {
                cb.set_active_id(last_value.as_deref());
            } else {
                cb.set_active_id(options.cam.ctrl.heater_str.as_deref());
            }
            if cb.active_id().is_none() {
                cb.set_active(Some(0));
            }
            Ok(())
        });
    }

    fn fill_conv_gain_items_list(&self) {
        let options = self.options.read().unwrap();
        self.fill_conv_gain_items_list_impl(&options);
    }

    fn fill_conv_gain_items_list_impl(&self, options: &Options) {
        exec_and_show_error(Some(&self.window), ||{
            let cb = &self.widgets.ctrl.cb_conv_gain;
            let last_value = cb.active_id();
            cb.remove_all();
            let Some(device) = &options.cam.device else { return Ok(()); };
            if device.name.is_empty() { return Ok(()); };
            if !self.indi.camera_is_conversion_gain_str_supported(&device.name)? { return Ok(()) }
            let Some(items) = self.indi.camera_get_conversion_gain_items(&device.name)? else { return Ok(()); };
            for (id, label) in items {
                cb.append(Some(id.as_str()), &label);
            }
            if last_value.is_some() {
                cb.set_active_id(last_value.as_deref());
            } else {
                cb.set_active_id(options.cam.ctrl.conv_gain_str.as_deref());
            }
            if cb.active_id().is_none() {
                cb.set_active(Some(0));
            }
            Ok(())
        });
    }

    fn select_maximum_resolution(&self) { // TODO: move to Core
        let options = self.options.read().unwrap();
        let Some(device) = &options.cam.device else { return; };
        let cam_name = &device.name;
        if cam_name.contains(" Simulator") { return; } // don't do it for simulators

        if cam_name.is_empty() { return; }

        if self.indi.camera_is_resolution_supported(cam_name).unwrap_or(false) {
            _ = self.indi.camera_select_max_resolution(
                cam_name,
                true,
                None
            );
        }
    }

    fn start_live_view(&self) {
        self.main_ui.get_all_options();
        exec_and_show_error(Some(&self.window), || {
            self.core.start_live_view()?;
            Ok(())
        });
    }

    fn handler_action_take_shot(&self) {
        self.main_ui.get_all_options();
        exec_and_show_error(Some(&self.window), || {
            self.core.start_single_shot()?;
            Ok(())
        });
    }

    fn handler_action_stop_shot(&self) {
        self.core.abort_active_mode();
    }

    // TODO: move camera control code into `core` module
    fn control_camera_by_options(&self, force_set: bool) {
        let options = self.options.read().unwrap();
        let Some(device) = &options.cam.device else { return; };
        let camera_name = &device.name;
        if camera_name.is_empty() { return; };
        exec_and_show_error(Some(&self.window), || {
            // Cooler + Temperature
            if self.indi.camera_is_cooler_supported(camera_name)? {
                self.indi.camera_enable_cooler(
                    camera_name,
                    options.cam.ctrl.enable_cooler,
                    true,
                    INDI_SET_PROP_TIMEOUT
                )?;
                if options.cam.ctrl.enable_cooler {
                    self.indi.camera_set_temperature(
                        camera_name,
                        options.cam.ctrl.temperature
                    )?;
                }
            }
            // Fan
            if self.indi.camera_is_fan_supported(camera_name)? {
                self.indi.camera_control_fan(
                    camera_name,
                    options.cam.ctrl.enable_fan || options.cam.ctrl.enable_cooler,
                    force_set,
                    INDI_SET_PROP_TIMEOUT
                )?;
            }
            // Window heater
            if self.indi.camera_is_heater_str_supported(camera_name)? {
                if let Some(heater_str) = &options.cam.ctrl.heater_str {
                    self.indi.camera_set_heater_str(
                        camera_name,
                        heater_str,
                        force_set,
                        INDI_SET_PROP_TIMEOUT
                    )?;
                }
            }
            Ok(())
        });
    }

    fn show_cur_temperature_value(
        &self,
        device_name: &str,
        temparature: f64
    ) {
        let options = self.options.read().unwrap();
        let Some(cur_cam_device) = &options.cam.device else { return; };
        if cur_cam_device.name == device_name {
            self.widgets.info.l_temp_value.set_label(
                &format!("T: {:.1}°C", temparature)
            );
        }
    }

    fn show_coolpwr_value(
        &self,
        device_name: &str,
        pwr_str:     &str
    ) {
        let options = self.options.read().unwrap();
        let Some(cur_cam_device) = &options.cam.device else { return; };
        if cur_cam_device.name == device_name {
            self.widgets.info.l_coolpwr_value.set_label(
                &format!("Pwr: {}", pwr_str)
            );
        }
    }

    fn handler_live_view_changed(&self) {
        if self.indi.state() != indi::ConnState::Connected {
            return;
        }
        if self.options.read().unwrap().cam.live_view {
            self.main_ui.get_all_options();
            self.start_live_view();
        } else {
            self.core.abort_active_mode();
        }
    }

    fn process_indi_conn_state_event(&self, conn_state: indi::ConnState) {
        let disconnect_event =
            conn_state == indi::ConnState::Disconnected ||
            conn_state == indi::ConnState::Disconnecting;
        *self.conn_state.borrow_mut() = conn_state;

        if disconnect_event {
            let mut options = self.options.write().unwrap();
            self.store_options_for_camera(options.cam.device.clone(), &mut options);
        }

        if disconnect_event {
            self.update_devices_list();
        }
        self.correct_widgets_props();
    }

    fn process_indi_prop_change(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str,
        new_prop:    bool,
        _prev_state: Option<&indi::PropState>,
        _new_state:  Option<&indi::PropState>,
        value:       &indi::PropValue,
    ) {
        if indi::Connection::camera_is_heater_str_property(prop_name) && new_prop {
            self.delayed_actions.schedule(DelayedAction::FillHeaterItems);
            self.delayed_actions.schedule(DelayedAction::StartCooling);
        }

        if indi::Connection::camera_is_conversion_gain_property(prop_name) && new_prop {
            self.delayed_actions.schedule(DelayedAction::FillConvGainItems);
        }

        if indi::Connection::camera_is_cooler_pwr_property(prop_name, elem_name) {
            self.show_coolpwr_value(device_name, &value.to_string());
        }

        match (prop_name, elem_name, value) {
            ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE"|"CCD_TEMPERATURE",
             indi::PropValue::Num(indi::NumPropValue{value, ..})) => {
                if new_prop {
                    self.delayed_actions.schedule(
                        DelayedAction::StartCooling
                    );
                }
                self.show_cur_temperature_value(device_name, *value);
            }

            ("CCD_COOLER", ..)
            if new_prop => {
                self.delayed_actions.schedule(DelayedAction::StartCooling);
                self.delayed_actions.schedule(DelayedAction::UpdateCtrlWidgets);
            }

            ("CCD_OFFSET", ..) | ("CCD_GAIN", ..) | ("CCD_CONTROLS", ..)
            if new_prop => {
                self.delayed_actions.schedule(DelayedAction::UpdateCtrlWidgets);
            }

            ("CCD_EXPOSURE"|"GUIDER_EXPOSURE", ..) => {
                let options = self.options.read().unwrap();
                if new_prop {
                    if options.cam.device.as_ref().map(|d| d.name == device_name).unwrap_or(false) {
                        self.delayed_actions.schedule_ex(
                            DelayedAction::StartLiveView,
                            // 2000 ms pause to start live view from camera
                            // after connecting to INDI server
                            2000
                        );
                    }
                } else {
                    self.update_shot_state();
                }
            }

            ("CCD_RESOLUTION", ..) if new_prop => {
                self.delayed_actions.schedule(
                    DelayedAction::SelectMaxResolution
                );
            }

            ("CCD_INFO", "CCD_MAX_X", ..) |
            ("CCD_INFO", "CCD_MAX_Y", ..) => {
                self.delayed_actions.schedule(
                    DelayedAction::UpdateResolutionList
                );
            }

            ("CCD1"|"CCD2", ..) if new_prop => {
                self.delayed_actions.schedule(DelayedAction::UpdateCamList);
            }
            _ => {},
        }
    }

    fn handler_action_start_live_stacking(self: &Rc<Self>) {
        self.main_ui.get_all_options();

        let ok = exec_and_show_error(Some(&self.window), || {
            self.core.check_before_saving_raw_or_live_stacking()?;
            Ok(())
        });
        if !ok { return; }

        let info_pairs = self.get_short_info_pairs(true);
        let dialog = StartDialog::new(
            self.window.upcast_ref(),
            "Start live stacking",
            &info_pairs
        );
        dialog.exec(clone!(@strong self as self_ => move || {
            self_.core.start_live_stacking()?;
            Ok(())
        }));
    }

    fn handler_action_stop_live_stacking(&self) {
        self.core.abort_active_mode();
    }

    fn handler_action_continue_live_stacking(&self) {
        self.main_ui.get_all_options();
        exec_and_show_error(Some(&self.window), || {
            self.core.check_before_saving_raw_or_live_stacking()?;
            self.core.continue_prev_mode()?;
            Ok(())
        });
    }

    fn update_shot_state(&self) {
        self.widgets.info.da_shot_state.queue_draw();
    }

    fn handler_draw_shot_state(
        &self,
        area: &gtk::DrawingArea,
        cr:   &cairo::Context
    ) {
        let mode_data = self.core.mode_data();
        let Some(cur_exposure) = mode_data.mode.get_cur_exposure() else {
            return;
        };
        if cur_exposure < 1.0 { return; };
        let options = self.options.read().unwrap();
        let Some(device) = &options.cam.device else { return; };
        let cam_ccd = indi::CamCcd::from_ccd_prop_name(&device.prop);
        let Ok(exposure) = self.indi.camera_get_exposure(&device.name, cam_ccd) else { return; };
        let progress = ((cur_exposure - exposure) / cur_exposure).max(0.0).min(1.0);
        let text_to_show = format!("{:.0} / {:.0}", cur_exposure - exposure, cur_exposure);
        exec_and_show_error(Some(&self.window), || {
            draw_progress_bar(area, cr, progress, &text_to_show)
        });
    }

    fn get_short_info_pairs(&self, for_live_stacking: bool) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let options = self.options.read().unwrap();
        let cam = options.cam.device.as_ref().map(|d| d.to_string()).unwrap_or_default();
        let total_time = options.cam.frame.exposure() * options.raw_frames.frame_cnt as f64;
        let light_frames = options.cam.frame.frame_type == FrameType::Lights;

        pairs.push(("Camera".to_string(), cam));
        pairs.push(("Frames".to_string(), options.cam.frame.frame_type.to_str().to_string()));
        pairs.push(("Exposure".to_string(), format!("{:.4}", options.cam.frame.exposure())));

        if !for_live_stacking {
            pairs.push(("Frames count".to_string(), format!("{}", options.raw_frames.frame_cnt)));
            pairs.push(("Total time".to_string(), format!("~{}", seconds_to_total_time_str(total_time, true))));
        } else {
            if options.live.save_enabled {
                pairs.push(("Save every".to_string(), format!("{} minutes", options.live.save_minutes)));
            }
            if options.live.save_orig {
                pairs.push(("Save originals".to_string(), "Yes".to_string()));
            }
            if options.live.remove_tracks {
                pairs.push(("Remove tracks".to_string(), "Yes".to_string()));
            }
        }

        if (for_live_stacking || light_frames)
        && (options.calibr.dark_frame_en || options.calibr.flat_frame_en) {
            let mut value = String::new();
            if options.calibr.dark_frame_en {
                value += "Darks library";
            }
            if options.calibr.flat_frame_en {
                if !value.is_empty() { value += "\n"; }
                value += "Master flat frame";
            }
            pairs.push(("Calibration".to_string(), value));
        }

        if (for_live_stacking || light_frames)
        && options.focuser.is_used() {
            let mut value = String::new();

            if options.focuser.on_temp_change {
                value += &format!("Temp. change >{:.1}°", options.focuser.max_temp_change);
            }

            if options.focuser.on_fwhm_change {
                if !value.is_empty() { value += "\n"; }
                value += &format!("FWHM change >{:.1}px", options.focuser.max_fwhm_change);
            }

            if options.focuser.periodically {
                if !value.is_empty() { value += "\n"; }
                value += &format!("Each {} minutes", options.focuser.period_minutes);
            }

            pairs.push(("Autofocus".to_string(), value));
        }

        if (for_live_stacking || light_frames) && options.guiding.is_used() {
            match options.guiding.mode {
                GuidingMode::MainCamera => {
                    pairs.push((
                        "Guiding".to_string(),
                        "By main camera".to_string(),
                    ));
                    if options.guiding.dith_period != 0 {
                        pairs.push((
                            "Dithering".to_string(),
                            format!(
                                "{} px each {} minutes",
                                options.guiding.main_cam.dith_dist,
                                options.guiding.dith_period
                            )
                        ));
                    }
                }
                GuidingMode::External => {
                    pairs.push((
                        "Guiding".to_string(),
                        "By external program".to_string(),
                    ));
                    if options.guiding.dith_period != 0 {
                        pairs.push((
                            "Dithering".to_string(),
                            format!(
                                "{} px each {} minutes",
                                options.guiding.ext_guider.dith_dist,
                                options.guiding.dith_period
                            )
                        ));
                    }
                }
                _ => {},
            }
        }

        pairs
    }

    fn handler_action_start_save_raw_frames(self: &Rc<Self>) {
        self.main_ui.get_all_options();

        let ok = exec_and_show_error(Some(&self.window), || {
            self.core.check_before_saving_raw_or_live_stacking()?;
            Ok(())
        });
        if !ok { return; }

        let info_pairs = self.get_short_info_pairs(false);
        let dialog = StartDialog::new(
            self.window.upcast_ref(),
            "Start save RAW files",
            &info_pairs
        );
        dialog.exec(clone!(@strong self as self_ => move || {
            self_.core.start_saving_raw_frames()?;
            Ok(())
        }));
    }

    fn handler_action_continue_save_raw_frames(&self) {
        self.main_ui.get_all_options();
        exec_and_show_error(Some(&self.window), || {
            self.core.check_before_saving_raw_or_live_stacking()?;
            self.core.continue_prev_mode()?;
            Ok(())
        });
    }

    fn handler_action_stop_save_raw_frames(&self) {
        self.core.abort_active_mode();
    }

    fn show_total_raw_time_impl(&self, options: &Options) {
        let total_time =
            options.cam.frame.exposure() *
            options.raw_frames.frame_cnt as f64;
        let text = format!(
            "{:.1}s x {} ~ {}",
            options.cam.frame.exposure(),
            options.raw_frames.frame_cnt,
            seconds_to_total_time_str(total_time, false)
        );
        self.widgets.raw.l_time_info.set_label(&text);
    }

    fn show_total_raw_time(&self) {
        let options = self.options.read().unwrap();
        self.show_total_raw_time_impl(&options);
    }

    fn show_frame_processing_result(&self, result: FrameProcessResult) {
        match result.data {
            // TODO: move to main_ui
            FrameProcessResultData::Error(error_text) => {
                _ = self.core.abort_active_mode();
                self.correct_widgets_props();
                show_error_message(Some(&self.window), "Fatal Error", &error_text);
            }
            FrameProcessResultData::MasterSaved { frame_type: FrameType::Flats, file_name } => {
                self.widgets.calibr.fch_flat.set_filename(&file_name);
            }
            _ => {}
        }
    }
}