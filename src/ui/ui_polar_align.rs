use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use serde::{Deserialize, Serialize};

use crate::{
    core::{core::{Core, ModeType}, events::*, mode_polar_align::PolarAlignmentEvent},
    indi,
    options::*,
    utils::{gtk_utils, io_utils::*},
};

use super::{sky_map::math::HorizCoord, ui_main::*, utils::*};


pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    indi:     &Arc<indi::Connection>,
    handlers: &mut MainUiEventHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, PolarAlignUi::CONF_FN)?;
        Ok(())
    });

    let obj = Rc::new(PolarAlignUi {
        builder:         builder.clone(),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        ui_options:      RefCell::new(ui_options),
        closed:          Cell::new(false),
        indi_evt_conn:   RefCell::new(None),
        delayed_actions: DelayedActions::new(200),
        self_:           RefCell::new(None),
        window,
    });

    *obj.self_.borrow_mut() = Some(Rc::clone(&obj));

    obj.init_widgets();
    obj.apply_ui_options();

    obj.connect_main_ui_events(handlers);
    obj.connect_widgets_events();
    obj.connect_indi_and_core_events();
    obj.connect_delayed_actions_events();

    obj.correct_widgets_props();
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

struct PolarAlignUi {
    builder:         gtk::Builder,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    ui_options:      RefCell<UiOptions>,
    closed:          Cell<bool>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    delayed_actions: DelayedActions<DelayedAction>,
    self_:           RefCell<Option<Rc<PolarAlignUi>>>,
}

impl Drop for PolarAlignUi {
    fn drop(&mut self) {
        log::info!("PlateSolveUi dropped");
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
    }

    fn connect_main_ui_events(self: &Rc<Self>, handlers: &mut MainUiEventHandlers) {
        handlers.subscribe(clone!(@weak self as self_ => move |event| {
            match event {
                MainUiEvent::ProgramClosing =>
                    self_.handler_closing(),
                _ => {},
            }
        }));
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

        *self.self_.borrow_mut() = None;
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
        self.options.write().unwrap().read_all(&self.builder);
        gtk_utils::exec_and_show_error(&self.window, ||{
            self.core.start_polar_alignment()?;
            Ok(())
        });
    }

    fn handler_stop_action_polar_align(&self) {
        self.core.abort_active_mode();
    }

    fn show_polar_alignment_error(&self, error: &HorizCoord) {
        dbg!(error);

    }
}