use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib::{self, clone}, pango, prelude::*};
use macros::FromBuilder;
use crate::{
    core::{core::{Core, ModeType}, events::*, mode_polar_align::PolarAlignmentEvent},
    indi::{self, degree_to_str},
    options::*,
    sky_math::math::*,
};
use super::{gtk_utils::*, module::*, ui_main::*, utils::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/polar_align.ui"));
    let obj = Rc::new(PolarAlignUi {
        widgets,
        window:          window.clone(),
        main_ui:         Rc::clone(main_ui),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        closed:          Cell::new(false),
        indi_evt_conn:   RefCell::new(None),
        delayed_actions: DelayedActions::new(200),
    });

    obj.init_widgets();
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

#[derive(FromBuilder)]
struct Widgets {
    bx:              gtk::Box,
    spb_angle:       gtk::SpinButton,
    cbx_dir:         gtk::ComboBoxText,
    cbx_speed:       gtk::ComboBoxText,
    l_sim_alt_err:   gtk::Label,
    spb_sim_alt_err: gtk::SpinButton,
    l_sim_az_err:    gtk::Label,
    spb_sim_az_err:  gtk::SpinButton,
    l_alt_err:       gtk::Label,
    l_az_err:        gtk::Label,
    l_alt_err_arr:   gtk::Label,
    l_az_err_arr:    gtk::Label,
}

struct PolarAlignUi {
    widgets:         Widgets,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
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
        self.widgets.spb_angle.set_value(options.polar_align.angle);
        self.widgets.cbx_dir.set_active_id(options.polar_align.direction.to_active_id());
        self.widgets.cbx_speed.set_active_id(options.polar_align.speed.as_deref());
        self.widgets.spb_sim_alt_err.set_value(options.polar_align.sim_alt_err);
        self.widgets.spb_sim_az_err.set_value(options.polar_align.sim_az_err);
    }

    fn get_options(&self, options: &mut Options) {
        options.polar_align.angle       = self.widgets.spb_angle.value();
        options.polar_align.speed       = self.widgets.cbx_speed.active_id().map(|s| s.to_string());
        options.polar_align.sim_alt_err = self.widgets.spb_sim_alt_err.value();
        options.polar_align.sim_az_err  = self.widgets.spb_sim_az_err.value();
        options.polar_align.direction = PloarAlignDir::from_active_id(
            self.widgets.cbx_dir.active_id().as_deref()
        ).unwrap_or(PloarAlignDir::West);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "polar_align",
            name:   "Polar alignment".to_string(),
            widget: self.widgets.bx.clone().upcast(),
            pos:    PanelPosition::Right,
            tab:    TabPage::Main,
            flags:  PanelFlags::empty(),
        }]
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

impl PolarAlignUi {
    fn init_widgets(&self) {
        self.widgets.spb_angle.set_range(15.0, 60.0);
        self.widgets.spb_angle.set_digits(0);
        self.widgets.spb_angle.set_increments(5.0, 15.0);

        self.widgets.l_alt_err.set_label("");
        self.widgets.l_az_err.set_label("");
        self.widgets.l_alt_err_arr.set_label("");
        self.widgets.l_az_err_arr.set_label("");

        if cfg!(debug_assertions) {
            self.widgets.l_sim_alt_err.set_visible(true);
            self.widgets.spb_sim_alt_err.set_visible(true);
            self.widgets.l_sim_az_err.set_visible(true);
            self.widgets.spb_sim_az_err.set_visible(true);
        }

        self.widgets.spb_sim_alt_err.set_range(-5.0, 5.0);
        self.widgets.spb_sim_alt_err.set_digits(2);
        self.widgets.spb_sim_alt_err.set_increments(0.01, 0.1);

        self.widgets.spb_sim_az_err.set_range(-5.0, 5.0);
        self.widgets.spb_sim_az_err.set_digits(2);
        self.widgets.spb_sim_az_err.set_increments(0.01, 0.1);
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action(&self.window, self, "start_polar_alignment", Self::handler_start_action_polar_align);
        connect_action(&self.window, self, "restart_polar_alignment", Self::handler_restart_action_polar_align);
        connect_action(&self.window, self, "stop_polar_alignment", Self::handler_stop_action_polar_align);

        self.widgets.spb_sim_alt_err.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.polar_align.sim_alt_err = spb.value();
            })
        );

        self.widgets.spb_sim_az_err.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.polar_align.sim_az_err = spb.value();
            })
        );
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


        enable_actions(&self.window, &[
            ("start_polar_alignment",   polar_alignment_can_be_started),
            ("restart_polar_alignment", polar_align),
            ("stop_polar_alignment",    polar_align),
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
            MainThreadEvent::Core(Event::CameraDeviceChanged{ to, ..}) => {
                let options = self.options.read().unwrap();
                let mount_device = options.mount.device.clone();
                drop(options);
                self.correct_widgets_props_impl(&mount_device, &Some(to));
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
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), ||{
            self.core.start_polar_alignment()?;
            Ok(())
        });
    }

    fn handler_restart_action_polar_align(&self) {
        self.main_ui.get_all_options();

        exec_and_show_error(Some(&self.window), ||{
            self.core.restart_polar_alignment()?;
            Ok(())
        });
    }

    fn handler_stop_action_polar_align(&self) {
        self.core.abort_active_mode();
    }

    fn show_polar_alignment_error(&self, error: &Option<HorizCoord>) {
        if let Some(error) = error {
            let alt_err_str = degree_to_str(radian_to_degree(error.alt));
            let az_err_str = degree_to_str(radian_to_degree(error.az));
            let alt_label = format!("Alt: {}", alt_err_str);
            let az_label = format!("Az: {}", az_err_str);
            self.widgets.l_alt_err.set_label(&alt_label);
            self.widgets.l_az_err.set_label(&az_label);

            let alt_err_arrow = if error.alt < 0.0 { "↑" } else { "↓" };
            let az_err_arrow = if error.az < 0.0 { "→" } else { "←" };
            self.widgets.l_alt_err_arr.set_label(&alt_err_arrow);
            self.widgets.l_az_err_arr.set_label(&az_err_arrow);

            let set_all_label_size = |label: &gtk::Label, err: f64| {
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

                label.set_attributes(Some(&alt_attrs));
            };

            set_all_label_size(&self.widgets.l_alt_err_arr, error.alt);
            set_all_label_size(&self.widgets.l_az_err_arr, error.az);
        } else {
            self.widgets.l_alt_err.set_label("---");
            self.widgets.l_az_err.set_label("---");
            self.widgets.l_alt_err_arr.set_text("");
            self.widgets.l_az_err_arr.set_text("");
        }
    }
}