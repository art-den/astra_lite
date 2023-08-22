use std::{
    rc::Rc,
    sync::*,
    cell::{RefCell, Cell},
    f64::consts::PI,
    path::PathBuf,
};
use chrono::{DateTime, Local, Utc};
use gtk::{prelude::*, glib, glib::clone, cairo, gdk};
use serde::{Serialize, Deserialize};

use crate::{
    options::*,
    gui_main::*,
    indi_api,
    gtk_utils,
    io_utils::*,
    image_processing::*,
    log_utils::*,
    image_info::*,
    image::RgbU8Data,
    image_raw::FrameType,
    state::*,
    plots::*,
    math::*,
    gui_common::*
};

pub const SET_PROP_TIMEOUT: Option<u64> = Some(1000);
const CONF_FN: &str = "gui_cam";

#[derive(Hash, Eq, PartialEq)]
enum DelayedActionTypes {
    UpdateCamList,
    StartLiveView,
    StartCooling,
    UpdateCtrlWidgets,
    UpdateResolutionList,
    SelectMaxResolution,
    UpdateFocList,
    UpdateFocPosNew,
    UpdateFocPos,
    UpdateMountWidgets,
    UpdateMountSpdList,
    FillHeaterItems,
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
            dith_exp:       false,
            quality_exp:    true,
            mount_exp:      false,
        }
    }
}

pub enum MainThreadEvent {
    ShowFrameProcessingResult(FrameProcessResult),
    StateEvent(StateEvent),
    IndiEvent(indi_api::Event),
}

struct CameraData {
    gui:                Rc<Gui>,
    builder:            gtk::Builder,
    window:             gtk::ApplicationWindow,
    options:            Arc<RwLock<Options>>,
    state:              Arc<State>,
    indi:               Arc<indi_api::Connection>,
    delayed_actions:    DelayedActions<DelayedActionTypes>,
    gui_options:        RefCell<GuiOptions>,
    conn_state:         RefCell<indi_api::ConnState>,
    indi_evt_conn:      RefCell<Option<indi_api::Subscription>>,
    preview_scroll_pos: RefCell<Option<((f64, f64), (f64, f64))>>,
    light_history:      RefCell<Vec<LightFrameShortInfo>>,
    focusing_data:      RefCell<Option<FocusingEvt>>,
    closed:             Cell<bool>,
    excl:               gtk_utils::ExclusiveCaller,
    full_screen_mode:   Cell<bool>,
    self_:              RefCell<Option<Rc<CameraData>>>,
}

impl Drop for CameraData {
    fn drop(&mut self) {
        log::info!("CameraData dropped");
    }
}

pub fn build_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    gui:      &Rc<Gui>,
    options:  &Arc<RwLock<Options>>,
    state:    &Arc<State>,
    indi:     &Arc<indi_api::Connection>,
    handlers: &mut MainGuiHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut gui_options = GuiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut gui_options, CONF_FN)?;
        Ok(())
    });

    let data = Rc::new(CameraData {
        gui:                Rc::clone(gui),
        builder:            builder.clone(),
        window:             window.clone(),
        state:              Arc::clone(state),
        indi:               Arc::clone(indi),
        options:            Arc::clone(options),
        delayed_actions:    DelayedActions::new(500),
        gui_options:        RefCell::new(gui_options),
        conn_state:         RefCell::new(indi_api::ConnState::Disconnected),
        indi_evt_conn:      RefCell::new(None),
        preview_scroll_pos: RefCell::new(None),
        light_history:      RefCell::new(Vec::new()),
        focusing_data:      RefCell::new(None),
        closed:             Cell::new(false),
        excl:               gtk_utils::ExclusiveCaller::new(),
        full_screen_mode:   Cell::new(false),
        self_:              RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    configure_camera_widget_props(&data);

    connect_indi_and_state_events(&data);

    connect_widgets_events_before_show_options(&data);

    init_focuser_widgets(&data);
    init_dithering_widgets(&data);

    show_options(&data);
    show_frame_options(&data);

    show_total_raw_time(&data);
    update_light_history_table(&data);

    connect_widgets_events(&data);
    connect_img_mouse_scroll_events(&data);
    connect_focuser_widgets_events(&data);
    connect_dithering_widgets_events(&data);
    connect_mount_widgets_events(&data);

    handlers.push(Box::new(clone!(@weak data => move |event| {
        handler_main_gui_event(&data, event);
    })));

    data.delayed_actions.set_event_handler(
        clone!(@weak data => move |action| {
            handler_delayed_action(&data, action);
        })
    );

    window.connect_delete_event(clone!(@weak data => @default-return gtk::Inhibit(false), move |_, _| {
        let res = handler_close_window(&data);
        *data.self_.borrow_mut() = None;
        res
    }));

    update_camera_devices_list(&data);
    update_focuser_devices_list(&data);
    correct_widgets_props(&data);
    correct_frame_quality_widgets_props(&data);
}

fn handler_main_gui_event(data: &Rc<CameraData>, event: MainGuiEvent) {
    match event {
        MainGuiEvent::Timer => {}
        MainGuiEvent::FullScreen(full_screen) =>
            handler_full_screen(&data, full_screen),
        MainGuiEvent::BeforeModeContinued =>
            read_options_from_widgets(data),
    }
}

fn configure_camera_widget_props(data: &Rc<CameraData>) {
    let spb_foc_len = data.builder.object::<gtk::SpinButton>("spb_foc_len").unwrap();
    spb_foc_len.set_range(10.0, 10_000.0);
    spb_foc_len.set_digits(0);
    spb_foc_len.set_increments(1.0, 10.0);

    let spb_barlow = data.builder.object::<gtk::SpinButton>("spb_barlow").unwrap();
    spb_barlow.set_range(0.1, 10.0);
    spb_barlow.set_digits(2);
    spb_barlow.set_increments(0.01, 0.1);

    let spb_temp = data.builder.object::<gtk::SpinButton>("spb_temp").unwrap();
    spb_temp.set_range(-1000.0, 1000.0);

    let spb_exp = data.builder.object::<gtk::SpinButton>("spb_exp").unwrap();
    spb_exp.set_range(0.0, 100_000.0);

    let spb_gain = data.builder.object::<gtk::SpinButton>("spb_gain").unwrap();
    spb_gain.set_range(0.0, 1_000_000.0);

    let spb_offset = data.builder.object::<gtk::SpinButton>("spb_offset").unwrap();
    spb_offset.set_range(0.0, 1_000_000.0);

    let spb_delay = data.builder.object::<gtk::SpinButton>("spb_delay").unwrap();
    spb_delay.set_range(0.0, 100_000.0);
    spb_delay.set_digits(1);
    spb_delay.set_increments(0.5, 5.0);

    let spb_raw_frames_cnt = data.builder.object::<gtk::SpinButton>("spb_raw_frames_cnt").unwrap();
    spb_raw_frames_cnt.set_range(1.0, 100_000.0);
    spb_raw_frames_cnt.set_digits(0);
    spb_raw_frames_cnt.set_increments(10.0, 100.0);

    let scl_dark = data.builder.object::<gtk::Scale>("scl_dark").unwrap();
    scl_dark.set_range(0.0, 1.0);
    scl_dark.set_increments(0.01, 0.1);
    scl_dark.set_round_digits(2);

    let scl_highlight = data.builder.object::<gtk::Scale>("scl_highlight").unwrap();
    scl_highlight.set_range(0.0, 1.0);
    scl_highlight.set_increments(0.01, 0.1);
    scl_highlight.set_round_digits(2);

    let scl_gamma = data.builder.object::<gtk::Scale>("scl_gamma").unwrap();
    scl_gamma.set_range(1.0, 5.0);
    scl_gamma.set_digits(1);
    scl_gamma.set_increments(0.1, 1.0);
    scl_gamma.set_round_digits(1);

    let spb_live_minutes = data.builder.object::<gtk::SpinButton>("spb_live_minutes").unwrap();
    spb_live_minutes.set_range(1.0, 60.0);
    spb_live_minutes.set_digits(0);
    spb_live_minutes.set_increments(1.0, 10.0);

    let spb_max_fwhm = data.builder.object::<gtk::SpinButton>("spb_max_fwhm").unwrap();
    spb_max_fwhm.set_range(1.0, 100.0);
    spb_max_fwhm.set_digits(1);
    spb_max_fwhm.set_increments(0.1, 1.0);

    let spb_max_oval = data.builder.object::<gtk::SpinButton>("spb_max_oval").unwrap();
    spb_max_oval.set_range(0.2, 2.0);
    spb_max_oval.set_digits(1);
    spb_max_oval.set_increments(0.1, 1.0);

    let l_temp_value = data.builder.object::<gtk::Label>("l_temp_value").unwrap();
    l_temp_value.set_text("");

    let l_coolpwr_value = data.builder.object::<gtk::Label>("l_coolpwr_value").unwrap();
    l_coolpwr_value.set_text("");
}

fn connect_indi_and_state_events(data: &Rc<CameraData>) {
    let (main_thread_sender, main_thread_receiver) =
        glib::MainContext::channel(glib::PRIORITY_DEFAULT);

    let sender = main_thread_sender.clone();
    *data.indi_evt_conn.borrow_mut() = Some(data.indi.subscribe_events(move |event| {
        sender.send(MainThreadEvent::IndiEvent(event)).unwrap();
    }));

    let sender = main_thread_sender.clone();
    data.state.subscribe_events(move |event| {
        sender.send(MainThreadEvent::StateEvent(event)).unwrap();
    });

    let sender = main_thread_sender.clone();
    data.state.connect_main_cam_proc_result_event(move |res| {
        _ = sender.send(MainThreadEvent::ShowFrameProcessingResult(res));
    });

    main_thread_receiver.attach(None,
        clone!(@weak data => @default-return Continue(false),
        move |event| {
            if data.closed.get() { return Continue(false); };
            process_event_in_main_thread(&data, event);
            Continue(true)
        })
    );
}

fn process_event_in_main_thread(data: &Rc<CameraData>, event: MainThreadEvent) {
    match event {
        MainThreadEvent::IndiEvent(indi_api::Event::ConnChange(conn_state)) =>
            process_conn_state_event(&data, conn_state),
        MainThreadEvent::IndiEvent(indi_api::Event::PropChange(event_data)) => {
            match &event_data.change {
                indi_api::PropChange::New(value) =>
                    process_simple_prop_change_event(
                        &data,
                        &event_data.device_name,
                        &event_data.prop_name,
                        &value.elem_name,
                        true,
                        None,
                        None,
                        &value.prop_value
                    ),
                indi_api::PropChange::Change{ value, prev_state, new_state } =>
                    process_simple_prop_change_event(
                        &data,
                        &event_data.device_name,
                        &event_data.prop_name,
                        &value.elem_name,
                        false,
                        Some(prev_state),
                        Some(new_state),
                        &value.prop_value
                    ),
                indi_api::PropChange::Delete => {}
            };
        },

        MainThreadEvent::IndiEvent(indi_api::Event::DeviceDelete(event)) => {
            update_devices_list_and_props_by_drv_interface(&data, event.drv_interface);
        },

        MainThreadEvent::ShowFrameProcessingResult(result) => {
            match result.data {
                FrameProcessResultData::ShotProcessingStarted(mode_type) => {
                    let mode_data = data.state.mode_data();
                    if mode_data.mode.get_type() == mode_type {
                        drop(mode_data);
                        data.state.notify_about_frame_processing_started();
                    }
                },
                FrameProcessResultData::ShotProcessingFinished {frame_is_ok, mode_type, process_time, blob_dl_time} => {
                    let max_fps = if process_time != 0.0 { 1.0/process_time } else { 0.0 };
                    let perf_str = format!("Download time = {:.2}s, max. FPS={:.1}", blob_dl_time, max_fps);
                    data.gui.set_perf_string(perf_str);

                    let mode_data = data.state.mode_data();
                    if mode_data.mode.get_type() == mode_type {
                        drop(mode_data);
                        data.state.notify_about_frame_processing_finished(frame_is_ok);
                    }
                },
                _ => {},
            }
            show_frame_processing_result(&data, result);
        },

        MainThreadEvent::StateEvent(StateEvent::ModeChanged) => {
            correct_widgets_props(&data);
        },

        MainThreadEvent::StateEvent(StateEvent::ModeContinued) => {
            show_frame_options(&data);
        },

        MainThreadEvent::StateEvent(StateEvent::Focusing(fdata)) => {
            *data.focusing_data.borrow_mut() = Some(fdata);
            let da_focusing = data.builder.object::<gtk::DrawingArea>("da_focusing").unwrap();
            da_focusing.queue_draw();
        },

        MainThreadEvent::StateEvent(StateEvent::FocusResultValue { value }) => {
            update_focuser_position_after_focusing(&data, value);
        },

        _ => {},
    }
}

fn connect_widgets_events_before_show_options(data: &Rc<CameraData>) {
    let sw_preview_img = data.builder.object::<gtk::Widget>("sw_preview_img").unwrap();
    sw_preview_img.connect_size_allocate(clone!(@weak data => move |_, rect| {
        let mut options = data.options.write().unwrap();
        options.preview.widget_width = rect.width() as usize;
        options.preview.widget_height = rect.height() as usize;
    }));
}

fn connect_widgets_events(data: &Rc<CameraData>) {
    let bldr = &data.builder;
    gtk_utils::connect_action(&data.window, data, "take_shot",              handler_action_take_shot);
    gtk_utils::connect_action(&data.window, data, "stop_shot",              handler_action_stop_shot);
    gtk_utils::connect_action(&data.window, data, "clear_light_history",    handler_action_clear_light_history);
    gtk_utils::connect_action(&data.window, data, "start_save_raw_frames",  handler_action_start_save_raw_frames);
    gtk_utils::connect_action(&data.window, data, "stop_save_raw_frames",   handler_action_stop_save_raw_frames);
    gtk_utils::connect_action(&data.window, data, "continue_save_raw",      handler_action_continue_save_raw_frames);
    gtk_utils::connect_action(&data.window, data, "start_live_stacking",    handler_action_start_live_stacking);
    gtk_utils::connect_action(&data.window, data, "stop_live_stacking",     handler_action_stop_live_stacking);
    gtk_utils::connect_action(&data.window, data, "continue_live_stacking", handler_action_continue_live_stacking);
    gtk_utils::connect_action(&data.window, data, "manual_focus",           handler_action_manual_focus);
    gtk_utils::connect_action(&data.window, data, "stop_manual_focus",      handler_action_stop_manual_focus);
    gtk_utils::connect_action(&data.window, data, "start_dither_calibr",    handler_action_start_dither_calibr);
    gtk_utils::connect_action(&data.window, data, "stop_dither_calibr",     handler_action_stop_dither_calibr);
    gtk_utils::connect_action(&data.window, data, "load_image",             handler_action_open_image);
    gtk_utils::connect_action(&data.window, data, "save_image_preview",     handler_action_save_image_preview);
    gtk_utils::connect_action(&data.window, data, "save_image_linear",      handler_action_save_image_linear);

    let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
    cb_frame_mode.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let frame_type = FrameType::from_active_id(cb.active_id().as_deref());
            data.options.write().unwrap().cam.frame.frame_type = frame_type;
            correct_widgets_props(&data);
        });
    }));

    let chb_cooler = bldr.object::<gtk::CheckButton>("chb_cooler").unwrap();
    chb_cooler.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.ctrl.enable_cooler = chb.is_active();
            control_camera_by_options(&data, false);
            correct_widgets_props(&data);
        });
    }));

    let cb_cam_heater = bldr.object::<gtk::ComboBoxText>("cb_cam_heater").unwrap();
    cb_cam_heater.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.ctrl.heater_str = cb.active_id().map(|id| id.to_string());
            control_camera_by_options(&data, false);
            correct_widgets_props(&data);
        });
    }));

    let chb_fan = bldr.object::<gtk::CheckButton>("chb_fan").unwrap();
    chb_fan.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.ctrl.enable_fan = chb.is_active();
            control_camera_by_options(&data, false);
            correct_widgets_props(&data);
        });
    }));

    let spb_temp = bldr.object::<gtk::SpinButton>("spb_temp").unwrap();
    spb_temp.connect_value_changed(clone!(@weak data => move |spb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.ctrl.temperature = spb.value();
            control_camera_by_options(&data, false);
        });
    }));

    let chb_shots_cont = bldr.object::<gtk::CheckButton>("chb_shots_cont").unwrap();
    chb_shots_cont.connect_active_notify(clone!(@weak data => move |_| {
        data.excl.exec(|| {
            read_options_from_widgets(&data);
            correct_widgets_props(&data);
            handler_live_view_changed(&data);
        });
    }));

    let cb_frame_mode = bldr.object::<gtk::ComboBoxText>("cb_frame_mode").unwrap();
    cb_frame_mode.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let frame_type = FrameType::from_active_id(cb.active_id().as_deref());
            let mut options = data.options.write().unwrap();
            options.cam.frame.frame_type = frame_type;
            gtk_utils::set_f64(&data.builder, "spb_exp", options.cam.frame.exposure());
            drop(options);
            show_total_raw_time(&data);
        });
    }));

    let spb_exp = bldr.object::<gtk::SpinButton>("spb_exp").unwrap();
    spb_exp.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.frame.set_exposure(sb.value());
            show_total_raw_time(&data);
        });
    }));

    let spb_gain = bldr.object::<gtk::SpinButton>("spb_gain").unwrap();
    spb_gain.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.frame.gain = sb.value();
        });
    }));

    let spb_offset = bldr.object::<gtk::SpinButton>("spb_offset").unwrap();
    spb_offset.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.frame.offset = sb.value() as i32;
        });
    }));

    let cb_bin = bldr.object::<gtk::ComboBoxText>("cb_bin").unwrap();
    cb_bin.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let binning = Binning::from_active_id(cb.active_id().as_deref());
            data.options.write().unwrap().cam.frame.binning = binning;
        });
    }));

    let cb_crop = bldr.object::<gtk::ComboBoxText>("cb_crop").unwrap();
    cb_crop.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let crop = Crop::from_active_id(cb.active_id().as_deref());
            data.options.write().unwrap().cam.frame.crop = crop;
        });
    }));

    let spb_delay = bldr.object::<gtk::SpinButton>("spb_delay").unwrap();
    spb_delay.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.frame.delay = sb.value();
        });
    }));

    let chb_low_noise = bldr.object::<gtk::CheckButton>("chb_low_noise").unwrap();
    chb_low_noise.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().cam.frame.low_noise = chb.is_active();
        });
    }));

    let spb_raw_frames_cnt = bldr.object::<gtk::SpinButton>("spb_raw_frames_cnt").unwrap();
    spb_raw_frames_cnt.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().raw_frames.frame_cnt = sb.value() as usize;
            show_total_raw_time(&data);
        });
    }));

    let da_shot_state = bldr.object::<gtk::DrawingArea>("da_shot_state").unwrap();
    da_shot_state.connect_draw(clone!(@weak data => @default-return Inhibit(false), move |area, cr| {
        handler_draw_shot_state(&data, area, cr);
        Inhibit(false)
    }));

    let cb_preview_src = bldr.object::<gtk::ComboBoxText>("cb_preview_src").unwrap();
    cb_preview_src.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let source = PreviewSource::from_active_id(cb.active_id().as_deref());
            data.options.write().unwrap().preview.source = source;
            create_and_show_preview_image(&data);
            repaint_histogram(&data);
            show_histogram_stat(&data);
            show_image_info(&data);
        });
    }));

    let cb_preview_scale = bldr.object::<gtk::ComboBoxText>("cb_preview_scale").unwrap();
    cb_preview_scale.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let scale = PreviewScale::from_active_id(cb.active_id().as_deref());
            data.options.write().unwrap().preview.scale = scale;
            create_and_show_preview_image(&data);
        });
    }));

    let cb_preview_color = bldr.object::<gtk::ComboBoxText>("cb_preview_color").unwrap();
    cb_preview_color.connect_active_id_notify(clone!(@weak data => move |cb| {
        data.excl.exec(|| {
            let color = PreviewColor::from_active_id(cb.active_id().as_deref());
            data.options.write().unwrap().preview.color = color;
            create_and_show_preview_image(&data);
        });
    }));

    let scl_dark = bldr.object::<gtk::Scale>("scl_dark").unwrap();
    scl_dark.connect_value_changed(clone!(@weak data => move |scl| {
        data.excl.exec(|| {
            let mut options = data.options.write().unwrap();
            options.preview.dark_lvl = scl.value();
            drop(options);
            create_and_show_preview_image(&data);
        });
    }));

    let scl_highlight = bldr.object::<gtk::Scale>("scl_highlight").unwrap();
    scl_highlight.connect_value_changed(clone!(@weak data => move |scl| {
        data.excl.exec(|| {
            let mut options = data.options.write().unwrap();
            options.preview.light_lvl = scl.value();
            drop(options);
            create_and_show_preview_image(&data);
        });
    }));

    let scl_gamma = bldr.object::<gtk::Scale>("scl_gamma").unwrap();
    scl_gamma.connect_value_changed(clone!(@weak data => move |scl| {
        data.excl.exec(|| {
            let mut options = data.options.write().unwrap();
            options.preview.gamma = scl.value();
            drop(options);
            create_and_show_preview_image(&data);
        });
    }));

    let chb_rem_grad = bldr.object::<gtk::CheckButton>("chb_rem_grad").unwrap();
    chb_rem_grad.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().preview.remove_grad = chb.is_active();
            create_and_show_preview_image(&data);
        });
    }));

    let da_histogram = bldr.object::<gtk::DrawingArea>("da_histogram").unwrap();
    da_histogram.connect_draw(clone!(@weak data => @default-return Inhibit(false), move |area, cr| {
        handler_draw_histogram(&data, area, cr);
        Inhibit(false)
    }));

    let ch_hist_logy = bldr.object::<gtk::CheckButton>("ch_hist_logy").unwrap();
    ch_hist_logy.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().hist.log_y = chb.is_active();
            repaint_histogram(&data)
        });
    }));

    let ch_stat_percents = bldr.object::<gtk::CheckButton>("ch_stat_percents").unwrap();
    ch_stat_percents.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().hist.percents = chb.is_active();
            show_histogram_stat(&data)
        });
    }));

    let chb_max_fwhm = bldr.object::<gtk::CheckButton>("chb_max_fwhm").unwrap();
    chb_max_fwhm.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().quality.use_max_fwhm = chb.is_active();
        });
        correct_frame_quality_widgets_props(&data);
    }));

    let chb_max_oval = bldr.object::<gtk::CheckButton>("chb_max_oval").unwrap();
    chb_max_oval.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().quality.use_max_ovality = chb.is_active();
        });
        correct_frame_quality_widgets_props(&data);
    }));

    let spb_max_fwhm = bldr.object::<gtk::SpinButton>("spb_max_fwhm").unwrap();
    spb_max_fwhm.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().quality.max_fwhm = sb.value() as f32;
        });
    }));

    let spb_max_oval = bldr.object::<gtk::SpinButton>("spb_max_oval").unwrap();
    spb_max_oval.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            data.options.write().unwrap().quality.max_ovality = sb.value() as f32;
        });
    }));

    let chb_master_dark = data.builder.object::<gtk::CheckButton>("chb_master_dark").unwrap();
    chb_master_dark.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().calibr.dark_frame_en = chb.is_active();
        });
    }));

    let fch_master_dark = data.builder.object::<gtk::FileChooserButton>("fch_master_dark").unwrap();
    fch_master_dark.connect_file_set(clone!(@weak data => move |fch| {
        data.excl.exec(|| {
            data.options.write().unwrap().calibr.dark_frame = fch.filename();
        });
    }));

    let chb_master_flat = data.builder.object::<gtk::CheckButton>("chb_master_flat").unwrap();
    chb_master_flat.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            data.options.write().unwrap().calibr.flat_frame_en = chb.is_active();
        });
    }));

    let fch_master_flat = data.builder.object::<gtk::FileChooserButton>("fch_master_flat").unwrap();
    fch_master_flat.connect_file_set(clone!(@weak data => move |fch| {
        data.excl.exec(|| {
            data.options.write().unwrap().calibr.flat_frame = fch.filename();
        });
    }));

    let chb_hot_pixels = data.builder.object::<gtk::CheckButton>("chb_hot_pixels").unwrap();
    chb_hot_pixels.connect_active_notify(clone!(@weak data => move |chb| {
        gtk_utils::enable_widgets(
            &data.builder,
            false,
            &[("l_hot_pixels_warn", chb.is_active())]
        );
        data.excl.exec(|| {
            data.options.write().unwrap().calibr.hot_pixels = chb.is_active();
        });
    }));
}

fn connect_img_mouse_scroll_events(data: &Rc<CameraData>) {
    let eb_preview_img = data.builder.object::<gtk::EventBox>("eb_preview_img").unwrap();
    let sw_preview_img = data.builder.object::<gtk::ScrolledWindow>("sw_preview_img").unwrap();

    eb_preview_img.connect_button_press_event(clone!(@weak data, @weak sw_preview_img => @default-return Inhibit(false), move |_, evt| {
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

    eb_preview_img.connect_button_release_event(clone!(@weak data => @default-return Inhibit(false), move |_, evt| {
        if evt.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
            *data.preview_scroll_pos.borrow_mut() = None;
        }
        Inhibit(false)
    }));

    eb_preview_img.connect_motion_notify_event(clone!(@weak data, @weak sw_preview_img => @default-return Inhibit(false), move |_, evt| {
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

fn handler_full_screen(data: &Rc<CameraData>, full_screen: bool) {
    let bldr = &data.builder;

    let bx_cam_left = bldr.object::<gtk::Widget>("bx_cam_left").unwrap();
    let scr_cam_right = bldr.object::<gtk::Widget>("scr_cam_right").unwrap();
    let pan_cam3 = bldr.object::<gtk::Widget>("pan_cam3").unwrap();
    let bx_img_info = bldr.object::<gtk::Widget>("bx_img_info").unwrap();
    if full_screen {
        read_options_from_widgets(data);
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
    data.full_screen_mode.set(full_screen);

    let options = data.options.read().unwrap();
    if options.preview.scale == PreviewScale::FitWindow {
        drop(options);
        gtk::main_iteration_do(true);
        gtk::main_iteration_do(true);
        gtk::main_iteration_do(true);
        create_and_show_preview_image(&data);
    }

}

fn handler_close_window(data: &Rc<CameraData>) -> gtk::Inhibit {
    data.closed.set(true);

    _ = data.state.stop_img_process_thread();

    _ = data.state.abort_active_mode();
    read_options_from_widgets(data);

    let gui_options = data.gui_options.borrow();
    _ = save_json_to_config::<GuiOptions>(&gui_options, CONF_FN);
    drop(gui_options);

    if let Some(indi_conn) = data.indi_evt_conn.borrow_mut().take() {
        data.indi.unsubscribe(indi_conn);
    }

    gtk::Inhibit(false)
}

fn show_frame_options(data: &Rc<CameraData>) {
    data.excl.exec(|| {
        let options = data.options.read().unwrap();
        let bld = &data.builder;

        gtk_utils::set_active_id(bld, "cb_frame_mode", options.cam.frame.frame_type.to_active_id());
        gtk_utils::set_f64      (bld, "spb_exp",       options.cam.frame.exposure());
        gtk_utils::set_f64      (bld, "spb_delay",     options.cam.frame.delay);
        gtk_utils::set_f64      (bld, "spb_gain",      options.cam.frame.gain);
        gtk_utils::set_f64      (bld, "spb_offset",    options.cam.frame.offset as f64);
        gtk_utils::set_active_id(bld, "cb_bin",        options.cam.frame.binning.to_active_id());
        gtk_utils::set_active_id(bld, "cb_crop",       options.cam.frame.crop.to_active_id());
        gtk_utils::set_bool     (bld, "chb_low_noise", options.cam.frame.low_noise);
    });


    match data.state.mode_data().mode.get_type() {
        ModeType::LiveStacking => {
            gtk_utils::set_active_id(
                &data.builder,
                "cb_preview_src",
                Some("live")

            );
        },
        _ => {
            gtk_utils::set_active_id(
                &data.builder,
                "cb_preview_src",
                Some("frame")
            );
        }
    }

}

fn show_options(data: &Rc<CameraData>) {
    data.excl.exec(|| {
        let bld = &data.builder;

        let options = data.options.read().unwrap();
        gtk_utils::set_f64(bld, "spb_foc_len", options.telescope.focal_len);
        gtk_utils::set_f64(bld, "spb_barlow",  options.telescope.barlow);

        gtk_utils::set_bool     (bld, "chb_shots_cont",      options.cam.live_view);

        gtk_utils::set_bool     (bld, "chb_cooler",          options.cam.ctrl.enable_cooler);
        gtk_utils::set_f64      (bld, "spb_temp",            options.cam.ctrl.temperature);
        gtk_utils::set_bool     (bld, "chb_fan",             options.cam.ctrl.enable_fan);

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
        gtk_utils::set_active_id(bld, "cb_preview_color",    options.preview.color.to_active_id());
        gtk_utils::set_f64      (bld, "scl_dark",            options.preview.dark_lvl);
        gtk_utils::set_f64      (bld, "scl_highlight",       options.preview.light_lvl);
        gtk_utils::set_f64      (bld, "scl_gamma",           options.preview.gamma);
        gtk_utils::set_bool     (bld, "chb_rem_grad",        options.preview.remove_grad);

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

        gtk_utils::set_active_id(bld, "cb_dith_perod",    Some(options.simp_guid.dith_period.to_string().as_str()));
        gtk_utils::set_active_id(bld, "cb_dith_distance", Some(format!("{:.0}", options.simp_guid.dith_percent * 10.0).as_str()));
        gtk_utils::set_bool     (bld, "chb_guid_enabled", options.simp_guid.enabled);
        gtk_utils::set_f64      (bld, "spb_guid_max_err", options.simp_guid.max_error);
        gtk_utils::set_f64      (bld, "spb_mnt_cal_exp",  options.simp_guid.calibr_exposure);

        gtk_utils::set_bool     (bld, "chb_inv_ns",      options.mount.inv_ns);
        gtk_utils::set_bool     (bld, "chb_inv_we",      options.mount.inv_we);

        gtk_utils::enable_widgets(bld, false, &[("l_hot_pixels_warn", options.calibr.hot_pixels)]);

        drop(options);

        let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
        let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
        let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();
        let pan_cam4 = bld.object::<gtk::Paned>("pan_cam4").unwrap();

        let gui_options = data.gui_options.borrow();
        pan_cam1.set_position(gui_options.paned_pos1);
        if gui_options.paned_pos2 != -1 {
            pan_cam2.set_position(pan_cam2.allocation().width()-gui_options.paned_pos2);
        }
        pan_cam3.set_position(gui_options.paned_pos3);
        if gui_options.paned_pos4 != -1 {
            pan_cam4.set_position(pan_cam4.allocation().height()-gui_options.paned_pos4);
        }
        gtk_utils::set_bool_prop(bld, "exp_cam_ctrl",   "expanded", gui_options.cam_ctrl_exp);
        gtk_utils::set_bool_prop(bld, "exp_shot_set",   "expanded", gui_options.shot_exp);
        gtk_utils::set_bool_prop(bld, "exp_calibr",     "expanded", gui_options.calibr_exp);
        gtk_utils::set_bool_prop(bld, "exp_raw_frames", "expanded", gui_options.raw_frames_exp);
        gtk_utils::set_bool_prop(bld, "exp_live",       "expanded", gui_options.live_exp);
        gtk_utils::set_bool_prop(bld, "exp_foc",        "expanded", gui_options.foc_exp);
        gtk_utils::set_bool_prop(bld, "exp_dith",       "expanded", gui_options.dith_exp);
        gtk_utils::set_bool_prop(bld, "exp_quality",    "expanded", gui_options.quality_exp);
        gtk_utils::set_bool_prop(bld, "exp_mount",      "expanded", gui_options.mount_exp);
        drop(gui_options);
    });
}

fn read_options_from_widgets(data: &Rc<CameraData>) {
    let mut options = data.options.write().unwrap();
    let bld = &data.builder;

    options.telescope.focal_len = gtk_utils::get_f64(bld, "spb_foc_len");
    options.telescope.barlow = gtk_utils::get_f64(bld, "spb_barlow");

    options.preview.scale = {
        let active_id = gtk_utils::get_active_id(bld, "cb_preview_scale");
        PreviewScale::from_active_id(active_id.as_deref())
    };

    options.preview.color = {
        let active_id = gtk_utils::get_active_id(bld, "cb_preview_color");
        PreviewColor::from_active_id(active_id.as_deref())
    };

    options.cam.frame.frame_type = FrameType::from_active_id(
        gtk_utils::get_active_id(bld, "cb_frame_mode").as_deref()
    );

    options.cam.frame.binning = Binning::from_active_id(
        gtk_utils::get_active_id(bld, "cb_bin").as_deref()
    );

    options.cam.frame.crop = Crop::from_active_id(
        gtk_utils::get_active_id(bld, "cb_crop").as_deref()
    );

    options.preview.source = PreviewSource::from_active_id(
        gtk_utils::get_active_id(bld, "cb_preview_src").as_deref()
    );

    options.cam.device               = gtk_utils::get_active_id(bld, "cb_camera_list").unwrap_or_default();
    options.cam.live_view            = gtk_utils::get_bool     (bld, "chb_shots_cont");

    options.cam.ctrl.enable_cooler   = gtk_utils::get_bool     (bld, "chb_cooler");
    options.cam.ctrl.temperature     = gtk_utils::get_f64      (bld, "spb_temp");
    options.cam.ctrl.enable_fan      = gtk_utils::get_bool     (bld, "chb_fan");

    options.cam.frame.set_exposure    (gtk_utils::get_f64      (bld, "spb_exp"));
    options.cam.frame.delay          = gtk_utils::get_f64      (bld, "spb_delay");
    options.cam.frame.gain           = gtk_utils::get_f64      (bld, "spb_gain");
    options.cam.frame.offset         = gtk_utils::get_f64      (bld, "spb_offset") as i32;
    options.cam.frame.low_noise      = gtk_utils::get_bool     (bld, "chb_low_noise");

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

    options.preview.gamma        = gtk_utils::get_f64      (bld, "scl_gamma");
    options.preview.dark_lvl     = gtk_utils::get_f64      (bld, "scl_dark");
    options.preview.light_lvl    = gtk_utils::get_f64      (bld, "scl_highlight");
    options.preview.remove_grad  = gtk_utils::get_bool     (bld, "chb_rem_grad");

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

    options.simp_guid.dith_period     = gtk_utils::get_active_id(bld, "cb_dith_perod").and_then(|v| v.parse().ok()).unwrap_or(0);
    options.simp_guid.dith_percent    = gtk_utils::get_active_id(bld, "cb_dith_distance").and_then(|v| v.parse().ok()).unwrap_or(10.0) / 10.0;
    options.simp_guid.enabled         = gtk_utils::get_bool     (bld, "chb_guid_enabled");
    options.simp_guid.max_error       = gtk_utils::get_f64      (bld, "spb_guid_max_err");
    options.simp_guid.calibr_exposure = gtk_utils::get_f64      (bld, "spb_mnt_cal_exp");

    options.mount.inv_ns            = gtk_utils::get_bool     (bld, "chb_inv_ns");
    options.mount.inv_we            = gtk_utils::get_bool     (bld, "chb_inv_we");
    let mount_speed                 = gtk_utils::get_active_id(bld, "cb_mnt_speed");
    if mount_speed.is_some() {
        options.mount.speed = mount_speed;
    }

    drop(options);

    let mut gui = data.gui_options.borrow_mut();
    if !data.full_screen_mode.get() {
        let pan_cam1 = bld.object::<gtk::Paned>("pan_cam1").unwrap();
        let pan_cam2 = bld.object::<gtk::Paned>("pan_cam2").unwrap();
        let pan_cam3 = bld.object::<gtk::Paned>("pan_cam3").unwrap();
        let pan_cam4 = bld.object::<gtk::Paned>("pan_cam4").unwrap();
        gui.paned_pos1 = pan_cam1.position();
        gui.paned_pos2 = pan_cam2.allocation().width()-pan_cam2.position();
        gui.paned_pos3 = pan_cam3.position();
        gui.paned_pos4 = pan_cam4.allocation().height()-pan_cam4.position();
    }
    gui.cam_ctrl_exp   = gtk_utils::get_bool_prop(bld, "exp_cam_ctrl",   "expanded");
    gui.shot_exp       = gtk_utils::get_bool_prop(bld, "exp_shot_set",   "expanded");
    gui.calibr_exp     = gtk_utils::get_bool_prop(bld, "exp_calibr",     "expanded");
    gui.raw_frames_exp = gtk_utils::get_bool_prop(bld, "exp_raw_frames", "expanded");
    gui.live_exp       = gtk_utils::get_bool_prop(bld, "exp_live",       "expanded");
    gui.foc_exp        = gtk_utils::get_bool_prop(bld, "exp_foc",        "expanded");
    gui.dith_exp       = gtk_utils::get_bool_prop(bld, "exp_dith",       "expanded");
    gui.quality_exp    = gtk_utils::get_bool_prop(bld, "exp_quality",    "expanded");
    gui.mount_exp      = gtk_utils::get_bool_prop(bld, "exp_mount",      "expanded");
    drop(gui);
}

fn handler_delayed_action(data: &Rc<CameraData>, action: &DelayedActionTypes) {
    match action {
        DelayedActionTypes::UpdateCamList => {
            update_camera_devices_list(data);
            correct_widgets_props(data);
        }
        DelayedActionTypes::UpdateFocList => {
            update_focuser_devices_list(data);
            correct_widgets_props(data);
        }
        DelayedActionTypes::StartLiveView
        if data.options.read().unwrap().cam.live_view => {
            start_live_view(data);
        }
        DelayedActionTypes::StartCooling => {
            control_camera_by_options(data, true);
        }
        DelayedActionTypes::UpdateCtrlWidgets => {
            correct_widgets_props(data);
        }
        DelayedActionTypes::UpdateResolutionList => {
            update_resolution_list(data);
        }
        DelayedActionTypes::SelectMaxResolution => {
            select_maximum_resolution(data);
        }
        DelayedActionTypes::UpdateFocPosNew |
        DelayedActionTypes::UpdateFocPos => {
            update_focuser_position_widget(
                data,
                *action == DelayedActionTypes::UpdateFocPosNew
            );
        }
        DelayedActionTypes::UpdateMountWidgets => {
            correct_widgets_props(data); // ???
        }
        DelayedActionTypes::UpdateMountSpdList => {
            fill_mount_speed_list_widget(data);
        }
        DelayedActionTypes::FillHeaterItems => {
            fill_heater_items_list(data);
        }
        _ => {}
    }
}

fn correct_widgets_props(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let bldr = &data.builder;
        let camera = gtk_utils::get_active_id(bldr, "cb_camera_list");
        let mount = data.options.read().unwrap().mount.device.clone();
        let correct_num_adjustment_by_prop = |
            spb_name:  &str,
            prop_info: indi_api::Result<Arc<indi_api::NumPropElemInfo>>,
            digits:    u32,
            step:      Option<f64>,
        | -> bool {
            if let Ok(info) = prop_info {
                let spb = bldr.object::<gtk::SpinButton>(spb_name).unwrap();
                spb.set_range(info.min, info.max);
                let value = spb.value();
                if value < info.min {
                    spb.set_value(info.min);
                }
                if value > info.max {
                    spb.set_value(info.max);
                }
                let desired_step =
                    if      info.max <= 1.0   { 0.1 }
                    else if info.max <= 10.0  { 1.0 }
                    else if info.max <= 100.0 { 10.0 }
                    else                      { 100.0 };
                let step = step.unwrap_or(desired_step);
                spb.set_increments(step, 10.0 * step);
                spb.set_digits(digits);
                true
            } else {
                false
            }
        };
        let temp_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            "spb_temp",
            data.indi.camera_get_temperature_prop_info(camera),
            0,
            Some(1.0)
        )).unwrap_or(false);
        let exposure_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            "spb_exp",
            data.indi.camera_get_exposure_prop_info(camera),
            3,
            Some(1.0),
        )).unwrap_or(false);
        let gain_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            "spb_gain",
            data.indi.camera_get_gain_prop_info(camera),
            0,
            None
        )).unwrap_or(false);
        let offset_supported = camera.as_ref().map(|camera| correct_num_adjustment_by_prop(
            "spb_offset",
            data.indi.camera_get_offset_prop_info(&camera),
            0,
            None
        )).unwrap_or(false);
        let bin_supported = camera.as_ref().map(|camera|
            data.indi.camera_is_binning_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let fan_supported = camera.as_ref().map(|camera|
            data.indi.camera_is_fan_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let heater_supported = camera.as_ref().map(|camera|
            data.indi.camera_is_heater_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let low_noise_supported = camera.as_ref().map(|camera|
            data.indi.camera_is_low_noise_ctrl_supported(&camera)
        ).unwrap_or(Ok(false))?;
        let crop_supported = camera.as_ref().map(|camera|
            data.indi.camera_is_frame_supported(&camera)
        ).unwrap_or(Ok(false))?;

        let indi_connected = data.indi.state() == indi_api::ConnState::Connected;

        let cooler_active = gtk_utils::get_bool(bldr, "chb_cooler");

        let frame_mode_str = gtk_utils::get_active_id(bldr, "cb_frame_mode");
        let frame_mode = FrameType::from_active_id(frame_mode_str.as_deref());

        let frame_mode_is_lights = frame_mode == FrameType::Lights;
        let frame_mode_is_flat = frame_mode == FrameType::Flats;
        let frame_mode_is_dark = frame_mode == FrameType::Darks;

        let mode_data = data.state.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let shot_active = mode_type == ModeType::SingleShot;
        let liveview_active = mode_type == ModeType::LiveView;
        let saving_frames = mode_type == ModeType::SavingRawFrames;
        let mnt_calibr = mode_type == ModeType::DitherCalibr;
        let focusing = mode_type == ModeType::Focusing;
        let saving_frames_paused = mode_data.aborted_mode
            .as_ref()
            .map(|mode| mode.get_type() == ModeType::SavingRawFrames)
            .unwrap_or(false);
        let live_active = mode_type == ModeType::LiveStacking;
        let livestacking_paused = mode_data.aborted_mode
            .as_ref()
            .map(|mode| mode.get_type() == ModeType::LiveStacking)
            .unwrap_or(false);
        let dither_calibr = mode_type == ModeType::DitherCalibr;
        drop(mode_data);

        let foc_device = gtk_utils::get_active_id(bldr, "cb_foc_list");
        let foc_active = data.indi
            .is_device_enabled(foc_device.as_deref().unwrap_or(""))
            .unwrap_or(false);

        let focuser_sensitive =
            indi_connected &&
            foc_device.is_some() && foc_active &&
            !saving_frames &&
            !live_active &&
            !mnt_calibr &&
            !focusing;

        let dithering_sensitive =
            indi_connected &&
            !mount.is_empty() &&
            !saving_frames &&
            !live_active &&
            !mnt_calibr &&
            !focusing;

        let mnt_active = data.indi.is_device_enabled(&mount).unwrap_or(false);

        let mount_ctrl_sensitive =
            (indi_connected &&
            mnt_active &&
            !mount.is_empty() &&
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

        let cam_active = data.indi
            .is_device_enabled(camera.as_deref().unwrap_or(""))
            .unwrap_or(false);

        let can_change_cam_opts = !saving_frames && !live_active;
        let can_change_mode = waiting || shot_active;
        let can_change_frame_opts = waiting || liveview_active;
        let can_change_live_stacking_opts = waiting || liveview_active;
        let can_change_cal_ops = !live_active && !dither_calibr;
        let cam_sensitive =
            indi_connected &&
            cam_active &&
            camera.is_some();

        gtk_utils::enable_actions(&data.window, &[
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
            ("l_cam_heater",  heater_supported),
            ("cb_cam_heater", heater_supported),
            ("chb_low_noise", low_noise_supported),
        ]);

        gtk_utils::enable_widgets(bldr, false, &[
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
            ("fch_master_dark",    can_change_cal_ops),
            ("chb_master_flat",    can_change_cal_ops),
            ("fch_master_flat",    can_change_cal_ops),
            ("chb_raw_frames_cnt", !saving_frames && can_change_mode),
            ("spb_raw_frames_cnt", !saving_frames && can_change_mode),

            ("chb_live_save",      can_change_live_stacking_opts),
            ("spb_live_minutes",   can_change_live_stacking_opts),
            ("chb_live_save_orig", can_change_live_stacking_opts),
            ("fch_live_folder",    can_change_live_stacking_opts),

            ("bx_cam_main",        cam_sensitive),
            ("grd_cam_ctrl",       cam_sensitive),
            ("grd_shot_settings",  cam_sensitive),
            ("grd_save_raw",       cam_sensitive),
            ("grd_live_stack",     cam_sensitive),
            ("grd_cam_calibr",     cam_sensitive),
            ("bx_light_qual",      cam_sensitive),
            ("grd_foc",            focuser_sensitive),
            ("grd_dither",         dithering_sensitive && cam_sensitive),
            ("bx_simple_mount",    mount_ctrl_sensitive),

            ("spb_foc_temp",       gtk_utils::get_bool(bldr, "chb_foc_temp")),
            ("cb_foc_fwhm",        gtk_utils::get_bool(bldr, "chb_foc_fwhm")),
            ("cb_foc_period",      gtk_utils::get_bool(bldr, "chb_foc_period")),
            ("spb_guid_max_err",   gtk_utils::get_bool(bldr, "chb_guid_enabled")),

            ("l_delay",            liveview_active),
            ("spb_delay",          liveview_active),
        ]);

        Ok(())
    });

}


fn update_camera_devices_list(data: &Rc<CameraData>) {
    data.excl.exec(|| {
        let options = data.options.read().unwrap();
        let cb_camera_list: gtk::ComboBoxText =
            data.builder.object("cb_camera_list").unwrap();
        let cameras_count = fill_combobox_with_cam_list(
            &data.indi,
            &cb_camera_list,
            &options.cam.device
        );
        let connected = data.indi.state() == indi_api::ConnState::Connected;
        gtk_utils::enable_widgets(&data.builder, false, &[
            ("cb_camera_list", connected && cameras_count > 1),
        ]);
    });
}

fn update_resolution_list(data: &Rc<CameraData>) {
    data.excl.exec(|| gtk_utils::exec_and_show_error(&data.window, || {
        let cb_bin = data.builder.object::<gtk::ComboBoxText>("cb_bin").unwrap();
        let last_bin = cb_bin.active_id();
        cb_bin.remove_all();
        let options = data.options.read().unwrap();
        if options.cam.device.is_empty() { return Ok(()); }
        let (max_width, max_height) = data.indi.camera_get_max_frame_size(&options.cam.device)?;
        let (max_hor_bin, max_vert_bin) = data.indi.camera_get_max_binning(&options.cam.device)?;
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
        Ok(())
    }));
}

fn fill_heater_items_list(data: &Rc<CameraData>) {
    data.excl.exec(|| gtk_utils::exec_and_show_error(&data.window, ||{
        let cb_cam_heater = data.builder.object::<gtk::ComboBoxText>("cb_cam_heater").unwrap();
        let last_heater_value = cb_cam_heater.active_id();
        cb_cam_heater.remove_all();
        let options = data.options.read().unwrap();
        if options.cam.device.is_empty() { return Ok(()); };
        if !data.indi.camera_is_heater_supported(&options.cam.device)? { return Ok(()) }
        let Some(items) = data.indi.camera_get_heater_items(&options.cam.device)? else { return Ok(()); };
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
    }));
}

fn select_maximum_resolution(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let indi = &data.indi;
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
    gtk_utils::exec_and_show_error(&data.window, || {
        data.state.start_live_view()?;
        gtk_utils::set_active_id(
            &data.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}

fn handler_action_take_shot(data: &Rc<CameraData>) {
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.window, || {
        data.state.start_single_shot()?;
        gtk_utils::set_active_id(
            &data.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}

fn handler_action_stop_shot(data: &Rc<CameraData>) {
    data.state.abort_active_mode();
}

fn create_and_show_preview_image(data: &Rc<CameraData>) {
    let options = data.options.read().unwrap();
    let preview_params = options.preview.preview_params();
    let (image, hist) = match options.preview.source {
        PreviewSource::OrigFrame =>
            (&data.state.cur_frame().image, &data.state.cur_frame().hist),
        PreviewSource::LiveStacking =>
            (&data.state.live_stacking().image, &data.state.live_stacking().hist),
    };
    drop(options);
    let image = image.read().unwrap();
    let hist = hist.read().unwrap();
    let rgb_bytes = get_rgb_bytes_from_preview_image(
        &image,
        &hist,
        &preview_params
    );
    show_preview_image(data, rgb_bytes, None);
}

fn show_preview_image(
    data:       &Rc<CameraData>,
    rgb_bytes:  RgbU8Data,
    src_params: Option<PreviewParams>,
) {
    let preview_options = data.options.read().unwrap().preview.clone();
    let pp = preview_options.preview_params();
    if src_params.is_some() && src_params.as_ref() != Some(&pp) {
        create_and_show_preview_image(data);
        return;
    }
    let img_preview = data.builder.object::<gtk::Image>("img_preview").unwrap();
    if rgb_bytes.bytes.is_empty() {
        img_preview.clear();
        return;
    }
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
    gtk_utils::enable_widgets(
        &data.builder, false,
        &[("cb_preview_color", rgb_bytes.is_color_image)]
    );
}

fn show_image_info(data: &Rc<CameraData>) {
    let info = match data.options.read().unwrap().preview.source {
        PreviewSource::OrigFrame =>
            data.state.cur_frame().info.read().unwrap(),
        PreviewSource::LiveStacking =>
            data.state.live_stacking().info.read().unwrap(),
    };

    let bldr = &data.builder;
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

fn handler_action_save_image_preview(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let options = data.options.read().unwrap();
        let (image, hist, fn_prefix) = match options.preview.source {
            PreviewSource::OrigFrame =>
                (&data.state.cur_frame().image, &data.state.cur_frame().hist, "preview"),
            PreviewSource::LiveStacking =>
                (&data.state.live_stacking().image, &data.state.live_stacking().hist, "live"),
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
            &data.window,
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
        let rgb_data = get_rgb_bytes_from_preview_image(&image, &hist, &preview_params);
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

fn handler_action_save_image_linear(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let options = data.options.read().unwrap();
        let preview_source = options.preview.source.clone();
        drop(options);
        let ask_to_select_name = |fn_prefix: &str| -> Option<PathBuf> {
            let def_file_name = format!(
                "{}_{}.tif",
                fn_prefix,
                Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string()
            );
            gtk_utils::select_file_name_to_save(
                &data.window,
                "Enter file name to save preview image as tiff",
                "Tiff images", "*.tif",
                "tif",
                &def_file_name,
            )
        };
        match preview_source {
            PreviewSource::OrigFrame => {
                let image = &data.state.cur_frame().image;
                if image.read().unwrap().is_empty() {
                    return Ok(());
                }
                let Some(file_name) = ask_to_select_name("preview") else {
                    return Ok(())
                };
                image.read().unwrap().save_to_tiff(&file_name)?;
            }
            PreviewSource::LiveStacking => {
                let adder = &data.state.live_stacking().adder;
                if adder.read().unwrap().is_empty() {
                    return Ok(());
                }
                let Some(file_name) = ask_to_select_name("live") else {
                    return Ok(())
                };
                adder.read().unwrap().save_to_tiff(&file_name)?;
            }
        }
        Ok(())
    });
}


fn show_frame_processing_result(
    data:   &Rc<CameraData>,
    result: FrameProcessResult
) {
    let options = data.options.read().unwrap();
    if options.cam.device != result.camera { return; }
    let live_stacking_res = options.preview.source == PreviewSource::LiveStacking;
    drop(options);

    let mode_type = data.state.mode_data().mode.get_type();

    let show_resolution_info = |width, height| {
        gtk_utils::set_str(
            &data.builder,
            "e_res_info",
            &format!("{} x {}", width, height)
        );
    };

    let is_mode_current = |mode: ModeType, live_result: bool| {
        mode_type == mode &&
        live_result == live_stacking_res
    };

    match result.data {
        FrameProcessResultData::Error(error_text) => {
            _ = data.state.abort_active_mode();
            correct_widgets_props(data);
            gtk_utils::show_error_message(&data.window, "Fatal Error", &error_text);
        }
        FrameProcessResultData::LightShortInfo(short_info, mode) => {
            if data.state.mode_data().mode.get_type() == mode {
                data.state.notify_about_light_short_info(&short_info);
            }
            data.light_history.borrow_mut().push(short_info);
            update_light_history_table(data);
        }
        FrameProcessResultData::PreviewFrame(img, mode) if is_mode_current(mode, false) => {
            show_preview_image(data, img.rgb_bytes, Some(img.params));
            show_resolution_info(img.image_width, img.image_height);
        }
        FrameProcessResultData::PreviewLiveRes(img, mode) if is_mode_current(mode, true) => {
            show_preview_image(data, img.rgb_bytes, Some(img.params));
            show_resolution_info(img.image_width, img.image_height);
        }
        FrameProcessResultData::Histogram(mode) if is_mode_current(mode, false) => {
            repaint_histogram(data);
            show_histogram_stat(data);
        }
        FrameProcessResultData::HistogramLiveRes(mode) if is_mode_current(mode, true) => {
            repaint_histogram(data);
            show_histogram_stat(data);
        }
        FrameProcessResultData::FrameInfo(mode) => {
            let info = data.state.cur_frame().info.read().unwrap();
            if let ResultImageInfo::LightInfo(info) = &*info {
                if data.state.mode_data().mode.get_type() == mode {
                    data.state.notify_about_light_frame_info(info);
                }
            }
            if is_mode_current(mode, false) {
                show_image_info(data);
            }
        }
        FrameProcessResultData::FrameInfoLiveRes(mode) if is_mode_current(mode, true) => {
            show_image_info(data);
        }
        FrameProcessResultData::MasterSaved { frame_type: FrameType::Flats, file_name } => {
            gtk_utils::set_path(
                &data.builder,
                "fch_master_flat",
                Some(&file_name)
            );
        }
        FrameProcessResultData::MasterSaved { frame_type: FrameType::Darks, file_name } => {
            gtk_utils::set_path(
                &data.builder,
                "fch_master_dark",
                Some(&file_name)
            );
        }
        _ => {}
    }
}

fn control_camera_by_options(
    data:      &Rc<CameraData>,
    force_set: bool,
) {
    let options = data.options.read().unwrap();
    let camera_name = &options.cam.device;
    if camera_name.is_empty() { return; };
    gtk_utils::exec_and_show_error(&data.window, || {
        // Cooler + Temperature
        if data.indi.camera_is_cooler_supported(camera_name)? {
            data.indi.camera_enable_cooler(
                camera_name,
                options.cam.ctrl.enable_cooler,
                true,
                SET_PROP_TIMEOUT
            )?;
            if options.cam.ctrl.enable_cooler {
                data.indi.camera_set_temperature(
                    camera_name,
                    options.cam.ctrl.temperature
                )?;
            }
        }
        // Fan
        if data.indi.camera_is_fan_supported(camera_name)? {
            data.indi.camera_control_fan(
                camera_name,
                options.cam.ctrl.enable_fan || options.cam.ctrl.enable_cooler,
                force_set,
                SET_PROP_TIMEOUT
            )?;
        }
        // Window heater
        if data.indi.camera_is_heater_supported(camera_name)? {
            if let Some(heater_str) = &options.cam.ctrl.heater_str {
                data.indi.camera_control_heater(
                    camera_name,
                    heater_str,
                    force_set,
                    SET_PROP_TIMEOUT
                )?;
            }
        }
        Ok(())
    });
}

fn show_cur_temperature_value(
    data:        &Rc<CameraData>,
    device_name: &str,
    temparature: f64
) {
    let cur_camera = gtk_utils::get_active_id(&data.builder, "cb_camera_list");
    if cur_camera.as_deref() == Some(device_name) {
        gtk_utils::set_str(
            &data.builder,
            "l_temp_value",
            &format!("T: {:.1}°C", temparature)
        );
    }
}

fn show_coolpwr_value(
    data:        &Rc<CameraData>,
    device_name: &str,
    pwr_str:     &str
) {
    let cur_camera = gtk_utils::get_active_id(&data.builder, "cb_camera_list");
    if cur_camera.as_deref() == Some(device_name) {
        gtk_utils::set_str(
            &data.builder,
            "l_coolpwr_value",
            &format!("Pwr: {}", pwr_str)
        );
    }
}

fn handler_live_view_changed(data: &Rc<CameraData>) {
    if data.options.read().unwrap().cam.live_view {
        read_options_from_widgets(data);
        start_live_view(data);
    } else {
        data.state.abort_active_mode();
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
    correct_widgets_props(data);
}

fn update_devices_list_and_props_by_drv_interface(
    data:          &Rc<CameraData>,
    drv_interface: indi_api::DriverInterface,
) {
    if drv_interface.contains(indi_api::DriverInterface::TELESCOPE) {
        data.delayed_actions.schedule(
            DelayedActionTypes::UpdateMountWidgets
        );
    }
    if drv_interface.contains(indi_api::DriverInterface::FOCUSER) {
        data.delayed_actions.schedule(
            DelayedActionTypes::UpdateFocList
        );
    }
    if drv_interface.contains(indi_api::DriverInterface::CCD) {
        data.delayed_actions.schedule(DelayedActionTypes::UpdateCamList);
        data.delayed_actions.schedule(DelayedActionTypes::FillHeaterItems);
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
    if indi_api::Connection::camera_is_heater_property(prop_name) && new_prop {
        data.delayed_actions.schedule(DelayedActionTypes::FillHeaterItems);
        data.delayed_actions.schedule(DelayedActionTypes::StartCooling);
    }
    if indi_api::Connection::camera_is_cooler_pwr_property(prop_name, elem_name) {
        show_coolpwr_value(data, device_name, &value.as_string());
    }

    // show_coolpwr_value

    match (prop_name, elem_name, value) {
        ("DRIVER_INFO", "DRIVER_INTERFACE", _) => {
            let flag_bits = value.as_i32().unwrap_or(0);
            let flags = indi_api::DriverInterface::from_bits_truncate(flag_bits as u32);
            if flags.contains(indi_api::DriverInterface::CCD) {
                data.delayed_actions.schedule(
                    DelayedActionTypes::UpdateCamList
                );
            }
            if flags.contains(indi_api::DriverInterface::FOCUSER) {
                data.delayed_actions.schedule(
                    DelayedActionTypes::UpdateFocList
                );
            }
            if flags.contains(indi_api::DriverInterface::TELESCOPE) {
                data.options.write().unwrap().mount.device = device_name.to_string();
                data.delayed_actions.schedule(
                    DelayedActionTypes::UpdateMountWidgets
                );
            }
        },
        ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE", indi_api::PropValue::Num(temp)) => {
            if new_prop {
                data.delayed_actions.schedule(
                    DelayedActionTypes::StartCooling
                );
            }
            show_cur_temperature_value(data, device_name, *temp);
        },
        ("CCD_COOLER", ..)
        if new_prop => {
            data.delayed_actions.schedule(DelayedActionTypes::StartCooling);
            data.delayed_actions.schedule(DelayedActionTypes::UpdateCtrlWidgets);
        },
        ("CCD_OFFSET", ..) | ("CCD_GAIN", ..) | ("CCD_CONTROLS", ..)
        if new_prop => {
            data.delayed_actions.schedule(
                DelayedActionTypes::UpdateCtrlWidgets
            );
        },
        ("CCD_EXPOSURE", ..) => {
            let options = data.options.read().unwrap();
            if new_prop && device_name == options.cam.device {
                data.delayed_actions.schedule_ex(
                    DelayedActionTypes::StartLiveView,
                    // 2000 ms pause to start live view from camera
                    // after connecting to INDI server
                    2000
                );
            } else {
                update_shot_state(data);
            }
        },

        ("CCD_RESOLUTION", ..)
        if new_prop => {
            data.delayed_actions.schedule(
                DelayedActionTypes::SelectMaxResolution
            );
        },

        ("CCD_BINNING", ..) |
        ("CCD_INFO", "CCD_MAX_X", ..) |
        ("CCD_INFO", "CCD_MAX_Y", ..) =>
            data.delayed_actions.schedule(
                DelayedActionTypes::UpdateResolutionList
            ),

        ("ABS_FOCUS_POSITION", ..) => {
            show_cur_focuser_value(&data);
                data.delayed_actions.schedule(
                    if new_prop { DelayedActionTypes::UpdateFocPosNew }
                    else { DelayedActionTypes::UpdateFocPos }
                );
        },
        ("FOCUS_MAX", ..) => {
            data.delayed_actions.schedule(
                DelayedActionTypes::UpdateFocPosNew
            );
        },
        ("CONNECTION", ..) => {
            let driver_interface = data.indi
                .get_driver_interface(device_name)
                .unwrap_or(indi_api::DriverInterface::empty());
            update_devices_list_and_props_by_drv_interface(data, driver_interface);
        }
        ("TELESCOPE_SLEW_RATE", ..) if new_prop => {
            data.delayed_actions.schedule(
                DelayedActionTypes::UpdateMountSpdList
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

fn show_histogram_stat(data: &Rc<CameraData>) {
    let options = data.options.read().unwrap();
    let hist = match options.preview.source {
        PreviewSource::OrigFrame =>
            data.state.cur_frame().raw_hist.read().unwrap(),
        PreviewSource::LiveStacking =>
            data.state.live_stacking().hist.read().unwrap(),
    };
    let bldr = &data.builder;
    let max = hist.max as f64;
    let show_chan_data = |chan: &Option<HistogramChan>, l_cap, l_mean, l_median, l_dev| {
        if let Some(chan) = chan.as_ref() {
            let median = chan.median();
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

fn repaint_histogram(data: &Rc<CameraData>) {
    let da_histogram = data.builder.object::<gtk::DrawingArea>("da_histogram").unwrap();
    da_histogram.queue_draw();
}

fn handler_draw_histogram(
    data: &Rc<CameraData>,
    area: &gtk::DrawingArea,
    cr:   &cairo::Context
) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let options = data.options.read().unwrap();
        let hist = match options.preview.source {
            PreviewSource::OrigFrame =>
                data.state.cur_frame().raw_hist.read().unwrap(),
            PreviewSource::LiveStacking =>
                data.state.live_stacking().hist.read().unwrap(),
        };
        paint_histogram(
            &hist,
            area,
            cr,
            area.allocated_width(),
            area.allocated_height(),
            options.hist.log_y,
        )?;
        Ok(())
    });
}

fn handler_action_start_live_stacking(data: &Rc<CameraData>) {
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.window, || {
        data.state.start_live_stacking()?;
        show_options(data);
        gtk_utils::set_active_id(
            &data.builder,
            "cb_preview_src",
            Some("live")
        );
        Ok(())
    });
}

fn handler_action_stop_live_stacking(data: &Rc<CameraData>) {
    data.state.abort_active_mode();
}

fn handler_action_continue_live_stacking(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        read_options_from_widgets(data);
        data.state.continue_prev_mode()?;
        Ok(())
    });
}

fn update_shot_state(data: &Rc<CameraData>) {
    let draw_area = data.builder.object::<gtk::DrawingArea>("da_shot_state").unwrap();
    draw_area.queue_draw();
}

fn handler_draw_shot_state(
    data: &Rc<CameraData>,
    area: &gtk::DrawingArea,
    cr:   &cairo::Context
) {
    let mode_data = data.state.mode_data();
    let Some(cur_exposure) = mode_data.mode.get_cur_exposure() else {
        return;
    };
    if cur_exposure < 1.0 { return; };
    let options = data.options.read().unwrap();
    if options.cam.device.is_empty() { return; }
    let Ok(exposure) = data.indi.camera_get_exposure(&options.cam.device) else { return; };
    let progress = ((cur_exposure - exposure) / cur_exposure).max(0.0).min(1.0);
    let text_to_show = format!("{:.0} / {:.0}", cur_exposure - exposure, cur_exposure);
    gtk_utils::exec_and_show_error(&data.window, || {
        gtk_utils::draw_progress_bar(area, cr, progress, &text_to_show)
    });
}

fn update_light_history_table(data: &Rc<CameraData>) {
    let tree: gtk::TreeView = data.builder.object("tv_light_history").unwrap();
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
        let noise_str = item.noise
            .map(|v| format!("{:.3}%", v))
            .unwrap_or_else(|| "???".to_string());
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
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.window, || {
        data.state.start_saving_raw_frames()?;
        show_options(data);
        gtk_utils::set_active_id(
            &data.builder,
            "cb_preview_src",
            Some("frame")
        );
        Ok(())
    });
}

fn handler_action_continue_save_raw_frames(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        read_options_from_widgets(data);
        data.state.continue_prev_mode()?;
        Ok(())
    });
}

fn handler_action_stop_save_raw_frames(data: &Rc<CameraData>) {
    data.state.abort_active_mode();
}

fn handler_action_clear_light_history(data: &Rc<CameraData>) {
    data.light_history.borrow_mut().clear();
    update_light_history_table(data);
}

fn show_total_raw_time(data: &Rc<CameraData>) {
    let options = data.options.read().unwrap();
    let total_time = options.cam.frame.exposure() * options.raw_frames.frame_cnt as f64;
    let text = format!(
        "{:.1}s x {} = {}",
        options.cam.frame.exposure(),
        options.raw_frames.frame_cnt,
        seconds_to_total_time_str(total_time, false)
    );
    gtk_utils::set_str(&data.builder, "l_raw_time_info", &text);
}

///////////////////////////////////////////////////////////////////////////////

fn init_focuser_widgets(data: &Rc<CameraData>) {
    let spb_foc_temp = data.builder.object::<gtk::SpinButton>("spb_foc_temp").unwrap();
    spb_foc_temp.set_range(1.0, 20.0);
    spb_foc_temp.set_digits(0);
    spb_foc_temp.set_increments(1.0, 5.0);

    let spb_foc_measures = data.builder.object::<gtk::SpinButton>("spb_foc_measures").unwrap();
    spb_foc_measures.set_range(7.0, 42.0);
    spb_foc_measures.set_digits(0);
    spb_foc_measures.set_increments(1.0, 10.0);

    let spb_foc_auto_step = data.builder.object::<gtk::SpinButton>("spb_foc_auto_step").unwrap();
    spb_foc_auto_step.set_range(1.0, 1_000_000.0);
    spb_foc_auto_step.set_digits(0);
    spb_foc_auto_step.set_increments(100.0, 1000.0);

    let spb_foc_exp = data.builder.object::<gtk::SpinButton>("spb_foc_exp").unwrap();
    spb_foc_exp.set_range(0.5, 60.0);
    spb_foc_exp.set_digits(1);
    spb_foc_exp.set_increments(0.5, 5.0);
}

fn update_focuser_devices_list(data: &Rc<CameraData>) {
    data.excl.exec(|| {
        let dev_list = data.indi.get_devices_list();
        let focusers = dev_list
            .iter()
            .filter(|device|
                device.interface.contains(indi_api::DriverInterface::FOCUSER)
            );
        let cb_foc_list: gtk::ComboBoxText =
            data.builder.object("cb_foc_list").unwrap();
        let last_active_id = cb_foc_list.active_id().map(|s| s.to_string());
        cb_foc_list.remove_all();
        for camera in focusers {
            cb_foc_list.append(Some(&camera.name), &camera.name);
        }
        let focusers_count = gtk_utils::combobox_items_count(&cb_foc_list);
        if focusers_count == 1 {
            cb_foc_list.set_active(Some(0));
        } else if focusers_count > 1 {
            let options = data.options.read().unwrap();
            if last_active_id.is_some() {
                cb_foc_list.set_active_id(last_active_id.as_deref());
            } else if !options.focuser.device.is_empty() {
                cb_foc_list.set_active_id(Some(options.focuser.device.as_str()));
            }
            if cb_foc_list.active_id().is_none() {
                cb_foc_list.set_active(Some(0));
            }
        }
        let connected = data.indi.state() == indi_api::ConnState::Connected;
        gtk_utils::enable_widgets(&data.builder, false, &[
            ("cb_foc_list", connected && focusers_count > 1),
        ]);
        data.options.write().unwrap().focuser.device =
            cb_foc_list.active_id().map(|s| s.to_string()).unwrap_or_else(String::new);
    });
}

fn update_focuser_position_widget(data: &Rc<CameraData>, new_prop: bool) {
    data.excl.exec(|| {
        let Some(foc_device) = gtk_utils::get_active_id(&data.builder, "cb_foc_list") else {
            return;
        };
        let Ok(prop_info) = data.indi.focuser_get_abs_value_prop_info(&foc_device) else {
            return;
        };
        let spb_foc_val = data.builder.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        if new_prop || spb_foc_val.value() == 0.0 {
            spb_foc_val.set_range(0.0, prop_info.max);
            spb_foc_val.set_digits(0);
            let step = prop_info.step.unwrap_or(1.0);
            spb_foc_val.set_increments(step, step * 10.0);
            let Ok(value) = data.indi.focuser_get_abs_value(&foc_device) else {
                return;
            };
            spb_foc_val.set_value(value);
        }
    });
    show_cur_focuser_value(data);
}

fn show_cur_focuser_value(data: &Rc<CameraData>) {
    let Some(foc_device) = gtk_utils::get_active_id(&data.builder, "cb_foc_list") else {
        return;
    };
    let Ok(value) = data.indi.focuser_get_abs_value(&foc_device) else {
        return;
    };
    let l_foc_value = data.builder.object::<gtk::Label>("l_foc_value").unwrap();
    l_foc_value.set_label(&format!("{:.0}", value));
}

fn update_focuser_position_after_focusing(data: &Rc<CameraData>, pos: f64) {
    data.excl.exec(|| {
        let spb_foc_val = data.builder.object::<gtk::SpinButton>("spb_foc_val").unwrap();
        spb_foc_val.set_value(pos);
    });
}

fn connect_focuser_widgets_events(data: &Rc<CameraData>) {
    let bldr = &data.builder;
    let spb_foc_val = bldr.object::<gtk::SpinButton>("spb_foc_val").unwrap();
    spb_foc_val.connect_value_changed(clone!(@weak data => move |sb| {
        data.excl.exec(|| {
            let Some(foc_device) = gtk_utils::get_active_id(&data.builder, "cb_foc_list") else {
                return;
            };
            gtk_utils::exec_and_show_error(&data.window, || {
                data.indi.focuser_set_abs_value(&foc_device, sb.value(), true, None)?;
                Ok(())
            })
        });
    }));

    let chb_foc_temp = bldr.object::<gtk::CheckButton>("chb_foc_temp").unwrap();
    chb_foc_temp.connect_active_notify(clone!(@weak data => move |_| {
        correct_widgets_props(&data);
    }));

    let chb_foc_fwhm = bldr.object::<gtk::CheckButton>("chb_foc_fwhm").unwrap();
    chb_foc_fwhm.connect_active_notify(clone!(@weak data => move |_| {
        correct_widgets_props(&data);
    }));

    let chb_foc_period = bldr.object::<gtk::CheckButton>("chb_foc_period").unwrap();
    chb_foc_period.connect_active_notify(clone!(@weak data => move |_| {
        correct_widgets_props(&data);
    }));

    let da_focusing = data.builder.object::<gtk::DrawingArea>("da_focusing").unwrap();
    da_focusing.connect_draw(clone!(@weak data => @default-return Inhibit(false), move |da, ctx| {
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
    gtk_utils::exec_and_show_error(&data.window, || {
        data.state.start_focusing()?;
        Ok(())
    });
}

fn handler_action_stop_manual_focus(data: &Rc<CameraData>) {
    data.state.abort_active_mode();
}

///////////////////////////////////////////////////////////////////////////////

fn init_dithering_widgets(data: &Rc<CameraData>) {
    let spb_guid_max_err = data.builder.object::<gtk::SpinButton>("spb_guid_max_err").unwrap();
    spb_guid_max_err.set_range(3.0, 50.0);
    spb_guid_max_err.set_digits(0);
    spb_guid_max_err.set_increments(1.0, 10.0);

    let spb_mnt_cal_exp = data.builder.object::<gtk::SpinButton>("spb_mnt_cal_exp").unwrap();
    spb_mnt_cal_exp.set_range(0.5, 10.0);
    spb_mnt_cal_exp.set_digits(1);
    spb_mnt_cal_exp.set_increments(0.5, 5.0);
}

fn connect_dithering_widgets_events(data: &Rc<CameraData>) {
    let chb_guid_enabled = data.builder.object::<gtk::CheckButton>("chb_guid_enabled").unwrap();
    chb_guid_enabled.connect_active_notify(clone!(@weak data => move |_| {
        correct_widgets_props(&data);
    }));
}

fn handler_action_start_dither_calibr(data: &Rc<CameraData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        data.state.start_mount_calibr()?;
        Ok(())
    });
}

fn handler_action_stop_dither_calibr(data: &Rc<CameraData>) {
    data.state.abort_active_mode();
}

///////////////////////////////////////////////////////////////////////////////

const MOUNT_NAV_BUTTON_NAMES: &[&'static str] = &[
    "btn_left_top",    "btn_top",        "btn_right_top",
    "btn_left",        "btn_stop_mount", "btn_right",
    "btn_left_bottom", "btn_bottom",     "btn_right_bottom",
];

fn connect_mount_widgets_events(data: &Rc<CameraData>) {
    for &btn_name in MOUNT_NAV_BUTTON_NAMES {
        let btn = data.builder.object::<gtk::Button>(btn_name).unwrap();
        btn.connect_button_press_event(clone!(@weak data => @default-return Inhibit(false), move |_, eb| {
            if eb.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                handler_nav_mount_btn_pressed(&data, btn_name);
            }
            Inhibit(false)
        }));
        btn.connect_button_release_event(clone!(@weak data => @default-return Inhibit(false), move |_, _| {
            handler_nav_mount_btn_released(&data, btn_name);
            Inhibit(false)
        }));
    }

    let chb_tracking = data.builder.object::<gtk::CheckButton>("chb_tracking").unwrap();
    chb_tracking.connect_active_notify(clone!(@weak data => move |chb| {
        data.excl.exec(|| {
            let options = data.options.read().unwrap();
            if options.mount.device.is_empty() { return; }
            gtk_utils::exec_and_show_error(&data.window, || {
                data.indi.mount_set_tracking(&options.mount.device, chb.is_active(), true, None)?;
                Ok(())
            });
        });
    }));

    let chb_parked = data.builder.object::<gtk::CheckButton>("chb_parked").unwrap();
    chb_parked.connect_active_notify(clone!(@weak data => move |chb| {
        let options = data.options.read().unwrap();
        if options.mount.device.is_empty() { return; }
        let parked = chb.is_active();
        gtk_utils::enable_widgets(&data.builder, true, &[
            ("chb_tracking", !parked),
            ("cb_mnt_speed", !parked),
            ("chb_inv_ns", !parked),
            ("chb_inv_we", !parked),
        ]);
        for &btn_name in MOUNT_NAV_BUTTON_NAMES {
            gtk_utils::set_bool_prop(&data.builder, btn_name, "sensitive", !parked);
        }
        data.excl.exec(|| {
            gtk_utils::exec_and_show_error(&data.window, || {
                data.indi.mount_set_parked(&options.mount.device, parked, true, None)?;
                Ok(())
            });
        });
    }));
}

fn handler_nav_mount_btn_pressed(data: &Rc<CameraData>, button_name: &str) {
    let options = data.options.read().unwrap();
    let mount_device_name = &options.mount.device;
    if mount_device_name.is_empty() { return; }
    gtk_utils::exec_and_show_error(&data.window, || {
        if button_name != "btn_stop_mount" {
            let inv_ns = gtk_utils::get_bool(&data.builder, "chb_inv_ns");
            let inv_we = gtk_utils::get_bool(&data.builder, "chb_inv_we");
            data.indi.mount_reverse_motion(
                mount_device_name,
                inv_ns,
                inv_we,
                false,
                SET_PROP_TIMEOUT
            )?;
            let speed = gtk_utils::get_active_id(&data.builder, "cb_mnt_speed");
            if let Some(speed) = speed {
                data.indi.mount_set_slew_speed(
                    mount_device_name,
                    &speed,
                    true, Some(100)
                )?
            }
        }
        match button_name {
            "btn_left_top" => {
                data.indi.mount_start_move_west(mount_device_name)?;
                data.indi.mount_start_move_north(mount_device_name)?;
            }
            "btn_top" => {
                data.indi.mount_start_move_north(mount_device_name)?;
            }
            "btn_right_top" => {
                data.indi.mount_start_move_east(mount_device_name)?;
                data.indi.mount_start_move_north(mount_device_name)?;
            }
            "btn_left" => {
                data.indi.mount_start_move_west(mount_device_name)?;
            }
            "btn_right" => {
                data.indi.mount_start_move_east(mount_device_name)?;
            }
            "btn_left_bottom" => {
                data.indi.mount_start_move_west(mount_device_name)?;
                data.indi.mount_start_move_south(mount_device_name)?;
            }
            "btn_bottom" => {
                data.indi.mount_start_move_south(mount_device_name)?;
            }
            "btn_right_bottom" => {
                data.indi.mount_start_move_south(mount_device_name)?;
                data.indi.mount_start_move_east(mount_device_name)?;
            }
            "btn_stop_mount" => {
                data.indi.mount_abort_motion(mount_device_name)?;
                data.indi.mount_stop_move(mount_device_name)?;
            }
            _ => {},
        };
        Ok(())
    });
}

fn handler_nav_mount_btn_released(data: &Rc<CameraData>, button_name: &str) {
    let options = data.options.read().unwrap();
    if options.mount.device.is_empty() { return; }
    gtk_utils::exec_and_show_error(&data.window, || {
        if button_name != "btn_stop_mount" {
            data.indi.mount_stop_move(&options.mount.device)?;
        }
        Ok(())
    });
}

fn fill_mount_speed_list_widget(data: &Rc<CameraData>) {
    let options = data.options.read().unwrap();
    if options.mount.device.is_empty() { return; }
    gtk_utils::exec_and_show_error(&data.window, || {
        let list = data.indi.mount_get_slew_speed_list(&options.mount.device)?;
        let cb_mnt_speed = data.builder.object::<gtk::ComboBoxText>("cb_mnt_speed").unwrap();
        cb_mnt_speed.remove_all();
        cb_mnt_speed.append(None, "---");
        for (id, text) in list {
            cb_mnt_speed.append(Some(&id), &text);
        }
        let options = data.options.read().unwrap();
        if options.mount.speed.is_some() {
            cb_mnt_speed.set_active_id(options.mount.speed.as_deref());
        } else {
            cb_mnt_speed.set_active(Some(0));
        }
        Ok(())
    });
}

fn show_mount_tracking_state(data: &Rc<CameraData>, tracking: bool) {
    let options = data.options.read().unwrap();
    if options.mount.device.is_empty() { return; }
    data.excl.exec(|| {
        gtk_utils::set_bool_prop(&data.builder, "chb_tracking", "active", tracking);
    });
}

fn show_mount_parked_state(data: &Rc<CameraData>, parked: bool) {
    let options = data.options.read().unwrap();
    if options.mount.device.is_empty() { return; }
    data.excl.exec(|| {
        gtk_utils::set_bool_prop(&data.builder, "chb_parked", "active", parked);
    });
}

fn handler_action_open_image(data: &Rc<CameraData>) {
    let fc = gtk::FileChooserDialog::builder()
        .action(gtk::FileChooserAction::Open)
        .title("Select image file to open")
        .modal(true)
        .transient_for(&data.window)
        .build();
    if cfg!(target_os = "windows") {
        fc.add_buttons(&[
            ("_Open", gtk::ResponseType::Accept),
            ("_Cancel", gtk::ResponseType::Cancel),
        ]);
    } else {
        fc.add_buttons(&[
            ("_Cancel", gtk::ResponseType::Cancel),
            ("_Open", gtk::ResponseType::Accept),
        ]);
    }
    fc.connect_response(clone!(@weak data => move |file_chooser, response| {
        if response == gtk::ResponseType::Accept {
            gtk_utils::exec_and_show_error(&data.window, || {
                let Some(file_name) = file_chooser.file() else { return Ok(()); };
                read_options_from_widgets(&data);
                let mut image = data.state.cur_frame().image.write().unwrap();
                image.load_from_file(&file_name.path().unwrap_or_default())?;
                let options = data.options.read().unwrap();
                if options.preview.remove_grad {
                    image.remove_gradient();
                }
                drop(image);

                let image = data.state.cur_frame().image.read().unwrap();
                let mut hist = data.state.cur_frame().hist.write().unwrap();
                hist.from_image(&image);
                drop(hist);
                let mut hist = data.state.cur_frame().raw_hist.write().unwrap();
                hist.from_image(&image);
                drop(hist);
                drop(image);

                create_and_show_preview_image(&data);
                show_histogram_stat(&data);
                repaint_histogram(&data);
                Ok(())
            });
        }
        file_chooser.close();
    }));
    fc.show();
}

fn correct_frame_quality_widgets_props(data: &Rc<CameraData>) {
    let bldr = &data.builder;
    gtk_utils::enable_widgets(bldr, true, &[
        ("spb_max_fwhm", gtk_utils::get_bool(bldr, "chb_max_fwhm")),
        ("spb_max_oval", gtk_utils::get_bool(bldr, "chb_max_oval")),
    ]);
}