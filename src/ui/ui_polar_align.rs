use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib::{self, clone}, pango, prelude::*};
use serde::{Deserialize, Serialize};
use crate::{
    core::{core::{Core, ModeType}, events::*, mode_polar_align::PolarAlignmentEvent},
    indi::{self, degree_to_str},
    options::*,
    sky_math::math::radian_to_degree,
    utils::{gtk_utils, io_utils::*}
};
use super::{sky_map::math::HorizCoord, ui_main::*, utils::*, module::*};

pub fn init_ui(
    builder: &gtk::Builder,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, PolarAlignUi::CONF_FN)?;
        Ok(())
    });

    let obj = Rc::new(PolarAlignUi {
        main_ui:         Rc::clone(main_ui),
        builder:         builder.clone(),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        ui_options:      RefCell::new(ui_options),
        closed:          Cell::new(false),
        indi_evt_conn:   RefCell::new(None),
        delayed_actions: DelayedActions::new(200),
        window,
    });

    obj.init_widgets();
    obj.apply_ui_options();

    obj.connect_widgets_events();
    obj.connect_indi_and_core_events();
    obj.connect_delayed_actions_events();

    obj
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    CorrectWidgetsProps,
}

enum MainThreadEvent {
    Indi(indi::Event),
    Core(Event),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct UiOptions {
    expanded: bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            expanded: false
        }
    }
}

impl PloarAlignDir {
    pub fn from_active_id(active_id: Option<&str>) -> Option<Self> {
        match active_id {
            Some("east") => Some(Self::East),
            Some("west") => Some(Self::West),
            _            => None,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::East  => Some("east"),
            Self::West  => Some("west"),
        }
    }
}

struct PolarAlignUi {
    main_ui:         Rc<MainUi>,
    builder:         gtk::Builder,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    ui_options:      RefCell<UiOptions>,
    closed:          Cell<bool>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    delayed_actions: DelayedActions<DelayedAction>,
}

impl Drop for PolarAlignUi {
    fn drop(&mut self) {
        log::info!("PolarAlignUi dropped");
    }
}

impl UiModule for PolarAlignUi {
    fn show_options(&self, options: &Options) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_f64("spb_pa_angle.value",       options.polar_align.angle);
        ui.set_prop_str("cbx_pa_dir.active-id",     options.polar_align.direction.to_active_id());
        ui.set_prop_str("cbx_pa_speed.active_id",   options.polar_align.speed.as_deref());
        ui.set_prop_f64("spb_pa_sim_alt_err.value", options.polar_align.sim_alt_err);
        ui.set_prop_f64("spb_pa_sim_az_err.value",  options.polar_align.sim_az_err);
    }

    fn get_options(&self, options: &mut Options) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        options.polar_align.angle       = ui.prop_f64("spb_pa_angle.value");
        options.polar_align.speed       = ui.prop_string("cbx_pa_speed.active_id");
        options.polar_align.sim_alt_err = ui.prop_f64("spb_pa_sim_alt_err.value");
        options.polar_align.sim_az_err  = ui.prop_f64("spb_pa_sim_az_err.value");
        options.polar_align.direction = PloarAlignDir::from_active_id(
            ui.prop_string("cbx_pa_dir.active-id").as_deref()
        ).unwrap_or(PloarAlignDir::West);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![]
    }

    fn process_event(&self, event: &UiModuleEvent) {
        match event {
            UiModuleEvent::ProgramClosing => {
                self.handler_closing();
            }

            _ => {}
        }
    }
}

impl PolarAlignUi {
    const CONF_FN: &str = "ui_ploar_align";

    fn init_widgets(&self) {
        let spb_pa_angle = self.builder.object::<gtk::SpinButton>("spb_pa_angle").unwrap();
        spb_pa_angle.set_range(15.0, 60.0);
        spb_pa_angle.set_digits(0);
        spb_pa_angle.set_increments(5.0, 15.0);

        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_str("l_pa_alt_err.label", Some(""));
        ui.set_prop_str("l_pa_az_err.label", Some(""));
        ui.set_prop_str("l_pa_alt_err_arr.label", Some(""));
        ui.set_prop_str("l_pa_az_err_arr.label", Some(""));

        if cfg!(debug_assertions) {
            ui.show_widgets(&[
                ("l_pa_sim_alt_err",   true),
                ("spb_pa_sim_alt_err", true),
                ("l_pa_sim_az_err",    true),
                ("spb_pa_sim_az_err",  true),
            ]);
        } else {
            // hide the expander in release mode because not everything is done
            ui.show_widgets(&[("exp_polar_align", false)]);
        }

        let spb_pa_sim_alt_err = self.builder.object::<gtk::SpinButton>("spb_pa_sim_alt_err").unwrap();
        spb_pa_sim_alt_err.set_range(-5.0, 5.0);
        spb_pa_sim_alt_err.set_digits(2);
        spb_pa_sim_alt_err.set_increments(0.01, 0.1);

        let spb_pa_sim_az_err = self.builder.object::<gtk::SpinButton>("spb_pa_sim_az_err").unwrap();
        spb_pa_sim_az_err.set_range(-5.0, 5.0);
        spb_pa_sim_az_err.set_digits(2);
        spb_pa_sim_az_err.set_increments(0.01, 0.1);
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        self.get_ui_options_from_widgets();
        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }
    }

    fn apply_ui_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.ui_options.borrow();
        ui.set_prop_bool("exp_polar_align.expanded", options.expanded);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut options = self.ui_options.borrow_mut();
        options.expanded = ui.prop_bool("exp_polar_align.expanded");
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action(&self.window, self, "start_polar_alignment", Self::handler_start_action_polar_align);
        gtk_utils::connect_action(&self.window, self, "stop_polar_alignment", Self::handler_stop_action_polar_align);

        let spb_pa_sim_alt_err = self.builder.object::<gtk::SpinButton>("spb_pa_sim_alt_err").unwrap();
        spb_pa_sim_alt_err.connect_value_changed(clone!(@weak self as self_ => move |spb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.polar_align.sim_alt_err = spb.value();
        }));

        let spb_pa_sim_az_err = self.builder.object::<gtk::SpinButton>("spb_pa_sim_az_err").unwrap();
        spb_pa_sim_az_err.connect_value_changed(clone!(@weak self as self_ => move |spb| {
            let Ok(mut options) = self_.options.try_write() else { return; };
            options.polar_align.sim_az_err = spb.value();
        }));
    }

    fn correct_widgets_props_impl(&self, mount_device: &str, cam_device: &Option<DeviceAndProp>) {
        let mnt_active = self.indi.is_device_enabled(mount_device).unwrap_or(false);
        let cam_active = cam_device.as_ref().map(|cam_device| self.indi.is_device_enabled(&cam_device.name).unwrap_or(false)).unwrap_or(false);
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;
        let polar_align = mode_type == ModeType::PolarAlignment;

        let polar_alignment_can_be_started =
            !polar_align &&
            indi_connected &&
            mnt_active && cam_active &&
            (waiting || single_shot || live_view);


        gtk_utils::enable_actions(&self.window, &[
            ("start_polar_alignment", polar_alignment_can_be_started),
            ("stop_polar_alignment",  polar_align),
        ]);
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let mount_device = options.mount.device.clone();
        let cam_device = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(&mount_device, &cam_device);
    }

    fn connect_delayed_actions_events(self: &Rc<Self>) {
        self.delayed_actions.set_event_handler(
            clone!(@weak self as self_ => move |action| {
                self_.handler_delayed_action(action);
            })
        );
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
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            MainThreadEvent::Core(Event::CameraDeviceChanged(cam_device)) => {
                let options = self.options.read().unwrap();
                let mount_device = options.mount.device.clone();
                drop(options);
                self.correct_widgets_props_impl(&mount_device, &Some(cam_device));
            }
            MainThreadEvent::Core(Event::MountDeviceSelected(mount_device)) => {
                let options = self.options.read().unwrap();
                let cam_device = options.cam.device.clone();
                drop(options);
                self.correct_widgets_props_impl(&mount_device, &cam_device);
            }
            MainThreadEvent::Core(Event::PolarAlignment(event)) => {
                match event {
                    PolarAlignmentEvent::Error(error) =>
                        self.show_polar_alignment_error(&error),
                }
            }
            MainThreadEvent::Indi(
                indi::Event::ConnChange(_)|
                indi::Event::DeviceConnected(_)|
                indi::Event::DeviceDelete(_)|
                indi::Event::NewDevice(_)
            ) => {
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            _ => {}
        }
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
        }
    }

    fn handler_start_action_polar_align(&self) {
        if !is_expanded(&self.builder, "exp_polar_align") { return; }

        self.main_ui.get_all_options();

        gtk_utils::exec_and_show_error(&self.window, ||{
            self.core.start_polar_alignment()?;
            Ok(())
        });
    }

    fn handler_stop_action_polar_align(&self) {
        if !is_expanded(&self.builder, "exp_polar_align") { return; }
        self.core.abort_active_mode();
    }

    fn show_polar_alignment_error(&self, error: &HorizCoord) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let alt_err_str = degree_to_str(radian_to_degree(error.alt));
        let az_err_str = degree_to_str(radian_to_degree(error.az));
        let alt_label = format!("Alt: {}", alt_err_str);
        let az_label = format!("Az: {}", az_err_str);
        ui.set_prop_str("l_pa_alt_err.label", Some(&alt_label));
        ui.set_prop_str("l_pa_az_err.label", Some(&az_label));

        let alt_err_arrow = if error.alt < 0.0 { "↑" } else { "↓" };
        let az_err_arrow = if error.az < 0.0 { "→" } else { "←" };
        ui.set_prop_str("l_pa_alt_err_arr.label", Some(&alt_err_arrow));
        ui.set_prop_str("l_pa_az_err_arr.label", Some(&az_err_arrow));

        let set_all_label_size = |label_name: &str, err: f64| {
            let err_minutes = f64::abs(radian_to_degree(err) * 60.0);
            let scale = if err_minutes > 60.0 {
                5
            } else if err_minutes > 2.0 {
                3
            } else {
                1
            };

            let alt_attrs = pango::AttrList::new();
            let attr_alt_size = pango::AttrSize::new(scale * 10 * pango::SCALE);
            alt_attrs.insert(attr_alt_size);

            let l_pa_alt_err_arr = self.builder.object::<gtk::Label>(label_name).unwrap();
            l_pa_alt_err_arr.set_attributes(Some(&alt_attrs));
        };

        set_all_label_size("l_pa_alt_err_arr", error.alt);
        set_all_label_size("l_pa_az_err_arr", error.az);
    }
}