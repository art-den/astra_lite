use std::{rc::Rc, sync::*, cell::{RefCell, Cell}, path::PathBuf};
use chrono::{DateTime, Local, Utc};
use gtk::{cairo, glib::{self, clone}, prelude::*};
use serde::{Serialize, Deserialize};
use crate::{
    core::{consts::*, core::*, events::*, frame_processing::*},
    image::{histogram::*, image::RgbU8Data, info::*, io::save_image_to_tif_file, raw::{CalibrMethods, FrameType}, stars_offset::Offset},
    indi,
    options::*,
    utils::{gtk_utils::{self, *}, io_utils::*, log_utils::*}
};
use super::{ui_darks_library::DarksLibraryDialog, ui_main::*, utils::*};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    main_ui:  &Rc<MainUi>,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    indi:     &Arc<indi::Connection>,
    handlers: &mut MainUiEventHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, CameraUi::CONF_FN)?;
        Ok(())
    });

    let obj = Rc::new(CameraUi {
        main_ui:            Rc::clone(main_ui),
        builder:            builder.clone(),
        window:             window.clone(),
        core:               Arc::clone(core),
        indi:               Arc::clone(indi),
        options:            Arc::clone(options),
        delayed_actions:    DelayedActions::new(500),
        ui_options:         RefCell::new(ui_options),
        conn_state:         RefCell::new(indi::ConnState::Disconnected),
        indi_evt_conn:      RefCell::new(None),
        preview_scroll_pos: RefCell::new(None),
        light_history:      RefCell::new(Vec::new()),
        calibr_history:     RefCell::new(Vec::new()),
        closed:             Cell::new(false),
        full_screen_mode:   Cell::new(false),
        flat_info:          RefCell::new(FlatImageInfo::default()),
        self_:              RefCell::new(None),
    });

    *obj.self_.borrow_mut() = Some(Rc::clone(&obj));

    obj.init_cam_ctrl_widgets();
    obj.init_cam_widgets();
    obj.init_raw_widgets();
    obj.init_preview_widgets();
    obj.init_live_stacking_widgets();
    obj.init_frame_quality_widgets();

    obj.show_ui_options();
    obj.connect_common_events();
    obj.connect_widgets_events();
    obj.connect_main_ui_events(handlers);

    obj.show_total_raw_time();
    obj.update_light_history_table();
    obj.update_calibr_history_table();

    obj.connect_img_mouse_scroll_events();

    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.handler_delayed_action(action);
        })
    );

    obj.correct_widgets_props();
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
}

#[derive(Serialize, Deserialize, Debug,)]
#[serde(default)]
struct StoredCamOptions {
    cam:    DeviceAndProp,
    frame:  FrameOptions,
    ctrl:   CamCtrlOptions,
    calibr: CalibrOptions,
}

impl Default for StoredCamOptions {
    fn default() -> Self {
        Self {
            cam:    DeviceAndProp::default(),
            frame:  FrameOptions::default(),
            ctrl:   CamCtrlOptions::default(),
            calibr: CalibrOptions::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    paned_pos1:     i32,
    paned_pos2:     i32,
    paned_pos3:     i32,
    paned_pos4:     i32,
    cam_ctrl_exp:   bool,
    shot_exp:       bool,
    calibr_exp:     bool,
    raw_frames_exp: bool,
    live_exp:       bool,
    quality_exp:    bool,
    all_cam_opts:   Vec<StoredCamOptions>,
    hist_log_y:     bool,
    hist_percents:  bool,
    flat_percents:  bool,
}

impl Default for UiOptions {
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
            quality_exp:    true,
            all_cam_opts:   Vec::new(),
            hist_log_y:     false,
            hist_percents:  true,
            flat_percents:  true,
        }
    }
}

enum MainThreadEvent {
    Core(Event),
    Indi(indi::Event),
}

struct LightHistoryItem {
    mode_type:      ModeType,
    time:           Option<DateTime<Utc>>,
    stars_fwhm:     Option<f32>,
    bad_fwhm:       bool,
    stars_ovality:  Option<f32>,
    bad_ovality:    bool,
    stars_count:    usize,
    noise:          Option<f32>, // %
    background:     f32, // %
    offset:         Option<Offset>,
    bad_offset:     bool,
    calibr_methods: CalibrMethods,
}

struct CalibrHistoryItem {
    time:           Option<DateTime<Utc>>,
    mode_type:      ModeType,
    frame_type:     FrameType,
    mean:           f32,
    median:         u16,
    std_dev:        f32,
    calibr_methods: CalibrMethods, // for flat files
}

struct CameraUi {
    main_ui:            Rc<MainUi>,
    builder:            gtk::Builder,
    window:             gtk::ApplicationWindow,
    options:            Arc<RwLock<Options>>,
    core:               Arc<Core>,
    indi:               Arc<indi::Connection>,
    delayed_actions:    DelayedActions<DelayedAction>,
    ui_options:         RefCell<UiOptions>,
    conn_state:         RefCell<indi::ConnState>,
    indi_evt_conn:      RefCell<Option<indi::Subscription>>,
    preview_scroll_pos: RefCell<Option<((f64, f64), (f64, f64))>>,
    light_history:      RefCell<Vec<LightHistoryItem>>,
    calibr_history:     RefCell<Vec<CalibrHistoryItem>>,
    closed:             Cell<bool>,
    full_screen_mode:   Cell<bool>,
    flat_info:          RefCell<FlatImageInfo>,
    self_:              RefCell<Option<Rc<CameraUi>>>,
}

impl Drop for CameraUi {
    fn drop(&mut self) {
        log::info!("CameraUi dropped");
    }
}

impl CameraUi {
    const CONF_FN: &'static str = "ui_camera";

    fn init_cam_ctrl_widgets(&self) {
        let spb_temp = self.builder.object::<gtk::SpinButton>("spb_temp").unwrap();
        spb_temp.set_range(-100.0, 100.0);

        let l_temp_value = self.builder.object::<gtk::Label>("l_temp_value").unwrap();
        l_temp_value.set_text("");

        let l_coolpwr_value = self.builder.object::<gtk::Label>("l_coolpwr_value").unwrap();
        l_coolpwr_value.set_text("");
    }

    fn init_cam_widgets(&self) {
        let spb_exp = self.builder.object::<gtk::SpinButton>("spb_exp").unwrap();
        spb_exp.set_range(0.0, 100_000.0);

        let spb_gain = self.builder.object::<gtk::SpinButton>("spb_gain").unwrap();
        spb_gain.set_range(0.0, 1_000_000.0);

        let spb_offset = self.builder.object::<gtk::SpinButton>("spb_offset").unwrap();
        spb_offset.set_range(0.0, 1_000_000.0);
    }

    fn init_raw_widgets(&self) {
        let spb_raw_frames_cnt = self.builder.object::<gtk::SpinButton>("spb_raw_frames_cnt").unwrap();
        spb_raw_frames_cnt.set_range(1.0, 100_000.0);
        spb_raw_frames_cnt.set_digits(0);
        spb_raw_frames_cnt.set_increments(10.0, 100.0);
    }

    fn init_preview_widgets(&self) {
        let scl_dark = self.builder.object::<gtk::Scale>("scl_dark").unwrap();
        scl_dark.set_range(0.0, 1.0);
        scl_dark.set_increments(0.01, 0.1);
        scl_dark.set_round_digits(2);
        scl_dark.set_digits(2);

        let (dpimm_x, _) = gtk_utils::get_widget_dpmm(&self.window)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        scl_dark.set_width_request((40.0 * dpimm_x) as i32);

        let scl_highlight = self.builder.object::<gtk::Scale>("scl_highlight").unwrap();
        scl_highlight.set_range(0.0, 1.0);
        scl_highlight.set_increments(0.01, 0.1);
        scl_highlight.set_round_digits(2);
        scl_highlight.set_digits(2);

        let scl_gamma = self.builder.object::<gtk::Scale>("scl_gamma").unwrap();
        scl_gamma.set_range(1.0, 5.0);
        scl_gamma.set_digits(1);
        scl_gamma.set_increments(0.1, 1.0);
        scl_gamma.set_round_digits(1);
        scl_gamma.set_digits(1);

        let configure_wb_scale = |name: &str| {
            let scale = self.builder.object::<gtk::Scale>(name).unwrap();
            scale.set_range(0.0, 2.0);
            scale.set_increments(0.1, 0.5);
            scale.set_round_digits(1);
            scale.set_digits(1);
        };

        configure_wb_scale("scl_wb_red");
        configure_wb_scale("scl_wb_green");
        configure_wb_scale("scl_wb_blue");
    }

    fn init_live_stacking_widgets(&self) {
        let spb_live_minutes = self.builder.object::<gtk::SpinButton>("spb_live_minutes").unwrap();
        spb_live_minutes.set_range(1.0, 60.0);
        spb_live_minutes.set_digits(0);
        spb_live_minutes.set_increments(1.0, 10.0);
    }

    fn init_frame_quality_widgets(&self) {
        let spb_max_fwhm = self.builder.object::<gtk::SpinButton>("spb_max_fwhm").unwrap();
        spb_max_fwhm.set_range(1.0, 100.0);
        spb_max_fwhm.set_digits(1);
        spb_max_fwhm.set_increments(0.1, 1.0);

        let spb_max_oval = self.builder.object::<gtk::SpinButton>("spb_max_oval").unwrap();
        spb_max_oval.set_range(0.2, 2.0);
        spb_max_oval.set_digits(1);
        spb_max_oval.set_increments(0.1, 1.0);
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
        let bldr = &self.builder;
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        gtk_utils::connect_action   (&self.window, self, "take_shot",              Self::handler_action_take_shot);
        gtk_utils::connect_action   (&self.window, self, "stop_shot",              Self::handler_action_stop_shot);
        gtk_utils::connect_action   (&self.window, self, "clear_light_history",    Self::handler_action_clear_light_history);
        gtk_utils::connect_action   (&self.window, self, "start_save_raw_frames",  Self::handler_action_start_save_raw_frames);
        gtk_utils::connect_action   (&self.window, self, "stop_save_raw_frames",   Self::handler_action_stop_save_raw_frames);
        gtk_utils::connect_action   (&self.window, self, "continue_save_raw",      Self::handler_action_continue_save_raw_frames);
        gtk_utils::connect_action   (&self.window, self, "start_live_stacking",    Self::handler_action_start_live_stacking);
        gtk_utils::connect_action   (&self.window, self, "stop_live_stacking",     Self::handler_action_stop_live_stacking);
        gtk_utils::connect_action   (&self.window, self, "continue_live_stacking", Self::handler_action_continue_live_stacking);
        gtk_utils::connect_action_rc(&self.window, self, "load_image",             Self::handler_action_open_image);
        gtk_utils::connect_action   (&self.window, self, "save_image_preview",     Self::handler_action_save_image_preview);
        gtk_utils::connect_action   (&self.window, self, "save_image_linear",      Self::handler_action_save_image_linear);
        gtk_utils::connect_action   (&self.window, self, "dark_library",           Self::handler_action_darks_library);

        let cb_camera_list = bldr.object::<gtk::ComboBoxText>("cb_camera_list").unwrap();
        cb_camera_list.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let Some(cur_id) = cb.active_id() else { return; };
            let new_device = DeviceAndProp::new(&cur_id);
            if options.cam.device.as_ref() == Some(&new_device) {
                return;
            }

            // Store previous camera options into UiOptions::all_cam_opts

            if let Some(prev_cam) = options.cam.device.clone() {
                options.read_focuser_cam(&self_.builder);
                options.read_guiding_cam(&self_.builder);
                self_.store_cur_cam_options_impl(&prev_cam, &options);
            }

            // Copy some options for specific camera from UiOptions::all_cam_opts

            self_.select_options_for_camera(&new_device, &mut options);

            // Assign new camera name

            options.cam.device = Some(new_device.clone());

            _ = self_.update_resolution_list_impl(&new_device, &options);
            self_.fill_heater_items_list_impl(&options);
            self_.show_total_raw_time_impl(&options);

            // Show some options for specific camera

            options.show_cam_frame(&self_.builder);
            options.show_calibr(&self_.builder);
            options.show_cam_ctrl(&self_.builder);

            drop(options);

            self_.correct_widgets_props_impl(&Some(new_device.clone()));
            self_.correct_frame_quality_widgets_props();

            self_.core.event_subscriptions().notify(Event::CameraDeviceChanged(new_device));
        }));

        let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
        cb_frame_mode.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let frame_type = FrameType::from_active_id(cb.active_id().as_deref());
            options.cam.frame.frame_type = frame_type;
            drop(options);
            self_.correct_widgets_props();
        }));

        let chb_cooler = bldr.object::<gtk::CheckButton>("chb_cooler").unwrap();
        chb_cooler.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.ctrl.enable_cooler = chb.is_active();
            drop(options);
            self_.control_camera_by_options(false);
            self_.correct_widgets_props();
        }));

        let cb_cam_heater = bldr.object::<gtk::ComboBoxText>("cb_cam_heater").unwrap();
        cb_cam_heater.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.ctrl.heater_str = cb.active_id().map(|id| id.to_string());
            drop(options);
            self_.control_camera_by_options(false);
            self_.correct_widgets_props();
        }));

        let chb_fan = bldr.object::<gtk::CheckButton>("chb_fan").unwrap();
        chb_fan.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.ctrl.enable_fan = chb.is_active();
            drop(options);
            self_.control_camera_by_options(false);
            self_.correct_widgets_props();
        }));

        let spb_temp = bldr.object::<gtk::SpinButton>("spb_temp").unwrap();
        spb_temp.connect_value_changed(clone!(@weak self as self_ => move |spb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.ctrl.temperature = spb.value();
            drop(options);
            self_.control_camera_by_options(false);
        }));

        let chb_shots_cont = bldr.object::<gtk::CheckButton>("chb_shots_cont").unwrap();
        chb_shots_cont.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.get_options_from_widgets();
            self_.correct_widgets_props();
            self_.handler_live_view_changed();
        }));

        let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
        cb_frame_mode.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let frame_type = FrameType::from_active_id(cb.active_id().as_deref());
            options.cam.frame.frame_type = frame_type;
            ui.set_prop_f64("spb_exp.value", options.cam.frame.exposure());
            drop(options);
            self_.show_total_raw_time();
        }));

        let spb_exp = bldr.object::<gtk::SpinButton>("spb_exp").unwrap();
        spb_exp.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.frame.set_exposure(sb.value());
            drop(options);
            self_.show_total_raw_time();
        }));

        let spb_gain = bldr.object::<gtk::SpinButton>("spb_gain").unwrap();
        spb_gain.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.frame.gain = sb.value();
        }));

        let spb_offset = bldr.object::<gtk::SpinButton>("spb_offset").unwrap();
        spb_offset.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.frame.offset = sb.value() as i32;
        }));

        let cb_bin = bldr.object::<gtk::ComboBoxText>("cb_bin").unwrap();
        cb_bin.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let binning = Binning::from_active_id(cb.active_id().as_deref());
            options.cam.frame.binning = binning;
        }));

        let cb_crop = bldr.object::<gtk::ComboBoxText>("cb_crop").unwrap();
        cb_crop.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let crop = Crop::from_active_id(cb.active_id().as_deref());
            options.cam.frame.crop = crop;
        }));

        let chb_low_noise = bldr.object::<gtk::CheckButton>("chb_low_noise").unwrap();
        chb_low_noise.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.cam.frame.low_noise = chb.is_active();
        }));

        let spb_raw_frames_cnt = bldr.object::<gtk::SpinButton>("spb_raw_frames_cnt").unwrap();
        spb_raw_frames_cnt.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.raw_frames.frame_cnt = sb.value() as usize;
            drop(options);
            self_.show_total_raw_time();
        }));

        let da_shot_state = bldr.object::<gtk::DrawingArea>("da_shot_state").unwrap();
        da_shot_state.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |area, cr| {
                self_.handler_draw_shot_state(area, cr);
                glib::Propagation::Proceed
            })
        );

        let cb_preview_src = bldr.object::<gtk::ComboBoxText>("cb_preview_src").unwrap();
        cb_preview_src.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let source = PreviewSource::from_active_id(cb.active_id().as_deref());
            options.preview.source = source;
            drop(options);
            self_.create_and_show_preview_image();
            self_.repaint_histogram();
            self_.show_histogram_stat();
            self_.show_image_info();
        }));

        let sw_preview_img = self.builder.object::<gtk::Widget>("sw_preview_img").unwrap();
        sw_preview_img.connect_size_allocate(clone!(@weak self as self_ => move |_, rect| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.widget_width = rect.width() as usize;
            options.preview.widget_height = rect.height() as usize;
        }));

        let cb_preview_scale = bldr.object::<gtk::ComboBoxText>("cb_preview_scale").unwrap();
        cb_preview_scale.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let scale = PreviewScale::from_active_id(cb.active_id().as_deref());
            options.preview.scale = scale;
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let cb_preview_color = bldr.object::<gtk::ComboBoxText>("cb_preview_color").unwrap();
        cb_preview_color.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            let color = PreviewColor::from_active_id(cb.active_id().as_deref());
            options.preview.color = color;
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let scl_dark = bldr.object::<gtk::Scale>("scl_dark").unwrap();
        scl_dark.connect_value_changed(clone!(@weak self as self_ => move |scl| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.dark_lvl = scl.value();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let scl_highlight = bldr.object::<gtk::Scale>("scl_highlight").unwrap();
        scl_highlight.connect_value_changed(clone!(@weak self as self_ => move |scl| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.light_lvl = scl.value();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let scl_gamma = bldr.object::<gtk::Scale>("scl_gamma").unwrap();
        scl_gamma.connect_value_changed(clone!(@weak self as self_ => move |scl| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.gamma = scl.value();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let chb_rem_grad = bldr.object::<gtk::CheckButton>("chb_rem_grad").unwrap();
        chb_rem_grad.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.preview.remove_grad = chb.is_active();
            drop(options);
            self_.create_and_show_preview_image();
        }));

        let da_histogram = bldr.object::<gtk::DrawingArea>("da_histogram").unwrap();
        da_histogram.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |area, cr| {
                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.handler_draw_histogram(area, cr)?;
                    Ok(())
                });
                glib::Propagation::Proceed
            })
        );

        let ch_hist_logy = bldr.object::<gtk::CheckButton>("ch_hist_logy").unwrap();
        ch_hist_logy.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
            ui_options.hist_log_y = chb.is_active();
            self_.repaint_histogram();
        }));

        let ch_stat_percents = bldr.object::<gtk::CheckButton>("ch_stat_percents").unwrap();
        ch_stat_percents.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
            ui_options.hist_percents = chb.is_active();
            drop(ui_options);
            self_.show_histogram_stat();
        }));

        let chb_max_fwhm = bldr.object::<gtk::CheckButton>("chb_max_fwhm").unwrap();
        chb_max_fwhm.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.quality.use_max_fwhm = chb.is_active();
            drop(options);
            self_.correct_frame_quality_widgets_props();
        }));

        let chb_max_oval = bldr.object::<gtk::CheckButton>("chb_max_oval").unwrap();
        chb_max_oval.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.quality.use_max_ovality = chb.is_active();
            self_.correct_frame_quality_widgets_props();
        }));

        let spb_max_fwhm = bldr.object::<gtk::SpinButton>("spb_max_fwhm").unwrap();
        spb_max_fwhm.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.quality.max_fwhm = sb.value() as f32;
        }));

        let spb_max_oval = bldr.object::<gtk::SpinButton>("spb_max_oval").unwrap();
        spb_max_oval.connect_value_changed(clone!(@weak self as self_ => move |sb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.quality.max_ovality = sb.value() as f32;
        }));

        let chb_master_dark = bldr.object::<gtk::CheckButton>("chb_master_dark").unwrap();
        chb_master_dark.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.calibr.dark_frame_en = chb.is_active();
        }));

        let chb_master_flat = bldr.object::<gtk::CheckButton>("chb_master_flat").unwrap();
        chb_master_flat.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.calibr.flat_frame_en = chb.is_active();
        }));

        let fch_master_flat = bldr.object::<gtk::FileChooserButton>("fch_master_flat").unwrap();
        fch_master_flat.connect_file_set(clone!(@weak self as self_ => move |fch| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.calibr.flat_frame_fname = fch.filename();
        }));

        let chb_hot_pixels = bldr.object::<gtk::CheckButton>("chb_hot_pixels").unwrap();
        chb_hot_pixels.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let l_hot_pixels_warn = self_.builder.object::<gtk::Label>("l_hot_pixels_warn").unwrap();
            l_hot_pixels_warn.set_visible(chb.is_active());
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.calibr.hot_pixels = chb.is_active();
            drop(options);
        }));

        let chb_flat_percents = bldr.object::<gtk::CheckButton>("chb_flat_percents").unwrap();
        chb_flat_percents.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let Ok(mut ui_options) = self_.ui_options.try_borrow_mut() else { return; };
            ui_options.flat_percents = chb.is_active();
            drop(ui_options);
            self_.show_flat_info();
        }));

        let chb_live_no_tracks = bldr.object::<gtk::CheckButton>("chb_live_no_tracks").unwrap();
        chb_live_no_tracks.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            let l_live_no_tracks = self_.builder.object::<gtk::Label>("l_live_no_tracks").unwrap();
            l_live_no_tracks.set_visible(chb.is_active());
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.live.remove_tracks = chb.is_active();
            drop(options);

        }));

    }

    fn connect_main_ui_events(self: &Rc<Self>, handlers: &mut MainUiEventHandlers) {
        handlers.subscribe(clone!(@weak self as self_ => move |event| {
            self_.handler_main_ui_event(event);
        }));
    }

    fn handler_main_ui_event(&self, event: MainUiEvent) {
        match event {
            MainUiEvent::Timer => {}
            MainUiEvent::FullScreen(full_screen) =>
                self.set_full_screen_mode(full_screen),
            MainUiEvent::BeforeModeContinued =>
                self.get_options_from_widgets(),
            MainUiEvent::TabPageChanged(TabPage::Camera) =>
                self.correct_widgets_props(),
            MainUiEvent::ProgramClosing =>
                self.handler_closing(),
            MainUiEvent::BeforeDisconnect => {
                self.get_options_from_widgets();
                self.store_cur_cam_options();
            },
            _ => {},
        }
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


            MainThreadEvent::Core(Event::FrameProcessing(result)) => {
                self.show_frame_processing_result(result);
            },

            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_widgets_props();
                self.correct_preview_source();
            },

            MainThreadEvent::Core(Event::ModeContinued) => {
                let options = self.options.read().unwrap();
                options.show_cam_frame(&self.builder);
                drop(options);
                self.correct_preview_source();
            },

            _ => {},
        }
    }

    fn store_cur_cam_options_impl(
        &self,
        device:  &DeviceAndProp,
        options: &Options
    ) {
        let mut ui_options = self.ui_options.borrow_mut();
        let store_dest = match ui_options.all_cam_opts.iter_mut().find(|item| item.cam == *device) {
            Some(existing) => existing,
            _ => {
                let mut new_cam_opts = StoredCamOptions::default();
                new_cam_opts.cam = device.clone();
                ui_options.all_cam_opts.push(new_cam_opts);
                ui_options.all_cam_opts.last_mut().unwrap()
            }
        };

        store_dest.frame = options.cam.frame.clone();
        store_dest.ctrl = options.cam.ctrl.clone();
        store_dest.calibr = options.calibr.clone();
    }

    fn select_options_for_camera(
        &self,
        camera_device: &DeviceAndProp,
        options:       &mut Options
    ) {
        // Restore previous options of selected camera
        let ui_options = self.ui_options.borrow();
        if let Some(stored) = ui_options.all_cam_opts.iter().find(|item| &item.cam == camera_device) {
            options.cam.frame = stored.frame.clone();
            options.cam.ctrl = stored.ctrl.clone();
            options.calibr = stored.calibr.clone();
        }
        drop(ui_options);
    }

    fn connect_img_mouse_scroll_events(self: &Rc<Self>) {
        let eb_preview_img = self.builder.object::<gtk::EventBox>("eb_preview_img").unwrap();
        let sw_preview_img = self.builder.object::<gtk::ScrolledWindow>("sw_preview_img").unwrap();

        eb_preview_img.connect_button_press_event(
            clone!(@weak self as self_, @weak sw_preview_img => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                    let hadjustment = sw_preview_img.hadjustment();
                    let vadjustment = sw_preview_img.vadjustment();
                    *self_.preview_scroll_pos.borrow_mut() = Some((
                        evt.root(),
                        (hadjustment.value(), vadjustment.value())
                    ));
                }
                glib::Propagation::Proceed
            })
        );

        eb_preview_img.connect_button_release_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                    *self_.preview_scroll_pos.borrow_mut() = None;
                }
                glib::Propagation::Proceed
            })
        );

        eb_preview_img.connect_motion_notify_event(
            clone!(@weak self as self_, @weak sw_preview_img => @default-return glib::Propagation::Proceed,
            move |_, evt| {
                const SCROLL_SPEED: f64 = 2.0;
                if let Some((start_mouse_pos, start_scroll_pos)) = *self_.preview_scroll_pos.borrow() {
                    let new_pos = evt.root();
                    let move_x = new_pos.0 - start_mouse_pos.0;
                    let move_y = new_pos.1 - start_mouse_pos.1;
                    let hadjustment = sw_preview_img.hadjustment();
                    hadjustment.set_value(start_scroll_pos.0 - SCROLL_SPEED*move_x);
                    let vadjustment = sw_preview_img.vadjustment();
                    vadjustment.set_value(start_scroll_pos.1 - SCROLL_SPEED*move_y);
                }
                glib::Propagation::Proceed
            })
        );
    }

    fn set_full_screen_mode(&self, full_screen: bool) {
        let bldr = &self.builder;
        let bx_cam_left = bldr.object::<gtk::Widget>("bx_cam_left").unwrap();
        let scr_cam_right = bldr.object::<gtk::Widget>("scr_cam_right").unwrap();
        let pan_cam3 = bldr.object::<gtk::Widget>("pan_cam3").unwrap();
        let bx_img_info = bldr.object::<gtk::Widget>("bx_img_info").unwrap();
        if full_screen {
            self.get_ui_options_from_widgets();
            bx_cam_left.set_visible(false);
            scr_cam_right.set_visible(false);
            pan_cam3.set_visible(false);
            bx_img_info.set_visible(false);
        } else {
            bx_cam_left.set_visible(true);
            scr_cam_right.set_visible(true);
            pan_cam3.set_visible(true);
            bx_img_info.set_visible(true);
        }
        self.full_screen_mode.set(full_screen);
        let options = self.options.read().unwrap();
        if options.preview.scale == PreviewScale::FitWindow {
            drop(options);
            gtk::main_iteration_do(true);
            gtk::main_iteration_do(true);
            gtk::main_iteration_do(true);
            self.create_and_show_preview_image();
        }
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        _ = self.core.stop_img_process_thread();

        _ = self.core.abort_active_mode();

        self.get_options_from_widgets();

        self.get_ui_options_from_widgets();
        self.store_cur_cam_options();

        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }

        *self.self_.borrow_mut() = None;
    }

    /// Stores current camera options for current camera
    fn store_cur_cam_options(&self) {
        let options = self.options.read().unwrap();
        if let Some(cur_cam_device) = &options.cam.device {
            self.store_cur_cam_options_impl(&cur_cam_device, &options);
        }
    }

    fn show_options(&self) {
        let options = self.options.read().unwrap();
        options.show_cam(&self.builder);
        options.show_raw(&self.builder);
        options.show_live_stacking(&self.builder);
        options.show_frame_quality(&self.builder);
        options.show_preview(&self.builder);
        options.show_guiding(&self.builder);
    }

    fn show_ui_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let bld = &self.builder;
        let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
        let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
        let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();
        let pan_cam4 = bld.object::<gtk::Paned>("pan_cam4").unwrap();

        let options = self.ui_options.borrow();
        pan_cam1.set_position(options.paned_pos1);
        if options.paned_pos2 != -1 {
            pan_cam2.set_position(pan_cam2.allocation().width()-options.paned_pos2);
        }
        pan_cam3.set_position(options.paned_pos3);
        if options.paned_pos4 != -1 {
            pan_cam4.set_position(pan_cam4.allocation().height()-options.paned_pos4);
        }
        ui.set_prop_bool("exp_cam_ctrl.expanded",   options.cam_ctrl_exp);
        ui.set_prop_bool("exp_shot_set.expanded",   options.shot_exp);
        ui.set_prop_bool("exp_calibr.expanded",     options.calibr_exp);
        ui.set_prop_bool("exp_raw_frames.expanded", options.raw_frames_exp);
        ui.set_prop_bool("exp_live.expanded",       options.live_exp);
        ui.set_prop_bool("exp_quality.expanded",    options.quality_exp);
        ui.set_prop_bool("ch_hist_logy.active",     options.hist_log_y);
        ui.set_prop_bool("ch_stat_percents.active", options.hist_percents);
        ui.set_prop_bool("chb_flat_percents.active", options.flat_percents);
    }

    fn get_options_from_widgets(&self) {
        let Ok(mut options) = self.options.try_write() else { return; };
        options.read_all(&self.builder);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let bld = &self.builder;
        let mut options = self.ui_options.borrow_mut();
        if !self.full_screen_mode.get() {
            let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
            let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
            let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();
            let pan_cam4 = bld.object::<gtk::Paned>("pan_cam4").unwrap();
            options.paned_pos1 = pan_cam1.position();
            options.paned_pos2 = pan_cam2.allocation().width()-pan_cam2.position();
            options.paned_pos3 = pan_cam3.position();
            options.paned_pos4 = pan_cam4.allocation().height()-pan_cam4.position();
        }
        options.cam_ctrl_exp   = ui.prop_bool("exp_cam_ctrl.expanded");
        options.shot_exp       = ui.prop_bool("exp_shot_set.expanded");
        options.calibr_exp     = ui.prop_bool("exp_calibr.expanded");
        options.raw_frames_exp = ui.prop_bool("exp_raw_frames.expanded");
        options.live_exp       = ui.prop_bool("exp_live.expanded");
        options.quality_exp    = ui.prop_bool("exp_quality.expanded");
        options.hist_log_y     = ui.prop_bool("ch_hist_logy.active");
        options.hist_percents  = ui.prop_bool("ch_stat_percents.active");
        options.flat_percents = ui.prop_bool("chb_flat_percents.active");
    }

    fn correct_preview_source(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mode_type = self.core.mode_data().mode.get_type();
        let cb_preview_src_aid = match mode_type {
            ModeType::LiveStacking => "live",
            ModeType::Waiting      => return,
            _                      => "frame",
        };
        ui.set_prop_str("cb_preview_src.active-id", Some(cb_preview_src_aid));
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
            }
            DelayedAction::SelectMaxResolution => {
                self.select_maximum_resolution();
            }
            DelayedAction::FillHeaterItems => {
                self.fill_heater_items_list();
            }
        }
    }

    fn correct_widgets_props_impl(&self, camera: &Option<DeviceAndProp>) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let temp_supported = camera.as_ref().map(|camera| {
            let temp_value = self.indi.camera_get_temperature_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&self.builder, "spb_temp", &temp_value, 0, Some(1.0))
        }).unwrap_or(false);
        let exposure_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&camera.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&self.builder, "spb_exp", &exp_value, 3, Some(1.0))
        }).unwrap_or(false);
        let gain_supported = camera.as_ref().map(|camera| {
            let gain_value = self.indi.camera_get_gain_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&self.builder, "spb_gain", &gain_value, 0, None)
        }).unwrap_or(false);
        let offset_supported = camera.as_ref().map(|camera| {
            let offset_value = self.indi.camera_get_offset_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&self.builder, "spb_offset", &offset_value, 0, None)
        }).unwrap_or(false);
        let bin_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            self.indi.camera_is_binning_supported(&camera.name, cam_ccd).unwrap_or(false)
        }).unwrap_or(false);
        let fan_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_fan_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let heater_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_heater_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let low_noise_supported = camera.as_ref().map(|camera|
            self.indi.camera_is_low_noise_ctrl_supported(&camera.name).unwrap_or(false)
        ).unwrap_or(false);
        let crop_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            self.indi.camera_is_frame_supported(&camera.name, cam_ccd).unwrap_or(false)
        }).unwrap_or(false);

        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let cooler_active = ui.prop_bool("chb_cooler.active");
        let frame_mode_str = ui.prop_string("cb_frame_mode.active-id");
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
        ui.set_prop_str("btn_start_save_raw.label", Some(save_raw_btn_cap));

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

        gtk_utils::enable_actions(&self.window, &[
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

        ui.show_widgets(&[
            ("chb_fan",       fan_supported),
            ("l_cam_heater",  heater_supported),
            ("cb_cam_heater", heater_supported),
            ("chb_low_noise", low_noise_supported),
        ]);

        ui.enable_widgets(false, &[
            ("l_camera_list",      waiting && indi_connected),
            ("cb_camera_list",     waiting && indi_connected),
            ("chb_fan",            !cooler_active),
            ("chb_cooler",         temp_supported && can_change_cam_opts),
            ("spb_temp",           cooler_active && temp_supported && can_change_cam_opts),
            ("chb_shots_cont",     (exposure_supported && liveview_active) || can_change_mode),
            ("cb_frame_mode",      can_change_frame_opts),
            ("spb_exp",            exposure_supported && can_change_frame_opts),
            ("cb_crop",            crop_supported && can_change_frame_opts),
            ("spb_gain",           gain_supported && can_change_frame_opts),
            ("spb_offset",         offset_supported && can_change_frame_opts),
            ("cb_bin",             bin_supported && can_change_frame_opts),
            ("chb_master_frame",   can_change_cal_ops && (frame_mode_is_flat || frame_mode_is_dark) && !saving_frames),
            ("chb_master_dark",    can_change_cal_ops),
            ("fch_dark_library",   can_change_cal_ops),
            ("chb_master_flat",    can_change_cal_ops),
            ("fch_master_flat",    can_change_cal_ops),
            ("chb_raw_frames_cnt", !saving_frames && can_change_mode),
            ("spb_raw_frames_cnt", !saving_frames && can_change_mode),

            ("chb_live_save",      can_change_live_stacking_opts),
            ("spb_live_minutes",   can_change_live_stacking_opts),
            ("chb_live_save_orig", can_change_live_stacking_opts),
            ("fch_live_folder",    can_change_live_stacking_opts),

            ("grd_cam_ctrl",       cam_sensitive),
            ("grd_shot_settings",  cam_sensitive),
            ("grd_save_raw",       cam_sensitive),
            ("grd_live_stack",     cam_sensitive),
            ("grd_cam_calibr",     cam_sensitive),
            ("bx_light_qual",      cam_sensitive),
        ]);
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let camera = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(&camera);
        self.correct_frame_quality_widgets_props();
    }

    fn correct_frame_quality_widgets_props(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.enable_widgets(true, &[
            ("spb_max_fwhm", ui.prop_bool("chb_max_fwhm.active")),
            ("spb_max_oval", ui.prop_bool("chb_max_oval.active")),
        ]);
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

        let cb = self.builder.object::<gtk::ComboBoxText>("cb_camera_list").unwrap();

        let connected = self.indi.state() == indi::ConnState::Connected;

        let camera_selected = fill_devices_list_into_combobox(
            &list,
            &cb,
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
                self.select_options_for_camera(&cur_cam_device, &mut options);
                drop(options);

                let options = self.options.read().unwrap();
                options.show_cam_frame(&self.builder);
                options.show_calibr(&self.builder);
                options.show_cam_ctrl(&self.builder);
            }

            self.correct_widgets_props();
        }
    }

    fn update_resolution_list_impl(
        &self,
        cam_dev: &DeviceAndProp,
        options: &Options
    ) {
        let cb_bin = self.builder.object::<gtk::ComboBoxText>("cb_bin").unwrap();
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
        gtk_utils::exec_and_show_error(&self.window, ||{
            let cb_cam_heater = self.builder.object::<gtk::ComboBoxText>("cb_cam_heater").unwrap();
            let last_heater_value = cb_cam_heater.active_id();
            cb_cam_heater.remove_all();
            let Some(device) = &options.cam.device else { return Ok(()); };
            if device.name.is_empty() { return Ok(()); };
            if !self.indi.camera_is_heater_supported(&device.name)? { return Ok(()) }
            let Some(items) = self.indi.camera_get_heater_items(&device.name)? else { return Ok(()); };
            for (id, label) in items {
                cb_cam_heater.append(Some(id.as_str()), &label);
            }
            if last_heater_value.is_some() {
                cb_cam_heater.set_active_id(last_heater_value.as_deref());
            } else {
                cb_cam_heater.set_active_id(options.cam.ctrl.heater_str.as_deref());
            }
            if cb_cam_heater.active_id().is_none() {
                cb_cam_heater.set_active(Some(0));
            }
            Ok(())
        });
    }

    fn select_maximum_resolution(&self) { // TODO: move to Core
        let options = self.options.read().unwrap();
        let Some(device) = &options.cam.device else { return; };
        let cam_name = &device.name;
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
        self.get_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_live_view()?;
            Ok(())
        });
    }

    fn handler_action_take_shot(&self) {
        self.get_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_single_shot()?;
            Ok(())
        });
    }

    fn handler_action_stop_shot(&self) {
        self.core.abort_active_mode();
    }

    fn create_and_show_preview_image(&self) {
        let options = self.options.read().unwrap();
        let preview_params = options.preview.preview_params();
        let (image, hist) = match options.preview.source {
            PreviewSource::OrigFrame =>
                (&*self.core.cur_frame().image, &self.core.cur_frame().img_hist),
            PreviewSource::LiveStacking =>
                (&self.core.live_stacking().image, &self.core.live_stacking().hist),
        };
        drop(options);
        let image = image.read().unwrap();
        let hist = hist.read().unwrap();
        let rgb_bytes = get_rgb_data_from_preview_image(
            &image,
            &hist,
            &preview_params
        );
        if let Some(rgb_bytes) = rgb_bytes {
            self.show_preview_image(Some(&rgb_bytes), None);
        } else {
            self.show_preview_image(None, None);
        }
    }

    fn show_preview_image(
        &self,
        rgb_bytes:  Option<&RgbU8Data>,
        src_params: Option<&PreviewParams>,
    ) {
        let preview_options = self.options.read().unwrap().preview.clone();
        let pp = preview_options.preview_params();
        if src_params.is_some() && src_params != Some(&pp) {
            self.create_and_show_preview_image();
            return;
        }

        let img_preview = self.builder.object::<gtk::Image>("img_preview").unwrap();

        let mut is_color_image = false;
        if let Some(rgb_bytes) = rgb_bytes {
            let tmr = TimeLogger::start();
            let bytes = glib::Bytes::from_owned(rgb_bytes.bytes.clone());
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

            let (img_width, img_height) = pp.img_size.get_preview_img_size(
                rgb_bytes.orig_width,
                rgb_bytes.orig_height
            );
            if (img_width != rgb_bytes.width || img_height != rgb_bytes.height)
            && img_width > 42 && img_height > 42 {
                let tmr = TimeLogger::start();
                pixbuf = pixbuf.scale_simple(
                    img_width as _,
                    img_height as _,
                    gtk::gdk_pixbuf::InterpType::Tiles,
                ).unwrap();
                tmr.log("Pixbuf::scale_simple");
            }
            img_preview.set_pixbuf(Some(&pixbuf));
            is_color_image = rgb_bytes.is_color_image;
        } else {
            img_preview.clear();
            img_preview.set_pixbuf(None);
        }

        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.enable_widgets(
            false,
            &[("cb_preview_color", is_color_image)]
        );
    }

    fn show_image_info(&self) {
        let info = match self.options.read().unwrap().preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().info.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().info.read().unwrap(),
        };

        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let update_info_panel_vis = |is_light_info: bool, is_flat_info: bool, is_raw_info: bool| {
            ui.show_widgets(&[
                ("bx_light_info", is_light_info),
                ("bx_flat_info",  is_flat_info),
                ("bx_raw_info",   is_raw_info),
            ]);
        };

        match &*info {
            ResultImageInfo::LightInfo(info) => {
                ui.set_prop_str("e_info_exp.text", Some(&seconds_to_total_time_str(info.exposure, true)));
                match info.stars.fwhm {
                    Some(value) => ui.set_prop_str("e_fwhm.text", Some(&format!("{:.1}", value))),
                    None        => ui.set_prop_str("e_fwhm.text", Some("")),
                }
                match info.stars.ovality {
                    Some(value) => ui.set_prop_str("e_ovality.text", Some(&format!("{:.1}", value))),
                    None        => ui.set_prop_str("e_ovality.text", Some("")),
                }
                let stars_cnt = info.stars.items.len();
                let overexp_stars = info.stars.items.iter().filter(|s| s.overexposured).count();
                ui.set_prop_str("e_stars.text", Some(&format!("{} ({})", stars_cnt, overexp_stars)));
                let bg = 100_f64 * info.background as f64 / info.max_value as f64;
                ui.set_prop_str("e_background.text", Some(&format!("{:.2}%", bg)));
                let noise = 100_f64 * info.noise as f64 / info.max_value as f64;
                ui.set_prop_str("e_noise.text", Some(&format!("{:.4}%", noise)));
                update_info_panel_vis(true, false, false);
            },
            ResultImageInfo::FlatInfo(info) => {
                *self.flat_info.borrow_mut() = info.clone();
                update_info_panel_vis(false, true, false);
                self.show_flat_info();
            },
            ResultImageInfo::RawInfo(info) => {
                let aver_text = format!(
                    "{:.1} ({:.1}%)",
                    info.aver,
                    100.0 * info.aver / info.max_value as f32
                );
                ui.set_prop_str("e_aver.text", Some(&aver_text));
                let median_text = format!(
                    "{} ({:.1}%)",
                    info.median,
                    100.0 * info.median as f64 / info.max_value as f64
                );
                ui.set_prop_str("e_median.text", Some(&median_text));
                let dev_text = format!(
                    "{:.1} ({:.3}%)",
                    info.std_dev,
                    100.0 * info.std_dev / info.max_value as f32
                );
                ui.set_prop_str("e_std_dev.text", Some(&dev_text));
                update_info_panel_vis(false, false, true);
            },
            _ => {
                update_info_panel_vis(false, false, false);
            },
        }
    }

    fn show_flat_info(&self) {
        let info = self.flat_info.borrow();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let ui_options = self.ui_options.borrow();
        let show_chan = |label_id, entry_id, item: Option<&FlatInfoChan>| {
            if let Some(item) = item {
                let text =
                    if ui_options.flat_percents {
                        let percent_aver = 100.0 * item.aver / info.max_value as f32;
                        let percent_max = 100.0 * item.max as f64 / info.max_value as f64;
                        format!("{:.1}% / {:.1}%", percent_aver, percent_max)
                    } else {
                        format!("{:.1} / {}", item.aver, item.max)
                    };
                ui.set_prop_str_ex(entry_id, "text", Some(&text));
            }
            ui.show_widgets(&[
                (label_id, item.is_some()),
                (entry_id, item.is_some()),
            ]);
        };

        show_chan("l_flat_r", "e_flat_r", info.r.as_ref());
        show_chan("l_flat_g", "e_flat_g", info.g.as_ref());
        show_chan("l_flat_b", "e_flat_b", info.b.as_ref());
        show_chan("l_flat_l", "e_flat_l", info.l.as_ref());
    }

    fn handler_action_save_image_preview(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            let options = self.options.read().unwrap();
            let (image, hist, fn_prefix) = match options.preview.source {
                PreviewSource::OrigFrame =>
                    (&*self.core.cur_frame().image, &self.core.cur_frame().img_hist, "preview"),
                PreviewSource::LiveStacking =>
                    (&self.core.live_stacking().image, &self.core.live_stacking().hist, "live"),
            };
            if image.read().unwrap().is_empty() { return Ok(()); }
            let mut preview_options = options.preview.clone();
            preview_options.scale = PreviewScale::Original;
            drop(options);
            let def_file_name = format!(
                "{}_{}.jpg",
                fn_prefix,
                Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
            );
            let Some(file_name) = gtk_utils::select_file_name_to_save(
                &self.window,
                "Enter file name to save preview image as jpeg",
                "Jpeg images", "*.jpg",
                "jpg",
                &def_file_name,
            ) else {
                return Ok(());
            };
            let image = image.read().unwrap();
            let hist = hist.read().unwrap();
            let preview_params = preview_options.preview_params();
            let rgb_data = get_rgb_data_from_preview_image(&image, &hist, &preview_params);
            let Some(rgb_data) = rgb_data else { anyhow::bail!("wrong RGB fata"); };
            let bytes = glib::Bytes::from_owned(rgb_data.bytes);
            let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
                &bytes,
                gtk::gdk_pixbuf::Colorspace::Rgb,
                false,
                8,
                rgb_data.width as i32,
                rgb_data.height as i32,
                (rgb_data.width * 3) as i32,
            );
            pixbuf.savev(file_name, "jpeg", &[("quality", "90")])?;
            Ok(())
        });
    }

    fn handler_action_save_image_linear(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            let options = self.options.read().unwrap();
            let preview_source = options.preview.source.clone();
            drop(options);
            let ask_to_select_name = |fn_prefix: &str| -> Option<PathBuf> {
                let def_file_name = format!(
                    "{}_{}.tif",
                    fn_prefix,
                    Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
                );
                gtk_utils::select_file_name_to_save(
                    &self.window,
                    "Enter file name to save preview image as tiff",
                    "Tiff images", "*.tif",
                    "tif",
                    &def_file_name,
                )
            };
            match preview_source {
                PreviewSource::OrigFrame => {
                    let image = &self.core.cur_frame().image;
                    if image.read().unwrap().is_empty() {
                        return Ok(());
                    }
                    let Some(file_name) = ask_to_select_name("preview") else {
                        return Ok(())
                    };
                    let image = image.read().unwrap();
                    save_image_to_tif_file(&image, &file_name)?;
                }
                PreviewSource::LiveStacking => {
                    let stacker = &self.core.live_stacking().stacker;
                    if stacker.read().unwrap().is_empty() {
                        return Ok(());
                    }
                    let Some(file_name) = ask_to_select_name("live") else {
                        return Ok(())
                    };
                    stacker.read().unwrap().save_to_tiff(&file_name)?;
                }
            }
            Ok(())
        });
    }

    fn show_frame_processing_result(
        &self,
        result: FrameProcessResult
    ) {
        let options = self.options.read().unwrap();
        if !result.camera.name.is_empty()
        && options.cam.device != Some(result.camera) {
            return;
        }
        let live_stacking_preview = options.preview.source == PreviewSource::LiveStacking;
        drop(options);
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let show_resolution_info = |width, height| {
            ui.set_prop_str(
                "e_res_info.text",
                Some(&format!("{} x {}", width, height))
            );
        };

        let is_mode_current = |live_result: bool| {
            live_result == live_stacking_preview
        };

        match result.data {
            FrameProcessResultData::ShotProcessingFinished {
                processing_time, blob_dl_time, ..
            } => {
                let perf_str = format!(
                    "Download time = {:.2}s, img. process time = {:.2}s",
                    blob_dl_time, processing_time
                );
                self.main_ui.set_perf_string(perf_str);
            }
            FrameProcessResultData::Error(error_text) => {
                _ = self.core.abort_active_mode();
                self.correct_widgets_props();
                gtk_utils::show_error_message(&self.window, "Fatal Error", &error_text);
            }
            FrameProcessResultData::PreviewFrame(img)
            if is_mode_current(false) => {
                self.show_preview_image(Some(&img.rgb_data), Some(&img.params));
                show_resolution_info(img.image_width, img.image_height);
            }
            FrameProcessResultData::PreviewLiveRes(img)
            if is_mode_current(true) => {
                self.show_preview_image(Some(&img.rgb_data), Some(&img.params));
                show_resolution_info(img.image_width, img.image_height);
            }
            FrameProcessResultData::HistorgamRaw(_)
            if is_mode_current(false) => {
                self.repaint_histogram();
                self.show_histogram_stat();
            }
            FrameProcessResultData::RawFrameInfo(raw_frame_info)
            if is_mode_current(false) => {
                if raw_frame_info.frame_type != FrameType::Lights {
                    let history_item = CalibrHistoryItem {
                        time:           raw_frame_info.time.clone(),
                        mode_type:      result.mode_type,
                        frame_type:     raw_frame_info.frame_type,
                        mean:           raw_frame_info.mean,
                        median:         raw_frame_info.median,
                        std_dev:        raw_frame_info.std_dev,
                        calibr_methods: raw_frame_info.calubr_methods,
                    };
                    self.calibr_history.borrow_mut().push(history_item);
                    self.update_calibr_history_table();
                    self.set_hist_tab_active(Self::HIST_TAB_CALIBR);
                }
            }
            FrameProcessResultData::HistogramLiveRes
            if is_mode_current(true) => {
                self.repaint_histogram();
                self.show_histogram_stat();
            }
            FrameProcessResultData::LightFrameInfo(info) => {
                let history_item = LightHistoryItem {
                    mode_type:      result.mode_type,
                    time:           info.time.clone(),
                    stars_fwhm:     info.stars.fwhm,
                    bad_fwhm:       !info.stars.fwhm_is_ok,
                    stars_ovality:  info.stars.ovality,
                    bad_ovality:    !info.stars.ovality_is_ok,
                    background:     info.bg_percent,
                    noise:          info.raw_noise.map(|n| 100.0 * n / info.max_value as f32),
                    stars_count:    info.stars.items.len(),
                    offset:         info.stars_offset.clone(),
                    bad_offset:     !info.offset_is_ok,
                    calibr_methods: info.calibr_methods.clone(),
                };
                self.light_history.borrow_mut().push(history_item);
                self.update_light_history_table();
                self.set_hist_tab_active(Self::HIST_TAB_LIGHT);
            }
            FrameProcessResultData::FrameInfo
            if is_mode_current(false) => {
                self.show_image_info();
            }
            FrameProcessResultData::FrameInfoLiveRes
            if is_mode_current(true) => {
                self.show_image_info();
            }
            FrameProcessResultData::MasterSaved { frame_type: FrameType::Flats, file_name } => {
                ui.set_fch_path("fch_master_flat", Some(&file_name));
            }
            _ => {}
        }
    }

    // TODO: move camera control code into `core` module
    fn control_camera_by_options(&self, force_set: bool) {
        let options = self.options.read().unwrap();
        let Some(device) = &options.cam.device else { return; };
        let camera_name = &device.name;
        if camera_name.is_empty() { return; };
        gtk_utils::exec_and_show_error(&self.window, || {
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
            if self.indi.camera_is_heater_supported(camera_name)? {
                if let Some(heater_str) = &options.cam.ctrl.heater_str {
                    self.indi.camera_control_heater(
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
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.options.read().unwrap();
        let Some(cur_cam_device) = &options.cam.device else { return; };
        if cur_cam_device.name == device_name {
            ui.set_prop_str(
                "l_temp_value.label",
                Some(&format!("T: {:.1}°C", temparature))
            );
        }
    }

    fn show_coolpwr_value(
        &self,
        device_name: &str,
        pwr_str:     &str
    ) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.options.read().unwrap();
        let Some(cur_cam_device) = &options.cam.device else { return; };
        if cur_cam_device.name == device_name {
            ui.set_prop_str(
                "l_coolpwr_value.label",
                Some(&format!("Pwr: {}", pwr_str))
            );
        }
    }

    fn handler_live_view_changed(&self) {
        if self.indi.state() != indi::ConnState::Connected {
            return;
        }
        if self.options.read().unwrap().cam.live_view {
            self.get_options_from_widgets();
            self.start_live_view();
        } else {
            self.core.abort_active_mode();
        }
    }

    fn process_indi_conn_state_event(&self, conn_state: indi::ConnState) {
        let update_devices_list =
            conn_state == indi::ConnState::Disconnected ||
            conn_state == indi::ConnState::Disconnecting;
        *self.conn_state.borrow_mut() = conn_state;
        if update_devices_list {
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
        if indi::Connection::camera_is_heater_property(prop_name) && new_prop {
            self.delayed_actions.schedule(DelayedAction::FillHeaterItems);
            self.delayed_actions.schedule(DelayedAction::StartCooling);
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

    fn show_histogram_stat(&self) {
        let options = self.options.read().unwrap();
        let hist = match options.preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().raw_hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().hist.read().unwrap(),
        };
        drop(options);
        let ui_options = self.ui_options.borrow();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let max = hist.max as f32;
        let show_chan_data = |chan: &Option<HistogramChan>, l_cap, l_mean, l_median, l_dev| {
            if let Some(chan) = chan.as_ref() {
                let median = chan.median();
                if ui_options.hist_percents {
                    ui.set_prop_str_ex(
                        l_mean, "label",
                        Some(&format!("{:.1}%", 100.0 * chan.mean / max))
                    );
                    ui.set_prop_str_ex(
                        l_median, "label",
                        Some(&format!("{:.1}%", 100.0 * median as f32 / max))
                    );
                    ui.set_prop_str_ex(
                        l_dev, "label",
                        Some(&format!("{:.1}%", 100.0 * chan.std_dev / max))
                    );
                } else {
                    ui.set_prop_str_ex(
                        l_mean, "label",
                        Some(&format!("{:.1}", chan.mean))
                    );
                    ui.set_prop_str_ex(
                        l_median, "label",
                        Some(&format!("{:.1}", median))
                    );
                    ui.set_prop_str_ex(
                        l_dev, "label",
                        Some(&format!("{:.1}", chan.std_dev))
                    );
                }
            }
            ui.show_widgets(&[
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

    fn repaint_histogram(&self) {
        let da_histogram = self.builder.object::<gtk::DrawingArea>("da_histogram").unwrap();
        da_histogram.queue_draw();
    }

    fn handler_draw_histogram(
        &self,
        area: &gtk::DrawingArea,
        cr:   &cairo::Context
    ) ->anyhow::Result<()> {
        let options = self.options.read().unwrap();
        let hist = match options.preview.source {
            PreviewSource::OrigFrame =>
                self.core.cur_frame().raw_hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                self.core.live_stacking().hist.read().unwrap(),
        };
        drop(options);

        let font_size_pt = 8.0;
        let (_, dpmm_y) = gtk_utils::get_widget_dpmm(area)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        let font_size_px = font_size_to_pixels(FontSize::Pt(font_size_pt), dpmm_y);
        cr.set_font_size(font_size_px);

        let ui_options = self.ui_options.borrow();
        draw_histogram(
            &hist,
            area,
            cr,
            area.allocated_width(),
            area.allocated_height(),
            ui_options.hist_log_y,
        )?;
        Ok(())
    }

    fn handler_action_start_live_stacking(&self) {
        if !is_expanded(&self.builder, "exp_live") { return; }
        self.get_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_live_stacking()?;
            self.show_options();
            Ok(())
        });
    }

    fn handler_action_stop_live_stacking(&self) {
        if !is_expanded(&self.builder, "exp_live") { return; }
        self.core.abort_active_mode();
    }

    fn handler_action_continue_live_stacking(&self) {
        if !is_expanded(&self.builder, "exp_live") { return; }
        self.get_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.continue_prev_mode()?;
            Ok(())
        });
    }

    fn update_shot_state(&self) {
        let draw_area = self.builder.object::<gtk::DrawingArea>("da_shot_state").unwrap();
        draw_area.queue_draw();
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
        gtk_utils::exec_and_show_error(&self.window, || {
            draw_progress_bar(area, cr, progress, &text_to_show)
        });
    }

    const HIST_TAB_LIGHT: u32 = 0;
    const HIST_TAB_CALIBR: u32 = 1;
    const HIST_TAB_PLOTS: u32 = 2;

    fn set_hist_tab_active(&self, tab_index: u32) {
        let nb_hist = self.builder.object::<gtk::Notebook>("nb_hist").unwrap();
        if nb_hist.current_page() == Some(Self::HIST_TAB_PLOTS) {
            return;
        }
        nb_hist.set_current_page(Some(tab_index));
    }

    fn mode_type_to_history_str(mode_type: ModeType) -> &'static str {
        match mode_type {
            ModeType::OpeningImgFile    => "File",
            ModeType::SingleShot        => "S",
            ModeType::LiveView          => "LV",
            ModeType::SavingRawFrames   => "RAW",
            ModeType::LiveStacking      => "LS",
            ModeType::Focusing          => "F",
            ModeType::DitherCalibr      => "MC",
            ModeType::Goto|
            ModeType::CapturePlatesolve => "PS",
            ModeType::DefectPixels      => "Pix",
            ModeType::MasterDark|
            ModeType::MasterBias        => "Master",
            _                           => "???",
        }
    }

    fn update_light_history_table(&self) {
        let tree: gtk::TreeView = self.builder.object("tv_light_history").unwrap();
        let model = match tree.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                gtk_utils::init_list_store_model_for_treeview(&tree, &[
                    /* 0 */  ("Mode",       String::static_type(), "text"),
                    /* 1 */  ("Time",       String::static_type(), "text"),
                    /* 2 */  ("FWHM",       String::static_type(), "markup"),
                    /* 3 */  ("Ovality",    String::static_type(), "markup"),
                    /* 4 */  ("Stars",      u32::static_type(),    "text"),
                    /* 5 */  ("Noise",      String::static_type(), "text"),
                    /* 6 */  ("Background", String::static_type(), "text"),
                    /* 7 */  ("Calibr.",    String::static_type(), "text"),
                    /* 8 */  ("Offs.X",     String::static_type(), "markup"),
                    /* 9 */  ("Offs.Y",     String::static_type(), "markup"),
                    /* 10 */ ("Rot.",       String::static_type(), "markup"),
                ])
            },
        };
        let items = self.light_history.borrow();
        if gtk_utils::get_model_row_count(model.upcast_ref()) > items.len() {
            model.clear();
        }
        let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
        let to_index = items.len();
        let make_bad_str = |s: &str| -> String {
            format!(r##"<span color="#FF4040"><b>{}</b></span>"##, s)
        };
        for item in &items[models_row_cnt..to_index] {
            let mode_type_str = Self::mode_type_to_history_str(item.mode_type);
            let local_time_str =
                if let Some(time) = item.time.clone() {
                    let local_time: DateTime<Local> = DateTime::from(time);
                    local_time.format("%H:%M:%S").to_string()
                } else {
                    String::new()
                };
            let mut fwhm_str = item.stars_fwhm
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(String::new);
            if item.bad_fwhm {
                fwhm_str = make_bad_str(&fwhm_str);
            }
            let mut ovality_str = item.stars_ovality
                .map(|v| format!("{:.1}", v))
                .unwrap_or_else(String::new);
            if item.bad_ovality {
                ovality_str = make_bad_str(&ovality_str);
            }
            let stars_cnt = item.stars_count as u32;
            let noise_str = item.noise
                .map(|v| format!("{:.3}%", v))
                .unwrap_or_else(|| "???".to_string());
            let bg_str = format!("{:.1}%", item.background);

            let (x_str, y_str, angle_str) = if let Some(offset) = &item.offset {
                let x_str = if !item.bad_offset {
                    format!("{:.1}", offset.x)
                } else {
                    make_bad_str("???")
                };
                let y_str = if !item.bad_offset {
                    format!("{:.1}", offset.y)
                } else {
                    make_bad_str("???")
                };
                let angle_str = if !item.bad_offset {
                    format!("{:.1}", offset.angle)
                } else {
                    make_bad_str("???")
                };
                (x_str, y_str, angle_str)
            } else {
                (String::new(), String::new(), String::new())
            };
            let calibr_str = Self::calibr_method_to_str(item.calibr_methods);
            let last_is_selected =
                gtk_utils::get_list_view_selected_row(&tree).map(|v| v+1) ==
                Some(models_row_cnt as i32);
            let last_iter = model.insert_with_values(None, &[
                (0, &mode_type_str),
                (1, &local_time_str),
                (2, &fwhm_str),
                (3, &ovality_str),
                (4, &stars_cnt),
                (5, &noise_str),
                (6, &bg_str),
                (7, &calibr_str),
                (8, &x_str),
                (9, &y_str),
                (10, &angle_str),
            ]);
            if last_is_selected || models_row_cnt == 0 {
                // Select and scroll to last row
                tree.selection().select_iter(&last_iter);
                if let [path] = tree.selection().selected_rows().0.as_slice() {
                    tree.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
                }
            }
        }
    }

    fn calibr_method_to_str(cm: CalibrMethods) -> String {
        let mut result = String::new();
        if cm.contains(CalibrMethods::BY_DARK) {
            result += "D";
        }
        if cm.contains(CalibrMethods::BY_BIAS) {
            result += "B";
        }
        if cm.contains(CalibrMethods::DEFECTIVE_PIXELS) {
            result += "P";
        }
        if cm.contains(CalibrMethods::BY_FLAT) {
            result += "F";
        }
        if cm.contains(CalibrMethods::HOT_PIXELS_SEARCH) {
            result += "S";
        }
        result
    }

    fn update_calibr_history_table(&self) {
        let tree: gtk::TreeView = self.builder.object("tv_calbr_history").unwrap();
        let model = match tree.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                gtk_utils::init_list_store_model_for_treeview(&tree, &[
                    /* 0 */  ("Time",     String::static_type(), "text"),
                    /* 1 */  ("Mode",     String::static_type(), "text"),
                    /* 2 */  ("Type",     String::static_type(), "text"),
                    /* 3 */  ("Mean",     String::static_type(), "text"),
                    /* 4 */  ("Median",   String::static_type(), "text"),
                    /* 5 */  ("Std.dev.", String::static_type(), "text"),
                    /* 6 */  ("Calibr.",  String::static_type(), "text"),
                ])
            },
        };
        let items = self.calibr_history.borrow();
        if gtk_utils::get_model_row_count(model.upcast_ref()) > items.len() {
            model.clear();
        }
        let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
        let to_index = items.len();
        for item in &items[models_row_cnt..to_index] {
            let local_time_str =
                if let Some(time) = &item.time {
                    let local_time: DateTime<Local> = DateTime::from(*time);
                    local_time.format("%H:%M:%S").to_string()
                } else {
                    String::new()
                };

            let mode_type_str = Self::mode_type_to_history_str(item.mode_type);
            let type_str = item.frame_type.to_str();
            let mean_str = format!("{:.1}", item.mean);
            let median_str = format!("{}", item.median);
            let dev_str = format!("{:.1}", item.std_dev);
            let calibr_str = Self::calibr_method_to_str(item.calibr_methods);

            let last_is_selected =
                gtk_utils::get_list_view_selected_row(&tree).map(|v| v+1) ==
                Some(models_row_cnt as i32);

            let last_iter = model.insert_with_values(None, &[
                (0, &local_time_str),
                (1, &mode_type_str),
                (2, &type_str),
                (3, &mean_str),
                (4, &median_str),
                (5, &dev_str),
                (6, &calibr_str),
            ]);
            if last_is_selected || models_row_cnt == 0 {
                // Select and scroll to last row
                tree.selection().select_iter(&last_iter);
                if let [path] = tree.selection().selected_rows().0.as_slice() {
                    tree.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
                }
            }
        }
    }

    fn handler_action_start_save_raw_frames(&self) {
        if !is_expanded(&self.builder, "exp_raw_frames") { return; }
        self.get_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_saving_raw_frames()?;
            self.show_options();
            Ok(())
        });
    }

    fn handler_action_continue_save_raw_frames(&self) {
        if !is_expanded(&self.builder, "exp_raw_frames") { return; }
        self.get_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.continue_prev_mode()?;
            Ok(())
        });
    }

    fn handler_action_stop_save_raw_frames(&self) {
        if !is_expanded(&self.builder, "exp_raw_frames") { return; }
        self.core.abort_active_mode();
    }

    fn handler_action_clear_light_history(&self) {
        let nb_hist = self.builder.object::<gtk::Notebook>("nb_hist").unwrap();

        let cur_page = nb_hist.current_page();
        match cur_page {
            Some(Self::HIST_TAB_LIGHT) => {
                self.light_history.borrow_mut().clear();
                self.update_light_history_table();
            }

            Some(Self::HIST_TAB_CALIBR) => {
                self.calibr_history.borrow_mut().clear();
                self.update_calibr_history_table();
            }

            _ => {},
        }
    }

    fn show_total_raw_time_impl(&self, options: &Options) {
        let total_time = options.cam.frame.exposure() * options.raw_frames.frame_cnt as f64;
        let text = format!(
            "{:.1}s x {} ~ {}",
            options.cam.frame.exposure(),
            options.raw_frames.frame_cnt,
            seconds_to_total_time_str(total_time, false)
        );
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_str("l_raw_time_info.label", Some(&text));
    }

    fn show_total_raw_time(&self) {
        let options = self.options.read().unwrap();
        self.show_total_raw_time_impl(&options);
    }

    fn handler_action_open_image(self: &Rc<Self>) {
        let fc = gtk::FileChooserDialog::builder()
            .action(gtk::FileChooserAction::Open)
            .title("Select image file to open")
            .modal(true)
            .transient_for(&self.window)
            .build();
        gtk_utils::add_ok_and_cancel_buttons(
            fc.upcast_ref::<gtk::Dialog>(),
            "_Open",   gtk::ResponseType::Accept,
            "_Cancel", gtk::ResponseType::Cancel
        );
        fc.connect_response(clone!(@weak self as self_ => move |file_chooser, response| {
            if response == gtk::ResponseType::Accept {
                gtk_utils::exec_and_show_error(&self_.window, || {
                    let Some(file_name) = file_chooser.file() else { return Ok(()); };
                    let Some(file_name) = file_name.path() else { return Ok(()); };
                    self_.get_options_from_widgets();
                    self_.core.open_image_from_file(&file_name)?;
                    Ok(())
                });
            }
            file_chooser.close();
        }));
        fc.show();
    }

    fn handler_action_darks_library(&self) {
        if !is_expanded(&self.builder, "exp_calibr") { return; }

        let dialog = DarksLibraryDialog::new(
            &self.core,
            &self.options,
            self.window.upcast_ref(),
        );
        dialog.exec();
    }
}