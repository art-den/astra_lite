use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use serde::{Deserialize, Serialize};

use crate::{
    core::{consts::INDI_SET_PROP_TIMEOUT, core::{Core, ModeType}},
    indi,
    options::*,
    utils::io_utils::*,
};

use super::{gtk_utils, utils::*, ui_main::*};


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
        load_json_from_config_file(&mut ui_options, MountUi::CONF_FN)?;
        Ok(())
    });

    let data = Rc::new(MountUi {
        builder:         builder.clone(),
        window,
        excl:            ExclusiveCaller::new(),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        delayed_actions: DelayedActions::new(500),
        ui_options:      RefCell::new(ui_options),
        closed:          Cell::new(false),
        indi_evt_conn:   RefCell::new(None),
        self_:           RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_widgets();
    data.connect_indi();
    data.connect_widgets_events();
    data.apply_ui_options();
    data.fill_devices_list();
    data.correct_widgets_props();

    handlers.subscribe(clone!(@weak data => move |event| {
        match event {
            MainUiEvent::ProgramClosing =>
                data.handler_closing(),
            _ => {},
        }
    }));

    data.delayed_actions.set_event_handler(
        clone!(@weak data => move |action| {
            data.handler_delayed_action(action);
        })
    );
}

pub enum MainThreadEvent {
    Indi(indi::Event),
}

struct MountUi {
    builder:         gtk::Builder,
    window:          gtk::ApplicationWindow,
    excl:            ExclusiveCaller,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    delayed_actions: DelayedActions<DelayedActionTypes>,
    ui_options:      RefCell<UiOptions>,
    closed:          Cell<bool>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
    self_:           RefCell<Option<Rc<MountUi>>>,
}

impl Drop for MountUi {
    fn drop(&mut self) {
        log::info!("MountUi dropped");
    }
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedActionTypes {
    FillDevicesList,
    CorrectWidgetsProps,
    FillMountSpdList,
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

impl MountUi {
    const CONF_FN: &'static str = "ui_mount";

    const MOUNT_NAV_BUTTON_NAMES: &'static [&'static str] = &[
        "btn_left_top",    "btn_top",        "btn_right_top",
        "btn_left",        "btn_stop_mount", "btn_right",
        "btn_left_bottom", "btn_bottom",     "btn_right_bottom",
    ];

    fn init_widgets(&self) {
        let spb_ps_exp = self.builder.object::<gtk::SpinButton>("spb_ps_exp").unwrap();
        spb_ps_exp.set_range(0.5, 30.0);
        spb_ps_exp.set_digits(1);
        spb_ps_exp.set_increments(0.5, 5.0);
    }

    fn connect_indi(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();
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
        for &btn_name in Self::MOUNT_NAV_BUTTON_NAMES {
            let btn = self.builder.object::<gtk::Button>(btn_name).unwrap();
            btn.connect_button_press_event(clone!(
                @weak self as self_ => @default-return glib::Propagation::Proceed,
                move |_, eb| {
                    if eb.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                        self_.handler_nav_mount_btn_pressed(btn_name);
                    }
                    glib::Propagation::Proceed
                }
            ));
            btn.connect_button_release_event(clone!(
                @weak self as self_ => @default-return glib::Propagation::Proceed,
                move |_, _| {
                    self_.handler_nav_mount_btn_released(btn_name);
                    glib::Propagation::Proceed
                }
            ));
        }

        let cb_mount_list = self.builder.object::<gtk::ComboBoxText>("cb_mount_list").unwrap();
        cb_mount_list.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            let Some(cur_id) = cb.active_id() else { return; };
            let Ok(mut options) = self_.options.try_write() else { return; };
            if options.mount.device == cur_id.as_str() { return; }
            options.mount.device = cur_id.to_string();
            drop(options);
            self_.fill_mount_speed_list_widget();
            self_.show_cur_mount_state();
            self_.correct_widgets_props();
        }));

        let chb_tracking = self.builder.object::<gtk::CheckButton>("chb_tracking").unwrap();
        chb_tracking.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            self_.excl.exec(|| {
                let options = self_.options.read().unwrap();
                if options.mount.device.is_empty() { return; }
                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.indi.mount_set_tracking(&options.mount.device, chb.is_active(), true, None)?;
                    Ok(())
                });
            });
        }));

        let chb_parked = self.builder.object::<gtk::CheckButton>("chb_parked").unwrap();
        chb_parked.connect_active_notify(clone!(@weak self as self_ => move |chb| {
            self_.excl.exec(|| {
                let options = self_.options.read().unwrap();
                if options.mount.device.is_empty() { return; }
                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.indi.mount_set_parked(&options.mount.device, chb.is_active(), true, None)?;
                    Ok(())
                });
                self_.correct_widgets_props();
            });
        }));
    }

    fn correct_widgets_props(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let options = self.options.read().unwrap();
        let mount = options.mount.device.clone();
        drop(options);

        let mnt_active = self.indi.is_device_enabled(&mount).unwrap_or(false);
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;

        let mount_ctrl_sensitive =
            indi_connected &&
            mnt_active &&
            waiting;

        let move_enabled = !ui.prop_bool("chb_parked.active") && mount_ctrl_sensitive;

        ui.enable_widgets(false, &[
            ("bx_simple_mount", mount_ctrl_sensitive),
        ]);

        ui.enable_widgets(true, &[
            ("chb_tracking", move_enabled),
            ("cb_mnt_speed", move_enabled),
            ("chb_inv_ns",   move_enabled),
            ("chb_inv_we",   move_enabled),
        ]);
        for &btn_name in Self::MOUNT_NAV_BUTTON_NAMES {
            ui.set_prop_bool_ex(btn_name, "sensitive", move_enabled);
        }
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
        let options = self.ui_options.borrow();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_bool("exp_mount.expanded", options.expanded);
    }

    fn get_ui_options_from_widgets(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let mut options = self.ui_options.borrow_mut();
        options.expanded = ui.prop_bool("exp_mount.expanded");
    }

    fn process_event_in_main_thread(&self, event: MainThreadEvent) {
        match event {
            MainThreadEvent::Indi(indi::Event::NewDevice(event)) =>
                if event.interface.contains(indi::DriverInterface::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedActionTypes::FillDevicesList);
                },

            MainThreadEvent::Indi(indi::Event::DeviceConnected(event)) =>
                if event.interface.contains(indi::DriverInterface::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedActionTypes::CorrectWidgetsProps);
                },

            MainThreadEvent::Indi(indi::Event::DeviceDelete(event)) => {
                if event.drv_interface.contains(indi::DriverInterface::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedActionTypes::FillDevicesList);
                    self.delayed_actions.schedule(DelayedActionTypes::CorrectWidgetsProps);
                }
            }
            MainThreadEvent::Indi(indi::Event::ConnChange(conn_state)) => {
                if conn_state == indi::ConnState::Disconnected
                || conn_state == indi::ConnState::Connected {
                    self.delayed_actions.schedule(DelayedActionTypes::CorrectWidgetsProps);
                }
            }

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
            }
            _ => {}
        }
    }

    fn handler_nav_mount_btn_pressed(&self, button_name: &str) {
        let options = self.options.read().unwrap();
        let mount_device_name = &options.mount.device;
        if mount_device_name.is_empty() { return; }
        gtk_utils::exec_and_show_error(&self.window, || {
            let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
            if button_name != "btn_stop_mount" {
                let inv_ns = ui.prop_bool("chb_inv_ns.active");
                let inv_we = ui.prop_bool("chb_inv_we.active");
                self.indi.mount_reverse_motion(
                    mount_device_name,
                    inv_ns,
                    inv_we,
                    false,
                    INDI_SET_PROP_TIMEOUT
                )?;
                let speed = ui.prop_string("cb_mnt_speed.active-id");
                if let Some(speed) = speed {
                    self.indi.mount_set_slew_speed(
                        mount_device_name,
                        &speed,
                        true,
                        Some(100)
                    )?
                }
            }
            match button_name {
                "btn_left_top" => {
                    self.indi.mount_start_move_west(mount_device_name)?;
                    self.indi.mount_start_move_north(mount_device_name)?;
                }
                "btn_top" => {
                    self.indi.mount_start_move_north(mount_device_name)?;
                }
                "btn_right_top" => {
                    self.indi.mount_start_move_east(mount_device_name)?;
                    self.indi.mount_start_move_north(mount_device_name)?;
                }
                "btn_left" => {
                    self.indi.mount_start_move_west(mount_device_name)?;
                }
                "btn_right" => {
                    self.indi.mount_start_move_east(mount_device_name)?;
                }
                "btn_left_bottom" => {
                    self.indi.mount_start_move_west(mount_device_name)?;
                    self.indi.mount_start_move_south(mount_device_name)?;
                }
                "btn_bottom" => {
                    self.indi.mount_start_move_south(mount_device_name)?;
                }
                "btn_right_bottom" => {
                    self.indi.mount_start_move_south(mount_device_name)?;
                    self.indi.mount_start_move_east(mount_device_name)?;
                }
                "btn_stop_mount" => {
                    self.indi.mount_abort_motion(mount_device_name)?;
                    self.indi.mount_stop_move(mount_device_name)?;
                }
                _ => {},
            };
            Ok(())
        });
    }

    fn handler_nav_mount_btn_released(&self, button_name: &str) {
        let options = self.options.read().unwrap();
        if options.mount.device.is_empty() { return; }
        gtk_utils::exec_and_show_error(&self.window, || {
            if button_name != "btn_stop_mount" {
                self.indi.mount_stop_move(&options.mount.device)?;
            }
            Ok(())
        });
    }

    fn fill_devices_list(&self) {
        let options = self.options.read().unwrap();
        let cur_mount = options.mount.device.clone();
        drop(options);

        let cb = self.builder.object::<gtk::ComboBoxText>("cb_mount_list").unwrap();
        let list = self.indi
            .get_devices_list_by_interface(indi::DriverInterface::TELESCOPE)
            .iter()
            .map(|dev| dev.name.to_string())
            .collect();
        let connected = self.indi.state() == indi::ConnState::Connected;
        fill_devices_list_into_combobox(
            &list,
            &cb,
            if !cur_mount.is_empty() { Some(cur_mount.as_str()) } else { None },
            connected,
            |id| {
                let Ok(mut options) = self.options.try_write() else { return; };
                options.mount.device = id.to_string();
            }
        );
    }

    fn fill_mount_speed_list_widget(&self) {
        let options = self.options.read().unwrap();
        if options.mount.device.is_empty() { return; }
        gtk_utils::exec_and_show_error(&self.window, || {
            let list = self.indi.mount_get_slew_speed_list(&options.mount.device)?;
            let cb_mnt_speed = self.builder.object::<gtk::ComboBoxText>("cb_mnt_speed").unwrap();
            cb_mnt_speed.remove_all();
            cb_mnt_speed.append(None, "---");
            for (id, text) in list {
                cb_mnt_speed.append(
                    Some(&id),
                    text.as_ref().unwrap_or(&id).as_str()
                );
            }
            if options.mount.speed.is_some() {
                cb_mnt_speed.set_active_id(options.mount.speed.as_deref());
            } else {
                cb_mnt_speed.set_active(Some(0));
            }
            Ok(())
        });
    }

    fn show_mount_tracking_state(&self, tracking: bool) {
        self.excl.exec(|| {
            let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
            ui.set_prop_bool("chb_tracking.active", tracking);
        });
    }

    fn show_mount_parked_state(&self, parked: bool) {
        self.excl.exec(|| {
            let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
            ui.set_prop_bool("chb_parked.active", parked);
        });
    }

    fn show_cur_mount_state(&self) {
        self.excl.exec(|| {
            let device = self.options.read().unwrap().mount.device.clone();

            let parked = self.indi.mount_get_parked(&device).unwrap_or(false);
            self.show_mount_parked_state(parked);

            let tracking = self.indi.mount_get_tracking(&device).unwrap_or(false);
            self.show_mount_tracking_state(tracking);
        });
    }

    fn handler_delayed_action(&self, action: &DelayedActionTypes) {
        match action {
            DelayedActionTypes::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
            DelayedActionTypes::FillMountSpdList => {
                self.fill_mount_speed_list_widget();
            }
            DelayedActionTypes::FillDevicesList => {
                self.fill_devices_list();
            }
        }
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
        match (prop_name, elem_name, value) {
            ("TELESCOPE_SLEW_RATE", ..) if new_prop => {
                let selected_device = self.options.read().unwrap().mount.device.clone();
                if selected_device != device_name { return; }
                self.delayed_actions.schedule(DelayedActionTypes::FillMountSpdList);
            }

            ("TELESCOPE_TRACK_STATE", elem, indi::PropValue::Switch(prop_value)) => {
                let selected_device = self.options.read().unwrap().mount.device.clone();
                if selected_device != device_name { return; }
                let tracking =
                    if elem == "TRACK_ON" { *prop_value }
                    else if elem == "TRACK_OFF" { !*prop_value }
                    else { return; };
                self.show_mount_tracking_state(tracking);
            }

            ("TELESCOPE_PARK", elem, indi::PropValue::Switch(prop_value)) => {
                let selected_device = self.options.read().unwrap().mount.device.clone();
                if selected_device != device_name { return; }
                let parked =
                    if elem == "PARK" { *prop_value }
                    else if elem == "UNPARK" { !*prop_value }
                    else { return; };
                self.show_mount_parked_state(parked);
            }

            _ => {}
        }
    }
}