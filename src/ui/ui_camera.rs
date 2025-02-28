use std::{rc::{Rc, Weak}, sync::{Arc, RwLock}, cell::{RefCell, Cell}};
use gtk::{cairo, glib::{self, clone}, prelude::*};
use macros::FromBuilder;
use serde::{Serialize, Deserialize};
use crate::{
    core::{consts::*, core::*, events::*, frame_processing::*},
    image::{info::*, raw::FrameType},
    indi,
    options::*,
    utils::{gtk_utils, io_utils::*}
};
use super::{ui_main::*, ui_start_dialog::StartDialog, utils::*};

pub fn init_ui(
    window:     &gtk::ApplicationWindow,
    options:    &Arc<RwLock<Options>>,
    core:       &Arc<Core>,
    indi:       &Arc<indi::Connection>,
    handlers:   &mut MainUiEventHandlers,
    ui_modules: Weak<UiModules>,
) -> Rc<dyn UiModule> {
    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(window, || {
        load_json_from_config_file(&mut ui_options, CameraUi::CONF_FN)?;
        Ok(())
    });

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
        window:             window.clone(),
        core:               Arc::clone(core),
        indi:               Arc::clone(indi),
        widgets,
        ui_modules,
        options:            Arc::clone(options),
        delayed_actions:    DelayedActions::new(500),
        ui_options:         RefCell::new(ui_options),
        conn_state:         RefCell::new(indi::ConnState::Disconnected),
        indi_evt_conn:      RefCell::new(None),
        closed:             Cell::new(false),
        full_screen_mode:   Cell::new(false),
        self_:              RefCell::new(None),
    });

    *obj.self_.borrow_mut() = Some(Rc::clone(&obj));

    obj.init_cam_ctrl_widgets();
    obj.init_cam_widgets();
    obj.init_raw_widgets();
    obj.init_live_stacking_widgets();
    obj.init_frame_quality_widgets();

    obj.show_ui_options();
    obj.connect_common_events();
    obj.connect_widgets_events();
    obj.connect_main_ui_events(handlers);

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
        }
    }
}

enum MainThreadEvent {
    Core(Event),
    Indi(indi::Event),
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
}

#[derive(FromBuilder)]
struct FrameWidgets {
    grid:       gtk::Grid,
    l_mode:     gtk::Label,
    cb_mode:    gtk::ComboBoxText,
    l_exp:      gtk::Label,
    spb_exp:    gtk::SpinButton,
    l_gain:     gtk::Label,
    spb_gain:   gtk::SpinButton,
    l_offset:   gtk::Label,
    spb_offset: gtk::SpinButton,
    l_bin:      gtk::Label,
    cb_bin:     gtk::ComboBoxText,
    l_crop:     gtk::Label,
    cb_crop:    gtk::ComboBoxText,
}

#[derive(FromBuilder)]
struct CalibrWidgets {
    grid:              gtk::Grid,
    chb_master_dark:   gtk::CheckButton,
    chb_master_flat:   gtk::CheckButton,
    fch_master_flat:   gtk::FileChooserButton,
    chb_hot_pixels:    gtk::CheckButton,
    l_hot_pixels_warn: gtk::Label,
}

#[derive(FromBuilder)]
struct RawWidgets {
    grid:             gtk::Grid,
    l_time_info:      gtk::Label,
    btn_start:        gtk::Button,
    btn_continue:     gtk::Button,
    chb_frames_cnt:   gtk::CheckButton,
    spb_frames_cnt:   gtk::SpinButton,
    fcb_path:         gtk::FileChooserButton,
    chb_master_frame: gtk::CheckButton,
}

#[derive(FromBuilder)]
struct LiveStWidgets {
    grid:            gtk::Grid,
    chb_period:      gtk::CheckButton,
    chb_save_period: gtk::SpinButton,
    chb_save_orig:   gtk::CheckButton,
    chb_no_tracks:   gtk::CheckButton,
    l_no_tracks:     gtk::Label,
    fch_path:        gtk::FileChooserButton,
}

#[derive(FromBuilder)]
struct QualityWidgets {
    bx:           gtk::Box,
    chb_max_fwhm: gtk::CheckButton,
    spb_max_fwhm: gtk::SpinButton,
    chb_max_oval: gtk::CheckButton,
    spb_max_oval: gtk::SpinButton,
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
    window:            gtk::ApplicationWindow,
    options:           Arc<RwLock<Options>>,
    core:              Arc<Core>,
    indi:              Arc<indi::Connection>,
    widgets:           Widgets,
    ui_modules:        Weak<UiModules>,
    delayed_actions:   DelayedActions<DelayedAction>,
    ui_options:        RefCell<UiOptions>,
    conn_state:        RefCell<indi::ConnState>,
    indi_evt_conn:     RefCell<Option<indi::Subscription>>,
    closed:            Cell<bool>,
    full_screen_mode:  Cell<bool>,
    self_:             RefCell<Option<Rc<CameraUi>>>,
}

impl UiModule for CameraUi {
    fn show_options(&self, options: &Options) {
    }

    fn get_options(&self, options: &mut Options) {
    }

    fn connect_ui_events(&self) {

    }
}

impl Drop for CameraUi {
    fn drop(&mut self) {
        log::info!("CameraUi dropped");
    }
}

impl CameraUi {
    const CONF_FN: &'static str = "ui_camera";

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
        self.widgets.live_st.chb_save_period.set_range(1.0, 60.0);
        self.widgets.live_st.chb_save_period.set_digits(0);
        self.widgets.live_st.chb_save_period.set_increments(1.0, 10.0);
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
        gtk_utils::connect_action   (&self.window, self, "take_shot",              Self::handler_action_take_shot);
        gtk_utils::connect_action   (&self.window, self, "stop_shot",              Self::handler_action_stop_shot);
        gtk_utils::connect_action_rc(&self.window, self, "start_save_raw_frames",  Self::handler_action_start_save_raw_frames);
        gtk_utils::connect_action   (&self.window, self, "stop_save_raw_frames",   Self::handler_action_stop_save_raw_frames);
        gtk_utils::connect_action   (&self.window, self, "continue_save_raw",      Self::handler_action_continue_save_raw_frames);
        gtk_utils::connect_action_rc(&self.window, self, "start_live_stacking",    Self::handler_action_start_live_stacking);
        gtk_utils::connect_action   (&self.window, self, "stop_live_stacking",     Self::handler_action_stop_live_stacking);
        gtk_utils::connect_action   (&self.window, self, "continue_live_stacking", Self::handler_action_continue_live_stacking);

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
                if options.cam.device.as_ref() == Some(&new_device) {
                    return;
                }

                // Store previous camera options into UiOptions::all_cam_opts

                if let Some(prev_cam) = options.cam.device.clone() {
                    // TODO !!!
                    //options.read_focuser_cam(&self_.builder);
                    //options.read_guiding_cam(&self_.builder);
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

                // TODO: !!!
                //options.show_cam_frame(&self_.builder);
                //options.show_calibr(&self_.builder);
                //options.show_cam_ctrl(&self_.builder);

                drop(options);

                self_.correct_widgets_props_impl(&Some(new_device.clone()));
                self_.correct_frame_quality_widgets_props();

                self_.core.event_subscriptions().notify(Event::CameraDeviceChanged(new_device));
            })
        );

        self.widgets.common.chb_live_view.connect_active_notify(
            clone!(@weak self as self_ => move |_| {
                self_.get_options_from_widgets();
                self_.correct_widgets_props();
                self_.handler_live_view_changed();
            })
        );

        self.widgets.ctrl.chb_cooler.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.ctrl.enable_cooler = chb.is_active();
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
                drop(options);
                self_.control_camera_by_options(false);
            })
        );

        self.widgets.ctrl.chb_low_noise.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.low_noise = chb.is_active();
            })
        );

        self.widgets.frame.cb_mode.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let frame_type = FrameType::from_active_id(cb.active_id().as_deref());
                options.cam.frame.frame_type = frame_type;
                self_.widgets.frame.spb_exp.set_value(options.cam.frame.exposure());
                drop(options);

                self_.correct_widgets_props();
                self_.show_total_raw_time();
            })
        );

        self.widgets.frame.spb_exp.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.set_exposure(sb.value());
                drop(options);

                self_.show_total_raw_time();
            })
        );

        self.widgets.frame.spb_gain.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.gain = sb.value();
            })
        );

        self.widgets.frame.spb_offset.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.frame.offset = sb.value() as i32;
            })
        );

        self.widgets.frame.cb_bin.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let binning = Binning::from_active_id(cb.active_id().as_deref());
                options.cam.frame.binning = binning;
            })
        );

        self.widgets.frame.cb_crop.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                let crop = Crop::from_active_id(cb.active_id().as_deref());
                options.cam.frame.crop = crop;
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

        self.widgets.calibr.chb_master_dark.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.dark_frame_en = chb.is_active();
            })
        );

        self.widgets.calibr.chb_master_flat.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.flat_frame_en = chb.is_active();
            })
        );

        self.widgets.calibr.fch_master_flat.connect_file_set(
            clone!(@weak self as self_ => move |fch| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.flat_frame_fname = fch.filename();
            })
        );

        self.widgets.calibr.chb_hot_pixels.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.widgets.calibr.l_hot_pixels_warn.set_visible(chb.is_active());
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.calibr.hot_pixels = chb.is_active();
                drop(options);
            })
        );

        self.widgets.live_st.chb_no_tracks.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            self_.widgets.live_st.l_no_tracks.set_visible(chb.is_active());
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

    fn handler_main_ui_event(&self, event: UiEvent) {
        match event {
            UiEvent::Timer => {}
            UiEvent::FullScreen(full_screen) =>
                self.set_full_screen_mode(full_screen),
            UiEvent::BeforeModeContinued =>
                self.get_options_from_widgets(),
            UiEvent::TabPageChanged(TabPage::Camera) =>
                self.correct_widgets_props(),
            UiEvent::ProgramClosing =>
                self.handler_closing(),
            UiEvent::BeforeDisconnect => {
                self.get_options_from_widgets();
                self.store_cur_cam_options();
            },
            UiEvent::OptionsHasShown => {
                self.correct_widgets_props();
                self.show_total_raw_time();
            }
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

            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_widgets_props();
            },

            MainThreadEvent::Core(Event::ModeContinued) => {
                let options = self.options.read().unwrap();
                // TODO: !!!
                //options.show_cam_frame(&self.builder);
                drop(options);
            },

            MainThreadEvent::Core(Event::FrameProcessing(result)) => {
                self.show_frame_processing_result(result);
            }
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

    fn set_full_screen_mode(&self, full_screen: bool) {
        // TODO:
        /*
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
        */
        self.full_screen_mode.set(full_screen);
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        _ = self.core.stop_img_process_thread();

        _ = self.core.abort_active_mode();

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
        // TODO: !!!
        /*
        let options = self.options.read().unwrap();
        options.show_cam(&self.builder);
        options.show_raw(&self.builder);
        options.show_live_stacking(&self.builder);
        options.show_frame_quality(&self.builder);
        options.show_preview(&self.builder);
        options.show_guiding(&self.builder);
        */
    }

    fn show_ui_options(&self) {
        // TODO: !!!
        /*
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
        */
    }

    fn get_options_from_widgets(&self) {
        // TODO: !!!

        //let Ok(mut options) = self.options.try_write() else { return; };
        //options.read_all(&self.builder);
    }

    fn get_ui_options_from_widgets(&self) {
        // TODO: !!!
        /*
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
        */
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
        let widgets = &self.widgets;

        let temp_supported = camera.as_ref().map(|camera| {
            let temp_value = self.indi.camera_get_temperature_prop_value(&camera.name);
            correct_spinbutton_by_cam_prop(&widgets.ctrl.spb_temp, &temp_value, 0, Some(1.0))
        }).unwrap_or(false);
        let exposure_supported = camera.as_ref().map(|camera| {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&camera.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&camera.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&widgets.frame.spb_exp, &exp_value, 3, Some(1.0))
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

        widgets.ctrl.chb_fan      .set_visible(fan_supported);
        widgets.ctrl.l_heater     .set_visible(heater_supported);
        widgets.ctrl.cb_heater    .set_visible(heater_supported);
        widgets.ctrl.chb_low_noise.set_visible(low_noise_supported);

        widgets.common.l_cam_list .set_sensitive(waiting && indi_connected);
        widgets.common.cb_cam_list.set_sensitive(waiting && indi_connected);
        widgets.ctrl.chb_fan      .set_sensitive(!cooler_active);
        widgets.ctrl.chb_cooler   .set_sensitive(temp_supported && can_change_cam_opts);
        widgets.ctrl.spb_temp     .set_sensitive(cooler_active && temp_supported && can_change_cam_opts);

        ui.enable_widgets(false, &[
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

    fn handler_action_start_live_stacking(self: &Rc<Self>) {
        if !is_expanded(&self.builder, "exp_live") { return; }

        self.get_options_from_widgets();
        let info_pairs = self.get_short_info_pairs(true);
        let dialog = StartDialog::new(
            self.window.upcast_ref(),
            "Start live stacking",
            &info_pairs
        );
        dialog.exec(clone!(@strong self as self_ => move || {
            self_.core.start_live_stacking()?;
            self_.show_options();
            Ok(())
        }));
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
        if !is_expanded(&self.builder, "exp_raw_frames") { return; }

        self.get_options_from_widgets();
        let info_pairs = self.get_short_info_pairs(false);
        let dialog = StartDialog::new(
            self.window.upcast_ref(),
            "Start save RAW files",
            &info_pairs
        );
        dialog.exec(clone!(@strong self as self_ => move || {
            self_.core.start_saving_raw_frames()?;
            self_.show_options();
            Ok(())
        }));
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

    fn show_frame_processing_result(&self, result: FrameProcessResult) {
        match result.data {
            FrameProcessResultData::Error(error_text) => {
                _ = self.core.abort_active_mode();
                self.correct_widgets_props();
                gtk_utils::show_error_message(&self.window, "Fatal Error", &error_text);
            }

            _ => {}
        }
    }
}