use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{consts::INDI_SET_PROP_TIMEOUT, core::{Core, ModeType}, events::*},
    indi,
    options::*,
};

use super::{gtk_utils::*, module::*, utils::*};


pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/mount.ui"));

    let obj = Rc::new(MountUi {
        widgets,
        window:          window.clone(),
        excl:            ExclusiveCaller::new(),
        options:         Arc::clone(options),
        core:            Arc::clone(core),
        indi:            Arc::clone(indi),
        delayed_actions: DelayedActions::new(500),
        closed:          Cell::new(false),
        indi_evt_conn:   RefCell::new(None),
    });

    obj.init_widgets();
    obj.connect_core_and_indi_events();
    obj.connect_widgets_events();
    obj.fill_devices_list();

    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.handler_delayed_action(action);
        })
    );

    obj
}

enum MainThreadEvent {
    Indi(indi::Event),
    Core(Event),
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    FillDevicesList,
    CorrectWidgetsProps,
    FillMountSpdList,
}

#[derive(FromBuilder)]
struct Widgets {
    bx:               gtk::Box,
    cb_list:          gtk::ComboBoxText,
    bx_widgets:       gtk::Box,
    btn_left_top:     gtk::Button,
    btn_top:          gtk::Button,
    btn_right_top:    gtk::Button,
    btn_left:         gtk::Button,
    btn_stop:         gtk::Button,
    btn_right:        gtk::Button,
    btn_left_bottom:  gtk::Button,
    btn_bottom:       gtk::Button,
    btn_right_bottom: gtk::Button,
    chb_tracking:     gtk::CheckButton,
    chb_parked:       gtk::CheckButton,
    cb_speed:         gtk::ComboBoxText,
    chb_inv_ns:       gtk::CheckButton,
    chb_inv_we:       gtk::CheckButton,
}

struct MountUi {
    widgets:         Widgets,
    window:          gtk::ApplicationWindow,
    excl:            ExclusiveCaller,
    options:         Arc<RwLock<Options>>,
    core:            Arc<Core>,
    indi:            Arc<indi::Connection>,
    delayed_actions: DelayedActions<DelayedAction>,
    closed:          Cell<bool>,
    indi_evt_conn:   RefCell<Option<indi::Subscription>>,
}

impl Drop for MountUi {
    fn drop(&mut self) {
        log::info!("MountUi dropped");
    }
}

impl UiModule for MountUi {
    fn show_options(&self, options: &Options) {
        self.widgets.chb_inv_ns.set_active(options.mount.inv_ns);
        self.widgets.chb_inv_we.set_active(options.mount.inv_we);
    }

    fn get_options(&self, options: &mut Options) {
        options.mount.inv_ns = self.widgets.chb_inv_ns.is_active();
        options.mount.inv_we = self.widgets.chb_inv_we.is_active();
        options.mount.speed  = self.widgets.cb_speed.active_id().map(|s| s.to_string());
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "mount",
            name:   "Mount control".to_string(),
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

impl MountUi {
    fn get_nav_bittons(&self) -> [&gtk::Button; 9] {
        [
            &self.widgets.btn_left_top,
            &self.widgets.btn_top,
            &self.widgets.btn_right_top,
            &self.widgets.btn_left,
            &self.widgets.btn_stop,
            &self.widgets.btn_right,
            &self.widgets.btn_left_bottom,
            &self.widgets.btn_bottom,
            &self.widgets.btn_right_bottom,
        ]
    }

    fn init_widgets(&self) {
    }

    fn connect_core_and_indi_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();
        let sender = main_thread_sender.clone();
        *self.indi_evt_conn.borrow_mut() = Some(self.indi.subscribe_events(move |event| {
            sender.send_blocking(MainThreadEvent::Indi(event)).unwrap();
        }));

        let sender = main_thread_sender.clone();
        self.core.event_subscriptions().subscribe(move |event| {
            sender.send_blocking(MainThreadEvent::Core(event)).unwrap();
        });

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                if self_.closed.get() { return; }
                self_.process_event_in_main_thread(event);
            }
        }));
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        for btn in self.get_nav_bittons() {
            btn.connect_button_press_event(clone!(
                @weak self as self_ => @default-return glib::Propagation::Proceed,
                move |btn, eb| {
                    if eb.button() == gtk::gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
                        self_.handler_nav_mount_btn_pressed(btn);
                    }
                    glib::Propagation::Proceed
                }
            ));
            btn.connect_button_release_event(clone!(
                @weak self as self_ => @default-return glib::Propagation::Proceed,
                move |btn, _| {
                    self_.handler_nav_mount_btn_released(btn);
                    glib::Propagation::Proceed
                }
            ));
        }

        self.widgets.cb_list.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                let Some(cur_id) = cb.active_id() else { return; };
                let Ok(mut options) = self_.options.try_write() else { return; };
                if options.mount.device == cur_id.as_str() { return; }
                options.mount.device = cur_id.to_string();
                drop(options);
                self_.fill_mount_speed_list_widget();
                self_.show_cur_mount_state();
                self_.correct_widgets_props();
                self_.core.event_subscriptions().notify(
                    Event::MountDeviceSelected(cur_id.to_string())
                );
            })
        );

        self.widgets.chb_tracking.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.excl.exec(|| {
                    let options = self_.options.read().unwrap();
                    if options.mount.device.is_empty() { return; }
                    exec_and_show_error(Some(&self_.window), || {
                        self_.indi.mount_set_tracking(
                            &options.mount.device,
                            chb.is_active(),
                            true,
                            None
                        )?;
                        Ok(())
                    });
                });
            })
        );

        self.widgets.chb_parked.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.excl.exec(|| {
                    let options = self_.options.read().unwrap();
                    if options.mount.device.is_empty() { return; }
                    exec_and_show_error(Some(&self_.window), || {
                        self_.indi.mount_set_parked(&options.mount.device, chb.is_active(), true, None)?;
                        Ok(())
                    });
                    self_.correct_widgets_props();
                });
            })
        );
    }

    fn correct_widgets_props(&self) {
        let options = self.options.read().unwrap();
        let mount = options.mount.device.clone();
        drop(options);

        let mnt_active = self.indi.is_device_enabled(&mount).unwrap_or(false);
        let indi_connected = self.indi.state() == indi::ConnState::Connected;

        let mode_data = self.core.mode_data();
        let mode_type = mode_data.mode.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;

        let mount_ctrl_sensitive =
            indi_connected &&
            mnt_active &&
            (waiting || single_shot || live_view);

        let move_enabled = !self.widgets.chb_parked.is_active() && mount_ctrl_sensitive;

        self.widgets.bx_widgets.set_sensitive(mount_ctrl_sensitive);
        self.widgets.chb_tracking.set_sensitive(move_enabled);
        self.widgets.cb_speed.set_sensitive(move_enabled);
        self.widgets.chb_inv_ns.set_sensitive(move_enabled);
        self.widgets.chb_inv_we.set_sensitive(move_enabled);

        for btn in self.get_nav_bittons() {
            btn.set_sensitive(move_enabled);
        }
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }
    }

    fn process_event_in_main_thread(&self, event: MainThreadEvent) {
        match event {
            MainThreadEvent::Indi(indi::Event::NewDevice(event)) =>
                if event.interface.contains(indi::DriverInterface::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::FillDevicesList);
                },

            MainThreadEvent::Indi(indi::Event::DeviceConnected(event)) =>
                if event.interface.contains(indi::DriverInterface::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                },

            MainThreadEvent::Indi(indi::Event::DeviceDelete(event)) => {
                if event.drv_interface.contains(indi::DriverInterface::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::FillDevicesList);
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                }
            }
            MainThreadEvent::Indi(indi::Event::ConnChange(conn_state)) => {
                if conn_state == indi::ConnState::Disconnected
                || conn_state == indi::ConnState::Connected {
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
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

            MainThreadEvent::Core(Event::ModeChanged) => {
                self.correct_widgets_props();
            }
            _ => {}
        }
    }

    fn handler_nav_mount_btn_pressed(&self, button: &gtk::Button) {
        let options = self.options.read().unwrap();
        let mount_device_name = &options.mount.device;
        if mount_device_name.is_empty() { return; }
        exec_and_show_error(Some(&self.window), || {
            if button != &self.widgets.btn_stop {
                let inv_ns = self.widgets.chb_inv_ns.is_active();
                let inv_we = self.widgets.chb_inv_we.is_active();
                self.indi.mount_reverse_motion(
                    mount_device_name,
                    inv_ns,
                    inv_we,
                    false,
                    INDI_SET_PROP_TIMEOUT
                )?;
                if let Some(speed) = self.widgets.cb_speed.active_id() {
                    self.indi.mount_set_slew_speed(
                        mount_device_name,
                        &speed,
                        true,
                        Some(100)
                    )?
                }
            }
            if button == &self.widgets.btn_left_top {
                self.indi.mount_start_move_west(mount_device_name)?;
                self.indi.mount_start_move_north(mount_device_name)?;
            } else if button == &self.widgets.btn_top {
                self.indi.mount_start_move_north(mount_device_name)?;
            } else if button == &self.widgets.btn_right_top {
                self.indi.mount_start_move_east(mount_device_name)?;
                self.indi.mount_start_move_north(mount_device_name)?;
            } else if button == &self.widgets.btn_left {
                self.indi.mount_start_move_west(mount_device_name)?;
            } else if button == &self.widgets.btn_right {
                self.indi.mount_start_move_east(mount_device_name)?;
            } else if button == &self.widgets.btn_left_bottom {
                self.indi.mount_start_move_west(mount_device_name)?;
                self.indi.mount_start_move_south(mount_device_name)?;
            } else if button == &self.widgets.btn_bottom {
                self.indi.mount_start_move_south(mount_device_name)?;
            } else if button == &self.widgets.btn_right_bottom {
                self.indi.mount_start_move_south(mount_device_name)?;
                self.indi.mount_start_move_east(mount_device_name)?;
            } else if button == &self.widgets.btn_stop {
                self.indi.mount_abort_motion(mount_device_name)?;
                self.indi.mount_stop_move(mount_device_name)?;
            }
            Ok(())
        });
    }

    fn handler_nav_mount_btn_released(&self, button: &gtk::Button) {
        let options = self.options.read().unwrap();
        if options.mount.device.is_empty() { return; }
        exec_and_show_error(Some(&self.window), || {
            if button != &self.widgets.btn_stop {
                self.indi.mount_stop_move(&options.mount.device)?;
            }
            Ok(())
        });
    }

    fn fill_devices_list(&self) {
        let options = self.options.read().unwrap();
        let cur_mount = options.mount.device.clone();
        drop(options);

        let list = self.indi
            .get_devices_list_by_interface(indi::DriverInterface::TELESCOPE)
            .iter()
            .map(|dev| dev.name.to_string())
            .collect();
        let connected = self.indi.state() == indi::ConnState::Connected;
        fill_devices_list_into_combobox(
            &list,
            &self.widgets.cb_list,
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
        exec_and_show_error(Some(&self.window), || {
            let list = self.indi.mount_get_slew_speed_list(&options.mount.device)?;
            self.widgets.cb_speed.remove_all();
            self.widgets.cb_speed.append(None, "---");
            for (id, text) in list {
                self.widgets.cb_speed.append(
                    Some(&id),
                    text.as_ref().unwrap_or(&id).as_str()
                );
            }
            if options.mount.speed.is_some() {
                self.widgets.cb_speed.set_active_id(options.mount.speed.as_deref());
            } else {
                self.widgets.cb_speed.set_active(Some(0));
            }
            Ok(())
        });
    }

    fn show_mount_tracking_state(&self, tracking: bool) {
        self.excl.exec(|| {
            self.widgets.chb_tracking.set_active(tracking);
        });
    }

    fn show_mount_parked_state(&self, parked: bool) {
        self.excl.exec(|| {
            self.widgets.chb_parked.set_active(parked);
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

    fn handler_delayed_action(&self, action: &DelayedAction) {
        match action {
            DelayedAction::CorrectWidgetsProps => {
                self.correct_widgets_props();
            }
            DelayedAction::FillMountSpdList => {
                self.fill_mount_speed_list_widget();
            }
            DelayedAction::FillDevicesList => {
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
                self.delayed_actions.schedule(DelayedAction::FillMountSpdList);
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