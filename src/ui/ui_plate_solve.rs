use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use serde::{Deserialize, Serialize};

use crate::{
    core::{core::{Core, ModeType}, events::*},
    indi,
    options::*,
    utils::{gtk_utils, io_utils::*},
};

use super::{ui_main::*, utils::*};


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
        load_json_from_config_file(&mut ui_options, PlateSolveUi::CONF_FN)?;
        Ok(())
    });

    let obj = Rc::new(PlateSolveUi {
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

    obj.connect_core_and_indi_events();
    obj.connect_widgets_events();
    obj.connect_main_ui_events(handlers);
    obj.connect_delayed_actions_events();
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    CorrectWidgetsProps,
}

enum MainThreadEvent {
    Indi(indi::Event),
    Core(Event),
}

struct PlateSolveUi {
    builder:         gtk::Builder,
    window:          gtk::ApplicationWindow,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    ui_options:      RefCell<UiOptions>,
    closed:          Cell<bool>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    delayed_actions: DelayedActions<DelayedAction>,
    self_:           RefCell<Option<Rc<PlateSolveUi>>>,
}

impl Drop for PlateSolveUi {
    fn drop(&mut self) {
        log::info!("PlateSolveUi dropped");
    }
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

impl PlateSolveUi {
    const CONF_FN: &'static str = "ui_plate_solve";

    fn init_widgets(&self) {
        let spb_ps_exp = self.builder.object::<gtk::SpinButton>("spb_ps_exp").unwrap();
        spb_ps_exp.set_range(0.5, 30.0);
        spb_ps_exp.set_digits(1);
        spb_ps_exp.set_increments(0.5, 5.0);

        let spb_ps_timeout = self.builder.object::<gtk::SpinButton>("spb_ps_timeout").unwrap();
        spb_ps_timeout.set_range(5.0, 120.0);
        spb_ps_timeout.set_digits(0);
        spb_ps_timeout.set_increments(5.0, 20.0);

        let spb_ps_blind_timeout = self.builder.object::<gtk::SpinButton>("spb_ps_blind_timeout").unwrap();
        spb_ps_blind_timeout.set_range(5.0, 120.0);
        spb_ps_blind_timeout.set_digits(0);
        spb_ps_blind_timeout.set_increments(5.0, 20.0);
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

    fn connect_core_and_indi_events(self: &Rc<Self>) {
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

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action_rc(&self.window, self, "capture_platesolve",   Self::handler_action_capture_platesolve);
        gtk_utils::connect_action   (&self.window, self, "plate_solve_and_goto", Self::handler_action_plate_solve_and_goto);
    }

    fn connect_main_ui_events(self: &Rc<Self>, handlers: &mut MainUiEventHandlers) {
        handlers.subscribe(clone!(@weak self as self_ => move |event| {
            match event {
                UiEvent::ProgramClosing =>
                    self_.handler_closing(),

                UiEvent::OptionsHasShown =>
                    self_.correct_widgets_props(),

                _ => {},
            }
        }));
    }

    fn connect_delayed_actions_events(self: &Rc<Self>) {
        self.delayed_actions.set_event_handler(
            clone!(@weak self as self_ => move |action| {
                self_.handler_delayed_action(action);
            })
        );
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

    fn apply_ui_options(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.ui_options.borrow();
        ui.set_prop_bool("exp_plate_solving.expanded", options.expanded);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut options = self.ui_options.borrow_mut();
        options.expanded = ui.prop_bool("exp_plate_solving.expanded");
    }

    fn correct_widgets_props_impl(&self, mount_device: &str, cam_device: &Option<DeviceAndProp>) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let mnt_active = self.indi.is_device_enabled(mount_device).unwrap_or(false);
        let cam_active = cam_device.as_ref().map(|cam_device| self.indi.is_device_enabled(&cam_device.name).unwrap_or(false)).unwrap_or(false);
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;

        let plate_solve_sensitive =
            indi_connected &&
            mnt_active && cam_active &&
            (waiting || single_shot || live_view);

        if let Some(cam_device) = cam_device {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_device.prop);
            let exp_value = self.indi.camera_get_exposure_prop_value(&cam_device.name, cam_ccd);
            correct_spinbutton_by_cam_prop(&self.builder, "spb_ps_exp", &exp_value, 1, Some(1.0));
        }

        ui.enable_widgets(false, &[
            ("l_ps_cam_group", plate_solve_sensitive),
            ("l_ps_exp", plate_solve_sensitive),
            ("spb_ps_exp", plate_solve_sensitive),
            ("l_ps_gain", plate_solve_sensitive),
            ("cbx_ps_gain", plate_solve_sensitive),
            ("l_ps_bin", plate_solve_sensitive),
            ("cbx_ps_bin", plate_solve_sensitive),
        ]);

        gtk_utils::enable_actions(&self.window, &[
            ("capture_platesolve", plate_solve_sensitive),
            ("plate_solve_and_goto", plate_solve_sensitive)
        ]);
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let mount_device = options.mount.device.clone();
        let cam_device = options.cam.device.clone();
        drop(options);
        self.correct_widgets_props_impl(&mount_device, &cam_device);
    }

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
        }
    }

    fn handler_action_capture_platesolve(self: &Rc<Self>) {
        if !is_expanded(&self.builder, "exp_plate_solving") { return; }

        self.options.write().unwrap().read_all(&self.builder);
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_capture_and_platesolve()?;
            Ok(())
        });
    }

    fn handler_action_plate_solve_and_goto(&self) {
        if !is_expanded(&self.builder, "exp_plate_solving") { return; }

        self.options.write().unwrap().read_all(&self.builder);
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_goto_image()?;
            Ok(())
        });
    }
}