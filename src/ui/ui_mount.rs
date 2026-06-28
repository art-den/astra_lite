use std::{cell::Cell, rc::Rc, sync::Arc};
use gtk::{glib, prelude::*, glib::clone};
use macros::FromBuilder;

use crate::{
    core::{core::{Core, ModeType}, events::*},
    hal::{DeviceType, HalState, TelescopeState, events::HalEvent, indi::{degree_to_str, hour_to_str}},
    options::*,
    ui::ui_main::MainUi,
};

use super::{gtk_utils::*, module::*, utils::*};


pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    core:    &Arc<Core>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/mount.ui"));
    let info_widgets = InfoWidgets::new();

    let obj = Rc::new(MountUi {
        widgets,
        info_widgets,
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        excl:            ExclusiveCaller::new(),
        core:            Arc::clone(core),
        delayed_actions: DelayedActions::new(500),
        prev_info_state: Cell::new(None),
        prev_info_ra:    Cell::new(0.0),
        prev_info_dec:   Cell::new(0.0),
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
    core:            Arc<Core>,
    delayed_actions: DelayedActions<DelayedAction>,
    prev_info_state: Cell<Option<TelescopeState>>,
    prev_info_ra:    Cell<f64>,
    prev_info_dec:   Cell<f64>,
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
        self.correct_widgets_props();
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::ModeChanged => {
                self.correct_widgets_props();
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
                let option = self.core.options().read().unwrap();
                if option.mount.device == **device_id {
                    self.delayed_actions.schedule(DelayedAction::FillMountSpdList);
                }
            }
            HalEvent::TelescopeStateChanged { device_id, state } => {
                let option = self.core.options().read().unwrap();
                if option.mount.device == **device_id {
                    self.show_info(Some(*state));
                }
            }
            HalEvent::TelescopeTrackingChanged { device_id, tracking } => {
                let option = self.core.options().read().unwrap();
                if option.mount.device == **device_id {
                    self.show_mount_tracking_state(*tracking);
                }
            }
            HalEvent::TelescopeParked(device_id) => {
                let option = self.core.options().read().unwrap();
                if option.mount.device == **device_id {
                    self.show_mount_parked_state(true);
                }
            }
            HalEvent::TelescopeUnparked(device_id) => {
                let option = self.core.options().read().unwrap();
                if option.mount.device == **device_id {
                    self.show_mount_parked_state(false);
                }
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
            clone!(@weak self as self_ => move |cb| { self_.excl.exec(|| {
                let Some(new_device_name) = cb.active_id() else { return; };
                self_.core.cur_devices.change_telescope(&new_device_name);
            });})
        );

        self.widgets.chb_tracking.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.excl.exec(|| {
                    let Some(telescope) = self_.core.cur_devices.telescope() else { return; };
                    exec_and_show_error(Some(&self_.window), || {
                        telescope.track(chb.is_active())?;
                        Ok(())
                    });
                });
            })
        );

        self.widgets.chb_parked.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.excl.exec(|| {
                    let Some(telescope) = self_.core.cur_devices.telescope() else { return; };
                    exec_and_show_error(Some(&self_.window), || {
                        if chb.is_active() {
                            telescope.park()?;
                        } else {
                            telescope.unpark()?;
                        }
                        Ok(())
                    });
                    self_.correct_widgets_props();
                });
            })
        );
    }

    fn correct_widgets_props(&self) {
        let mnt_active = self.core.cur_devices.telescope()
            .and_then(|t| t.is_active().ok())
            .unwrap_or(false);

        let mode = self.core.mode();
        let mode_type = mode.active.get_type();
        let waiting = mode_type == ModeType::Waiting;
        let live_view = mode_type == ModeType::LiveView;
        let single_shot = mode_type == ModeType::SingleShot;

        let mount_ctrl_sensitive =
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

        self.main_ui.set_module_panel_visible(self.info_widgets.bx.upcast_ref(), mnt_active);
        self.show_info(None);
    }

    fn handler_nav_mount_btn_pressed(&self, button: &gtk::Button) {
        let Some(telescope) = self.core.cur_devices.telescope() else { return; };
        exec_and_show_error(Some(&self.window), || {
            if button != &self.widgets.btn_stop {
                let inv_ns = self.widgets.chb_inv_ns.is_active();
                let inv_we = self.widgets.chb_inv_we.is_active();
                telescope.revert_motion(inv_ns, inv_we)?;
                if let Some(speed) = self.widgets.cb_speed.active_id() {
                    telescope.set_slew_speed(&speed)?;
                }
            }
            if button == &self.widgets.btn_left_top {
                telescope.move_(crate::hal::TelescopeMoveDir::NorthWest)?;
            } else if button == &self.widgets.btn_top {
                telescope.move_(crate::hal::TelescopeMoveDir::North)?;
            } else if button == &self.widgets.btn_right_top {
                telescope.move_(crate::hal::TelescopeMoveDir::NorthEast)?;
            } else if button == &self.widgets.btn_left {
                telescope.move_(crate::hal::TelescopeMoveDir::West)?;
            } else if button == &self.widgets.btn_right {
                telescope.move_(crate::hal::TelescopeMoveDir::East)?;
            } else if button == &self.widgets.btn_left_bottom {
                telescope.move_(crate::hal::TelescopeMoveDir::SouthWest)?;
            } else if button == &self.widgets.btn_bottom {
                telescope.move_(crate::hal::TelescopeMoveDir::South)?;
            } else if button == &self.widgets.btn_right_bottom {
                telescope.move_(crate::hal::TelescopeMoveDir::SouthEast)?;
            } else if button == &self.widgets.btn_stop {
                telescope.abort_motion()?;
            }
            Ok(())
        });
    }

    fn handler_nav_mount_btn_released(&self, button: &gtk::Button) {
        let Some(telescope) = self.core.cur_devices.telescope() else { return; };
        exec_and_show_error(Some(&self.window), || {
            if button != &self.widgets.btn_stop {
                telescope.abort_motion()?;
            }
            Ok(())
        });
    }

    fn fill_devices_list(&self) {
        let options = self.core.options().read().unwrap();
        let cur_mount = options.mount.device.clone();
        drop(options);

        let Ok(mounts) = self.core.hal().devices(DeviceType::TELESCOPE) else {
            return;
        };

        let mounts_ids_and_names = mounts
            .into_iter()
            .map(|dev| (dev.id, dev.name))
            .collect::<Vec<_>>();

        fill_devices_list_into_combobox(
            &mounts_ids_and_names,
            &self.widgets.cb_list,
            if !cur_mount.is_empty() { Some(cur_mount.as_str()) } else { None },
            |id| {
                let Ok(mut options) = self.core.options().try_write() else { return; };
                options.mount.device = id.to_string();
            }
        );
    }

    fn fill_mount_speed_list_widget(&self) {
        let Some(telescope) = self.core.cur_devices.telescope() else { return; };
        let options = self.core.options().read().unwrap();

        exec_and_show_error(Some(&self.window), || {
            let list = telescope.slew_speed_list()?;
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
            let Some(telescope) = self.core.cur_devices.telescope() else { return; };

            let parked = telescope.is_parked().unwrap_or(false);
            self.show_mount_parked_state(parked);

            let tracking = telescope.is_parked().unwrap_or(false);
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

    fn show_info(&self, state: Option<TelescopeState>) {
        let Some(telescope) = self.core.cur_devices.telescope() else {
            self.info_widgets.l_pos.set_label("---");
            return;
        };

        let new_state = state
            .or_else(|| telescope.state().ok())
            .unwrap_or(TelescopeState::Error);

        if self.prev_info_state.get() != Some(new_state) {
            self.prev_info_state.set(Some(new_state));
            let (text, color_str) = match new_state {
                TelescopeState::Stopped   => ("Stopped", Some(get_warn_color_str())),
                TelescopeState::Parked    => ("Parked", None),
                TelescopeState::Tracking  => ("Tracking", Some(get_ok_color_str())),
                TelescopeState::Slewing   => ("Slewing", Some(get_warn_color_str())),
                TelescopeState::Error     => ("Error", Some(get_err_color_str())),
                TelescopeState::Correcton => ("Correction", Some(get_warn_color_str())),
                TelescopeState::Moved     => ("Moved", Some(get_warn_color_str())),
            };
            let mut text = format!("<b>{}</b>", text);
            if let Some(color_str) = color_str {
                text = format!("<span foreground='{}'>{}</span>", color_str, text);
            }
            self.info_widgets.l_state.set_label(&text);
        }

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
