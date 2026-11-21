use std::{cell::{Cell, RefCell}, collections::HashSet, rc::Rc, sync::Arc};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{
        api::{Command, MountCommand, MountPatch, Query, QueryReply, Reply},
        engine::Engine, events::*, snapshot::DeviceState,
    },
    hal::{DeviceType, HalState, TelescopeMoveDir, TelescopeState, events::HalEvent},
    options::*,
    ui::ui_main::MainUi,
    utils::log_utils::log_if_error,
};
use ::indi::{degree_to_str, hour_to_str};

use super::{gtk_utils::*, module::*, utils::*};


pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    engine:  &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/mount.ui"));
    let info_widgets = InfoWidgets::new();

    let obj = Rc::new(MountUi {
        widgets,
        info_widgets,
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        excl:            ExclusiveCaller::new(),
        engine:          Arc::clone(engine),
        delayed_actions: DelayedActions::new(500),
        prev_info_state: Cell::new(None),
        prev_info_ra:    Cell::new(0.0),
        prev_info_dec:   Cell::new(0.0),
        state_syncing:   Cell::new(false),
        pending_cmds:    RefCell::new(HashSet::new()),
    });

    obj.init_widgets();
    obj.connect_widgets_events();

    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.handler_delayed_action(action);
        })
    );

    obj
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

struct InfoWidgets {
    bx:      gtk::Box,
    l_state: gtk::Label,
    l_pos:   gtk::Label,
}

impl InfoWidgets {
    fn new() -> Self {
        let bx = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(5)
            .visible(true)
            .build();

        let l_state = gtk::Label::builder()
            .label("State")
            .use_markup(true)
            .width_chars(10)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .visible(true)
            .build();

        let l_pos = gtk::Label::builder()
            .label("Pos")
            .use_markup(true)
            .width_chars(30)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .visible(true)
            .build();

        bx.add(&l_state);
        bx.add(&l_pos);
        Self { bx, l_state, l_pos }
    }
}

struct MountUi {
    widgets:         Widgets,
    info_widgets:    InfoWidgets,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    excl:            ExclusiveCaller,
    engine:          Arc<Engine>,
    delayed_actions: DelayedActions<DelayedAction>,
    prev_info_state: Cell<Option<TelescopeState>>,
    prev_info_ra:    Cell<f64>,
    prev_info_dec:   Cell<f64>,
    // Suppresses command handlers while widgets mirror the device state
    state_syncing: Cell<bool>,
    pending_cmds:  RefCell<HashSet<u64>>,
}

impl Drop for MountUi {
    fn drop(&mut self) {
        log::info!("MountUi dropped");
    }
}

impl UiModule for MountUi {
    fn show_options(&self, options: &Options) {
        let prev = self.state_syncing.replace(true);
        self.widgets.chb_inv_ns.set_active(options.mount.inv_ns);
        self.widgets.chb_inv_we.set_active(options.mount.inv_we);
        self.state_syncing.set(prev);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "mount",
                name:   "Mount control".to_string(),
                widget: self.widgets.bx.clone().upcast(),
                pos:    PanelPosition::Right,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "mount_info",
                name:   "Mount".to_string(),
                widget: self.info_widgets.bx.clone().upcast(),
                pos:    PanelPosition::Bottom,
                tab:    TabPage::Main,
                flags:  PanelFlags::NO_EXPANDER,
            },
        ]
    }

    fn on_show_options_first_time(&self) {
        // Fill the device list at first show: ASCOM Classic drivers are always
        // available, so the list must not depend on connect events
        self.delayed_actions.schedule(DelayedAction::FillDevicesList);
        self.correct_widgets_props();
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.correct_widgets_props();
            }
            Event::CommandReply { id, result } => {
                if !self.pending_cmds.borrow_mut().remove(id) {
                    return;
                }
                if let Err(err_msg) = result {
                    show_error_message(Some(&self.window), "Error", err_msg);
                }
            }
            Event::MountDeviceChanged(new_device_name) => {
                if self.widgets.cb_list.active_id().as_deref() != Some(new_device_name.as_str()) {
                    self.excl.exec(|| {
                        self.widgets.cb_list.set_active_id(Some(new_device_name.as_str()));
                    });
                }
                self.fill_mount_speed_list_widget();
                self.show_cur_mount_state();
                self.correct_widgets_props();
            }
            _ => {}
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(HalState::Connected|HalState::Disconnected) => {
                self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
            }
            HalEvent::DeviceConnected(info) => {
                if info.type_.contains(DeviceType::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::FillDevicesList);
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                }
            }
            HalEvent::DeviceDisconnected(info) => {
                if info.type_.contains(DeviceType::TELESCOPE) {
                    self.delayed_actions.schedule(DelayedAction::FillDevicesList);
                    self.delayed_actions.schedule(DelayedAction::CorrectWidgetsProps);
                }
            }
            HalEvent::TelescopeSlewRateListReady(device_id) => {
                let snap = self.main_ui.snapshot();
                if snap.options.mount.device == **device_id {
                    self.delayed_actions.schedule(DelayedAction::FillMountSpdList);
                }
            }
            HalEvent::TelescopeStateChanged { device_id, state } => {
                let snap = self.main_ui.snapshot();
                if snap.options.mount.device == **device_id {
                    self.show_info(Some(*state));
                }
            }
            HalEvent::TelescopeTrackingChanged { device_id, tracking } => {
                let snap = self.main_ui.snapshot();
                if snap.options.mount.device == **device_id {
                    self.show_mount_tracking_state(*tracking);
                }
            }
            HalEvent::TelescopeParked(device_id) => {
                let snap = self.main_ui.snapshot();
                if snap.options.mount.device == **device_id {
                    self.show_mount_parked_state(true);
                }
            }
            HalEvent::TelescopeUnparked(device_id) => {
                let snap = self.main_ui.snapshot();
                if snap.options.mount.device == **device_id {
                    self.show_mount_parked_state(false);
                }
            }
            _ => {}
        }
    }
}

impl MountUi {
    fn get_nav_buttons(&self) -> [&gtk::Button; 9] {
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

    fn connect_widgets_events(self: &Rc<Self>) {
        for btn in self.get_nav_buttons() {
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
            clone!(@weak self as self_ => move |cb| { self_.excl.exec(|| {
                let Some(new_device_name) = cb.active_id() else { return; };
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetMountDevice { device_id: new_device_name.to_string() }
                    ),
                    "Select mount device"
                );
            });})
        );

        self.widgets.chb_tracking.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                if self_.state_syncing.get() { return; }
                self_.submit_mount_cmd(MountCommand::Track(chb.is_active()));
            })
        );

        self.widgets.chb_parked.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                if self_.state_syncing.get() { return; }
                let cmd = if chb.is_active() {
                    MountCommand::Park
                } else {
                    MountCommand::Unpark
                };
                self_.submit_mount_cmd(cmd);
                self_.correct_widgets_props();
            })
        );

        self.widgets.chb_inv_ns.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                if self_.state_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetMountOptions(MountPatch {
                            inv_ns: Some(chb.is_active()),
                            ..Default::default()
                        })
                    ),
                    "Set mount invert NS"
                );
            })
        );

        self.widgets.chb_inv_we.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                if self_.state_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetMountOptions(MountPatch {
                            inv_we: Some(chb.is_active()),
                            ..Default::default()
                        })
                    ),
                    "Set mount invert WE"
                );
            })
        );

        self.widgets.cb_speed.connect_active_id_notify(
            clone!(@weak self as self_ => move |cb| {
                if self_.state_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetMountOptions(MountPatch {
                            speed: Some(cb.active_id().map(|id| id.to_string())),
                            ..Default::default()
                        })
                    ),
                    "Set mount speed"
                );
            })
        );
    }

    fn submit_mount_cmd(&self, cmd: MountCommand) {
        match self.engine.submit_command(Command::Mount(cmd)) {
            Ok(id) => { _ = self.pending_cmds.borrow_mut().insert(id); }
            Err(err) => show_error_message(Some(&self.window), "Error", &err.to_string()),
        }
    }

    fn correct_widgets_props(&self) {
        let snap = self.main_ui.snapshot();
        let mnt_active = snap.devices.mount.as_ref().is_some_and(|m| m.is_active);

        let mount_ctrl_sensitive = mnt_active && snap.permissions.can_change_mode;

        let move_enabled = !self.widgets.chb_parked.is_active() && mount_ctrl_sensitive;

        self.widgets.bx_widgets.set_sensitive(mount_ctrl_sensitive);
        self.widgets.chb_tracking.set_sensitive(move_enabled);
        self.widgets.cb_speed.set_sensitive(move_enabled);
        self.widgets.chb_inv_ns.set_sensitive(move_enabled);
        self.widgets.chb_inv_we.set_sensitive(move_enabled);

        for btn in self.get_nav_buttons() {
            btn.set_sensitive(move_enabled);
        }

        self.main_ui.set_module_panel_visible(self.info_widgets.bx.upcast_ref(), mnt_active);
        self.show_info(None);
    }

    fn handler_nav_mount_btn_pressed(&self, button: &gtk::Button) {
        let cmd = if button == &self.widgets.btn_stop {
            MountCommand::AbortMotion
        } else {
            let dir = if button == &self.widgets.btn_left_top {
                TelescopeMoveDir::NorthWest
            } else if button == &self.widgets.btn_top {
                TelescopeMoveDir::North
            } else if button == &self.widgets.btn_right_top {
                TelescopeMoveDir::NorthEast
            } else if button == &self.widgets.btn_left {
                TelescopeMoveDir::West
            } else if button == &self.widgets.btn_right {
                TelescopeMoveDir::East
            } else if button == &self.widgets.btn_left_bottom {
                TelescopeMoveDir::SouthWest
            } else if button == &self.widgets.btn_bottom {
                TelescopeMoveDir::South
            } else if button == &self.widgets.btn_right_bottom {
                TelescopeMoveDir::SouthEast
            } else {
                return;
            };
            MountCommand::MoveStart {
                dir,
                inv_ns:   self.widgets.chb_inv_ns.is_active(),
                inv_we:   self.widgets.chb_inv_we.is_active(),
                speed_id: self.widgets.cb_speed.active_id().map(|id| id.to_string()),
            }
        };
        self.submit_mount_cmd(cmd);
    }

    fn handler_nav_mount_btn_released(&self, button: &gtk::Button) {
        if button == &self.widgets.btn_stop {
            return;
        }
        self.submit_mount_cmd(MountCommand::AbortMotion);
    }

    fn fill_devices_list(&self) {
        let cur_mount = self.main_ui.snapshot().options.mount.device.clone();

        let Ok(Reply::Query(q)) = self.engine.exec_command(Command::Query(Query::Devices))
        else { return; };
        let QueryReply::Devices(devices) = *q else { return; };

        let mounts_ids_and_names = devices
            .into_iter()
            .filter(|dev| dev.type_.contains(DeviceType::TELESCOPE))
            .map(|dev| (dev.id, dev.name))
            .collect::<Vec<_>>();

        fill_devices_list_into_combobox(
            &mounts_ids_and_names,
            &self.widgets.cb_list,
            if !cur_mount.is_empty() { Some(cur_mount.as_str()) } else { None },
            |id| {
                log_if_error(
                    &self.engine.exec_command(
                        Command::SetMountDevice { device_id: id.to_string() }
                    ),
                    "Select mount device"
                );
            }
        );
    }

    fn fill_mount_speed_list_widget(&self) {
        // Exception (follow-up `Query::MountNav`): `slew_speed_list()` is a
        // live mount read; the snapshot API has no mount nav query yet
        let Some(telescope) = self.engine.cur_devices.telescope() else { return; };
        // Fresh: `SetMountOptions { speed }` is eventless, the cache may miss
        // the last speed selection
        let snap = self.main_ui.fresh_snapshot();
        let options = &snap.options;

        exec_and_show_error(Some(&self.window), || {
            let list = telescope.slew_speed_list()?;
            // Suppress the speed handler while widgets are synced from `Options`
            let prev = self.state_syncing.replace(true);
            self.widgets.cb_speed.remove_all();
            self.widgets.cb_speed.append(None, "---");
            for (id, text) in list {
                self.widgets.cb_speed.append(Some(&id), text.as_str());
            }
            if options.mount.speed.is_some() {
                self.widgets.cb_speed.set_active_id(options.mount.speed.as_deref());
            } else {
                self.widgets.cb_speed.set_active(Some(0));
            }
            self.state_syncing.set(prev);
            Ok(())
        });
    }

    fn show_mount_tracking_state(&self, tracking: bool) {
        let prev = self.state_syncing.replace(true);
        self.widgets.chb_tracking.set_active(tracking);
        self.state_syncing.set(prev);
    }

    fn show_mount_parked_state(&self, parked: bool) {
        let prev = self.state_syncing.replace(true);
        self.widgets.chb_parked.set_active(parked);
        self.state_syncing.set(prev);
    }

    fn show_cur_mount_state(&self) {
        let Some(mnt) = &self.main_ui.snapshot().devices.mount else { return; };
        let ts = match &mnt.state {
            Some(DeviceState::Telescope(ts)) => ts,
            _ => return,
        };
        self.show_mount_parked_state(ts.is_parked);
        self.show_mount_tracking_state(ts.is_tracking);
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

    fn show_info(&self, state: Option<TelescopeState>) {
        // Exception (follow-up `Query::MountNav`): the `telescope` object is
        // kept for the live `eq_coord()` read below
        let Some(telescope) = self.engine.cur_devices.telescope() else {
            self.info_widgets.l_pos.set_label("---");
            return;
        };

        let snap_state = self.main_ui.snapshot().devices.mount.as_ref()
            .and_then(|m| match &m.state {
                Some(DeviceState::Telescope(ts)) => ts.state,
                _ => None,
            });
        let new_state = state
            .or(snap_state)
            .unwrap_or(TelescopeState::Error);

        if self.prev_info_state.get() != Some(new_state) {
            self.prev_info_state.set(Some(new_state));
            let (text, color_str) = match new_state {
                TelescopeState::Stopped   => ("Stopped", Some(get_warn_color_str())),
                TelescopeState::Parked    => ("Parked", None),
                TelescopeState::Tracking  => ("Tracking", Some(get_ok_color_str())),
                TelescopeState::Slewing   => ("Slewing", Some(get_warn_color_str())),
                TelescopeState::Error     => ("Error", Some(get_err_color_str())),
                TelescopeState::Correction => ("Correction", Some(get_warn_color_str())),
                TelescopeState::Moved     => ("Moved", Some(get_warn_color_str())),
            };
            let mut text = format!("<b>{}</b>", text);
            if let Some(color_str) = color_str {
                text = format!("<span foreground='{}'>{}</span>", color_str, text);
            }
            self.info_widgets.l_state.set_label(&text);
        }

        // Exception (follow-up `Query::MountNav`): live `eq_coord()` read
        if let Ok((ra, dec)) = telescope.eq_coord()
        && (ra != self.prev_info_ra.get() || dec != self.prev_info_dec.get()) {
            self.prev_info_ra.set(ra);
            self.prev_info_dec.set(dec);
            let text = format!(
                "{} {}",
                hour_to_str(ra),
                degree_to_str(dec)
            );
            self.info_widgets.l_pos.set_label(&text);
        }
    }
}
