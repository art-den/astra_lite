use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{core::*, events::*},
    guiding::external_guider::*,
    indi, options::*,
};

use super::{gtk_utils::*, module::*, ui_main::*, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/guiding.ui"));
    let info_widgets = InfoWidgets::new();

    let obj = Rc::new(GuidingUi {
        widgets,
        info_widgets,
        window:        window.clone(),
        main_ui:       Rc::clone(main_ui),
        options:       Arc::clone(options),
        core:          Arc::clone(core),
        indi:          Arc::clone(indi),
        closed:        Cell::new(false),
        indi_evt_conn: RefCell::new(None),
    });

    obj.init_widgets();
    obj.connect_widgets_events();
    obj.connect_indi_and_core_events();

    obj
}

#[derive(FromBuilder)]
struct Widgets {
    grd:                 gtk::Grid,
    cb_dith_perod:       gtk::ComboBoxText,
    rbtn_no_guiding:     gtk::RadioButton,
    rbtn_guide_main_cam: gtk::RadioButton,
    spb_dith_dist:       gtk::SpinButton,
    spb_guid_max_err:    gtk::SpinButton,
    spb_mnt_cal_exp:     gtk::SpinButton,
    cbx_mnt_cal_gain:    gtk::ComboBoxText,
    rbtn_guide_ext:      gtk::RadioButton,
    spb_ext_dith_dist:   gtk::SpinButton,
}

struct InfoWidgets {
    bx:     gtk::Box,
    l_info: gtk::Label,
}

impl InfoWidgets {
    fn new() -> Self {
        let bx = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(5)
            .visible(true)
            .build();
        let l_info = gtk::Label::builder()
            .label("info")
            .use_markup(true)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .width_chars(10)
            .max_width_chars(30)
            .visible(true)
            .build();
        bx.add(&l_info);
        Self { bx, l_info }
    }
}

struct GuidingUi {
    widgets:       Widgets,
    info_widgets:  InfoWidgets,
    main_ui:       Rc<MainUi>,
    window:        gtk::ApplicationWindow,
    options:       Arc<RwLock<Options>>,
    core:          Arc<Core>,
    indi:          Arc<indi::Connection>,
    closed:        Cell<bool>,
    indi_evt_conn: RefCell<Option<indi::Subscription>>,
}

enum MainThreadEvent {
    Core(Event),
    Indi(indi::Event),
}

impl Drop for GuidingUi {
    fn drop(&mut self) {
        log::info!("DitheringUi dropped");
    }
}

impl UiModule for GuidingUi {
    fn show_options(&self, options: &Options) {
        match options.guiding.mode {
            GuidingMode::Disabled =>
                self.widgets.rbtn_no_guiding.set_active(true),
            GuidingMode::MainCamera =>
                self.widgets.rbtn_guide_main_cam.set_active(true),
            GuidingMode::External =>
                self.widgets.rbtn_guide_ext.set_active(true),
        }
        self.widgets.cb_dith_perod    .set_active_id(Some(options.guiding.dith_period.to_string().as_str()));
        self.widgets.spb_ext_dith_dist.set_value    (options.guiding.ext_guider.dith_dist as f64);
        self.widgets.spb_guid_max_err .set_value    (options.guiding.main_cam.max_error);
        self.widgets.spb_dith_dist    .set_value    (options.guiding.main_cam.dith_dist as f64);
        self.widgets.cbx_mnt_cal_gain .set_active_id(Some(options.guiding.main_cam.calibr_gain.to_active_id()));
        set_spb_value(&self.widgets.spb_mnt_cal_exp, options.guiding.main_cam.calibr_exposure);
    }

    fn get_options(&self, options: &mut Options) {
        options.guiding.mode =
            if self.widgets.rbtn_guide_main_cam.is_active() {
                GuidingMode::MainCamera
            } else if self.widgets.rbtn_guide_ext.is_active() {
                GuidingMode::External
            } else {
                GuidingMode::Disabled
            };

        options.guiding.dith_period              = self.widgets.cb_dith_perod.active_id().and_then(|v| v.parse().ok()).unwrap_or(0);
        options.guiding.ext_guider.dith_dist     = self.widgets.spb_ext_dith_dist.value() as i32;

        options.guiding.main_cam.dith_dist       = self.widgets.spb_dith_dist.value() as i32;
        options.guiding.main_cam.calibr_exposure = self.widgets.spb_mnt_cal_exp.value();
        options.guiding.main_cam.calibr_gain     = Gain::from_active_id(self.widgets.cbx_mnt_cal_gain.active_id().as_deref());
        options.guiding.main_cam.max_error       = self.widgets.spb_guid_max_err.value();
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "guiding",
                name:   "Guiding".to_string(),
                widget: self.widgets.grd.clone().upcast(),
                pos:    PanelPosition::Right,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "guiding_info",
                name:   "Guiding".to_string(),
                widget: self.info_widgets.bx.clone().upcast(),
                pos:    PanelPosition::Bottom,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER|PanelFlags::INVISIBLE,
            },
        ]
    }

    fn on_show_options_first_time(&self) {
        self.correct_widgets_props();
    }

    fn on_app_closing(&self) {
        self.closed.set(true);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }

        let mut options = self.options.write().unwrap();
        if let Some(cur_cam_device) = options.cam.device.clone() {
            self.store_options_for_camera(&cur_cam_device, &mut options);
        }
        drop(options);
    }
}

impl GuidingUi {
    fn init_widgets(&self) {
        self.widgets.spb_guid_max_err.set_range(3.0, 50.0);
        self.widgets.spb_guid_max_err.set_digits(0);
        self.widgets.spb_guid_max_err.set_increments(1.0, 10.0);

        self.widgets.spb_mnt_cal_exp.set_range(0.5, 10.0);
        self.widgets.spb_mnt_cal_exp.set_digits(1);
        self.widgets.spb_mnt_cal_exp.set_increments(0.5, 5.0);

        self.widgets.spb_dith_dist.set_range(1.0, 300.0);
        self.widgets.spb_dith_dist.set_digits(0);
        self.widgets.spb_dith_dist.set_increments(1.0, 10.0);

        self.widgets.spb_ext_dith_dist.set_range(1.0, 300.0);
        self.widgets.spb_ext_dith_dist.set_digits(0);
        self.widgets.spb_ext_dith_dist.set_increments(1.0, 10.0);
    }

    fn connect_indi_and_core_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();

        let sender = main_thread_sender.clone();

        self.core.event_subscriptions().subscribe(move |event| {
            sender.send_blocking(MainThreadEvent::Core(event)).unwrap();
        });

        let sender = main_thread_sender.clone();
        *self.indi_evt_conn.borrow_mut() = Some(self.indi.subscribe_events(move |event| {
            sender.send_blocking(MainThreadEvent::Indi(event)).unwrap();
        }));

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                if self_.closed.get() { return; }
                self_.process_event_in_main_thread(event);
            }
        }));
    }

    fn process_event_in_main_thread(&self, event: MainThreadEvent) {
        match event {
            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_widgets_props();
            }
            MainThreadEvent::Core(Event::CameraDeviceChanged{ from, to }) => {
                self.handler_camera_changed(&from, &to);
            }
            MainThreadEvent::Core(Event::Guider(evt)) => {
                self.process_ext_guider_event(evt);
            }
            MainThreadEvent::Indi(indi::Event::ConnChange(_)) => {
                self.correct_widgets_props();
            }
            _ => {}
        }
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action(&self.window, self, "start_dither_calibr", Self::handler_action_start_dither_calibr);
        connect_action(&self.window, self, "stop_dither_calibr",  Self::handler_action_stop_dither_calibr);

        let connect_rbtn = |rbtn: &gtk::RadioButton| {
            let self_ = Rc::clone(self);
            rbtn.connect_active_notify(move |_| {
                self_.correct_widgets_props();
            });
        };

        connect_rbtn(&self.widgets.rbtn_no_guiding);
        connect_rbtn(&self.widgets.rbtn_guide_main_cam);
        connect_rbtn(&self.widgets.rbtn_guide_ext);
    }

    fn correct_widgets_props_impl(&self, cam_device: Option<&DeviceAndProp>) {
        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        drop(mode_data);
        let can_change_mode =
            mode_type == ModeType::Waiting ||
            mode_type == ModeType::SingleShot ||
            mode_type == ModeType::LiveView;
        let dither_calibr = mode_type == ModeType::DitherCalibr;
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let disabled = self.widgets.rbtn_no_guiding.is_active();
        let by_main_cam = self.widgets.rbtn_guide_main_cam.is_active();
        let by_ext = self.widgets.rbtn_guide_ext.is_active();

        if let Some(cam_device) = cam_device {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_device.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&cam_device.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&self.widgets.spb_mnt_cal_exp, &exp_value, 1, Some(1.0));
        }

        self.widgets.grd.set_sensitive(indi_connected);
        self.widgets.rbtn_no_guiding.set_sensitive(can_change_mode);
        self.widgets.rbtn_guide_main_cam.set_sensitive(can_change_mode);
        self.widgets.rbtn_guide_ext.set_sensitive(can_change_mode);
        self.widgets.cb_dith_perod.set_sensitive(!disabled && can_change_mode);
        self.widgets.spb_dith_dist.set_sensitive(by_main_cam && can_change_mode);
        self.widgets.spb_guid_max_err.set_sensitive(by_main_cam && can_change_mode);
        self.widgets.spb_mnt_cal_exp.set_sensitive(by_main_cam && can_change_mode);
        self.widgets.spb_ext_dith_dist.set_sensitive(by_ext && can_change_mode);

        enable_actions(&self.window, &[
            ("start_dither_calibr", !dither_calibr && by_main_cam && can_change_mode),
            ("stop_dither_calibr", dither_calibr),
        ]);
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let cam_device = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(cam_device.as_ref());
    }

    fn handler_camera_changed(&self, from: &Option<DeviceAndProp>, to: &DeviceAndProp) {
        let mut options = self.options.write().unwrap();
        self.get_options(&mut options);
        if let Some(from) = from {
            self.store_options_for_camera(from, &mut options);
        }
        self.restore_options_for_camera(to, &mut options);
        self.show_options(&options);
        drop(options);
        self.correct_widgets_props_impl(Some(to));
    }

    fn store_options_for_camera(
        &self,
        device:  &DeviceAndProp,
        options: &mut Options
    ) {
        let key = device.to_file_name_part();
        let sep_options = options.sep_guiding.entry(key).or_default();
        sep_options.exposure = options.guiding.main_cam.calibr_exposure;
        sep_options.gain = options.guiding.main_cam.calibr_gain;
    }

    fn restore_options_for_camera(
        &self,
        device:  &DeviceAndProp,
        options: &mut Options
    ) {
        let key = device.to_file_name_part();
        if let Some(sep_options) = options.sep_guiding.get(&key) {
            options.guiding.main_cam.calibr_exposure = sep_options.exposure;
            options.guiding.main_cam.calibr_gain = sep_options.gain;
        }
    }

    fn handler_action_start_dither_calibr(&self) {
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), || {
            self.core.start_mount_calibr()?;
            Ok(())
        });
    }

    fn handler_action_stop_dither_calibr(&self) {
        self.core.abort_active_mode();
    }

    fn process_ext_guider_event(&self, evt: ExtGuiderEvent) {
        match evt {
            ExtGuiderEvent::State(state) =>
                self.show_ext_guider_state(state),
            ExtGuiderEvent::Error(err) =>
                self.show_ext_guider_error(&err),
            ExtGuiderEvent::DitheringFinishedWithErr(err) =>
                self.show_ext_guider_error(&err),
            ExtGuiderEvent::Connected => {
                if let Some(state) = self.core.ext_giuder().state() {
                    self.show_ext_guider_state(state);
                }
            }
            ExtGuiderEvent::Disconnected =>
                self.show_info_text("Disconnected", Some(get_err_color_str())),
            _ => {}
        }
    }

    fn show_ext_guider_error(&self, err: &str) {
        self.show_info_text(err, Some(get_err_color_str()));
    }

    fn show_ext_guider_state(&self, state: ExtGuiderState) {
        let color = match state {
            ExtGuiderState::Guiding =>
                Some(get_ok_color_str()),
            ExtGuiderState::Stopped|
            ExtGuiderState::Looping =>
                Some(get_warn_color_str()),
            _ =>
                None,
        };
        self.show_info_text(&format!("{:?}", state), color);
    }

    fn show_info_text(&self, text: &str, color: Option<&str>) {
        let mut text_str = format!("<b>{}</b>", text);
        if let Some(color) = color {
            text_str = format!("<span foreground='{}'>{}</span>", color, text_str);
        }
        self.info_widgets.l_info.set_label(&text_str);
        self.info_widgets.l_info.set_tooltip_text(Some(text));
    }
}
