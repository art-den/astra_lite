use std::{
    rc::Rc,
    cell::{RefCell, Cell},
    collections::HashMap,
    borrow::Cow,
    io::{prelude::*, BufWriter},
    fs::File,
    sync::{RwLock, Arc},
};
use gtk::{prelude::*, gdk, glib, glib::clone};
use itertools::Itertools;
use chrono::prelude::*;
use macros::FromBuilder;
use crate::{
    core::core::Core,
    guiding::{external_guider::ExtGuiderType, phd2_conn},
    indi::{self, sexagesimal_to_value, value_to_sexagesimal},
    options::*,
    utils::gtk_utils::{self, is_combobox_empty}
};
use super::{ui_main::*, indi_widget::*, module::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    options: &Arc<RwLock<Options>>,
    core:    &Arc<Core>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let (drivers, load_drivers_err) =
        if cfg!(target_os = "windows") {
            (indi::Drivers::new_empty(), None)
        } else {
            match indi::Drivers::new() {
                Ok(drivers) =>
                    (drivers, None),
                Err(err) =>
                    (indi::Drivers::new_empty(), Some(err.to_string())),
            }
        };

    if drivers.groups.is_empty() {
        let mut options = options.write().unwrap();
        options.indi.remote = true; // force remote mode if no devices info
    }

    let indi_widget = IndiWidget::new(&indi);

    let widgets = Widgets {
        telescope: TelescopeWidgets  ::from_builder_str(include_str!(r"resources/hw_telescope.ui")),
        site:      SiteWidgets       ::from_builder_str(include_str!(r"resources/hw_site.ui")),
        indi:      IndiWidgets       ::from_builder_str(include_str!(r"resources/hw_indi.ui")),
        ext_soft:  ExtSoftwareWidgets::from_builder_str(include_str!(r"resources/hw_ext_soft.ui")),
        conn_stat: ConnStatusWidgets ::from_builder_str(include_str!(r"resources/hw_conn_stat.ui")),
        common:    CommonWidgets     ::from_builder_str(include_str!(r"resources/hw_common.ui")),
    };

    widgets.common.bx_devices_ctrl.add(indi_widget.widget());

    let obj = Rc::new(HardwareUi {
        widgets,
        core:          Arc::clone(core),
        indi:          Arc::clone(indi),
        options:       Arc::clone(options),
        indi_status:   RefCell::new(indi::ConnState::Disconnected),
        indi_drivers:  drivers,
        indi_evt_conn: RefCell::new(None),
        is_remote:     Cell::new(false),
        main_ui:       Rc::clone(main_ui),
        indi_widget,
        window:        window.clone(),
    });

    obj.init_widgets();
    obj.fill_devices_name();

    obj.connect_widgets_events();
    obj.connect_indi_and_guider_events();
    obj.correct_widgets_by_cur_state();

    if let Some(load_drivers_err) = load_drivers_err {
        obj.add_log_record(
            &Some(Utc::now()),
            "",
            &format!("Load devices info error: {}", load_drivers_err)
        );
    }

    obj
}

impl indi::ConnState {
    fn to_str(&self, short: bool) -> Cow<str> {
        match self {
            indi::ConnState::Disconnected =>
                Cow::from("Disconnected"),
            indi::ConnState::Connecting =>
                Cow::from("Connecting..."),
            indi::ConnState::Connected =>
                Cow::from("Connected"),
            indi::ConnState::Disconnecting =>
                Cow::from("Disconnecting..."),
            indi::ConnState::Error(text) =>
                if short { Cow::from("Connection error") }
                else { Cow::from(format!("Error: {}", text)) },
        }
    }
}

enum HardwareEvent {
    Indi(indi::Event),
    Phd2(phd2_conn::Event),
}

#[derive(FromBuilder)]
struct TelescopeWidgets {
    grd:              gtk::Grid,
    spb_foc_len:      gtk::SpinButton,
    spb_barlow:       gtk::SpinButton,
    spb_guid_foc_len: gtk::SpinButton,
}

#[derive(FromBuilder)]
struct SiteWidgets {
    grd:          gtk::Grid,
    e_lat:        gtk::Entry,
    e_long:       gtk::Entry,
    btn_get_site: gtk::Button,
}

#[derive(FromBuilder)]
struct IndiWidgets {
    bx:                  gtk::Box,
    l_mount_drivers:     gtk::Label,
    cb_mount_drivers:    gtk::ComboBox,
    l_camera_drivers:    gtk::Label,
    cb_camera_drivers:   gtk::ComboBox,
    l_guid_cam_drivers:  gtk::Label,
    cb_guid_cam_drivers: gtk::ComboBox,
    l_focuser_drivers:   gtk::Label,
    cb_focuser_drivers:  gtk::ComboBox,
    chb_remote:          gtk::CheckButton,
    e_remote_addr:       gtk::Entry,
    btn_conn_indi:       gtk::Button,
    btn_diconn_indi:     gtk::Button,
}

#[derive(FromBuilder)]
struct ExtSoftwareWidgets {
    bx: gtk::Box,
}

#[derive(FromBuilder)]
struct ConnStatusWidgets {
    grd:      gtk::Grid,
    lbl_indi: gtk::Label,
    lbl_phd2: gtk::Label,
}

#[derive(FromBuilder)]
struct CommonWidgets {
    bx:              gtk::Box,
    se_prop_name:    gtk::SearchEntry,
    bx_devices_ctrl: gtk::Box,
    tv_hw_log:       gtk::TreeView,
}

struct Widgets {
    telescope: TelescopeWidgets,
    site:      SiteWidgets,
    indi:      IndiWidgets,
    ext_soft:  ExtSoftwareWidgets,
    conn_stat: ConnStatusWidgets,
    common:    CommonWidgets,
}

struct HardwareUi {
    widgets:       Widgets,
    main_ui:       Rc<MainUi>,
    core:          Arc<Core>,
    indi:          Arc<indi::Connection>,
    options:       Arc<RwLock<Options>>,
    window:        gtk::ApplicationWindow,
    indi_status:   RefCell<indi::ConnState>,
    indi_drivers:  indi::Drivers,
    indi_evt_conn: RefCell<Option<indi::Subscription>>,
    indi_widget:   IndiWidget,
    is_remote:     Cell<bool>,
}

impl Drop for HardwareUi {
    fn drop(&mut self) {
        log::info!("HardwareUi dropped");
    }
}

impl UiModule for HardwareUi {
    fn show_options(&self, options: &Options) {
        self.show_indi_options(options);
        self.show_telescope_options(options);
        self.show_site_options(options);

        self.correct_widgets_by_cur_state();
    }

    fn get_options(&self, options: &mut Options) {
        self.get_indi_options(options);
        self.get_telescope_options(options);
        self.get_site_options(options);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "telescope",
                name:   "Telescope".to_string(),
                widget: self.widgets.telescope.grd.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "site",
                name:   "Site".to_string(),
                widget: self.widgets.site.grd.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "indi",
                name:   "INDI Drivers".to_string(),
                widget: self.widgets.indi.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "ext_soft",
                name:   "External software".to_string(),
                widget: self.widgets.ext_soft.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "conn_status",
                name:   "Connection status".to_string(),
                widget: self.widgets.conn_stat.grd.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "indi_ctrl",
                name:   String::new(),
                widget: self.widgets.common.bx.clone().upcast(),
                pos:    PanelPosition::Center,
                tab:    PanelTab::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
        ]
    }

    fn process_event(&self, event: &UiModuleEvent) {
        match event {
            UiModuleEvent::ProgramClosing => {
                self.handler_closing();
            }
            UiModuleEvent::TabChanged { from, to } => {
                self.indi_widget.set_enabled(*to == TabPage::Hardware);
                if *from == TabPage::Hardware {
                    let mut options = self.options.write().unwrap();
                    self.get_telescope_options(&mut options);
                    self.get_site_options(&mut options);
                }
            }
            _ => {}
        }
    }
}

impl HardwareUi {
    fn init_widgets(&self) {
        self.widgets.telescope.spb_foc_len.set_range(10.0, 10_000.0);
        self.widgets.telescope.spb_foc_len.set_digits(0);
        self.widgets.telescope.spb_foc_len.set_increments(1.0, 10.0);

        self.widgets.telescope.spb_barlow.set_range(0.1, 10.0);
        self.widgets.telescope.spb_barlow.set_digits(2);
        self.widgets.telescope.spb_barlow.set_increments(0.01, 0.1);

        self.widgets.telescope.spb_guid_foc_len.set_range(0.0, 1000.0);
        self.widgets.telescope.spb_guid_foc_len.set_digits(0);
        self.widgets.telescope.spb_guid_foc_len.set_increments(1.0, 10.0);
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action   (&self.window, self, "help_save_indi",        HardwareUi::handler_action_help_save_indi);
        gtk_utils::connect_action   (&self.window, self, "conn_indi",             HardwareUi::handler_action_conn_indi);
        gtk_utils::connect_action   (&self.window, self, "disconn_indi",          HardwareUi::handler_action_disconn_indi);
        gtk_utils::connect_action   (&self.window, self, "conn_phd2",             HardwareUi::handler_action_conn_phd2);
        gtk_utils::connect_action   (&self.window, self, "disconn_phd2",          HardwareUi::handler_action_disconn_phd2);
        gtk_utils::connect_action   (&self.window, self, "clear_hw_log",          HardwareUi::handler_action_clear_hw_log);
        gtk_utils::connect_action   (&self.window, self, "enable_all_devs",       HardwareUi::handler_action_enable_all_devices);
        gtk_utils::connect_action   (&self.window, self, "disable_all_devs",      HardwareUi::handler_action_disable_all_devices);
        gtk_utils::connect_action   (&self.window, self, "save_devs_options",     HardwareUi::handler_action_save_devices_options);
        gtk_utils::connect_action   (&self.window, self, "load_devs_options",     HardwareUi::handler_action_load_devices_options);
        gtk_utils::connect_action_rc(&self.window, self, "get_site_from_devices", HardwareUi::handler_action_get_site_from_devices);

        self.widgets.indi.chb_remote.connect_active_notify(
            clone!(@weak self as self_ => move |_| {
                self_.correct_widgets_by_cur_state();
            })
        );

        self.widgets.common.se_prop_name.connect_search_changed(
            clone!(@weak self as self_ => move |se| {
                let text_lc = se.text().to_lowercase();
                self_.indi_widget.set_filter_text(&text_lc);
            })
        );

        self.widgets.telescope.spb_foc_len.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.telescope.focal_len = sb.value();
                drop(options);
                _ = self_.core.init_cam_telescope_data();
            })
        );

        self.widgets.telescope.spb_barlow.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.telescope.barlow = sb.value();
                drop(options);
                _ = self_.core.init_cam_telescope_data();
            })
        );

        self.window.add_events(gdk::EventMask::KEY_PRESS_MASK);
        self.window.connect_key_press_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, event| {
                if self_.main_ui.current_tab_page() == TabPage::Hardware {
                    if event.state().contains(gdk::ModifierType::CONTROL_MASK)
                    && matches!(event.keyval(), gdk::keys::constants::F|gdk::keys::constants::f) {
                        self_.widgets.common.se_prop_name.grab_focus();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            }
        ));
    }

    fn connect_indi_and_guider_events(self: &Rc<Self>) {
        let (sender, receiver) = async_channel::unbounded();

        // Connect INDI events
        let sender_clone = sender.clone();
        *self.indi_evt_conn.borrow_mut() = Some(self.indi.subscribe_events(move |event| {
            sender_clone.send_blocking(HardwareEvent::Indi(event)).unwrap();
        }));

        // Connect PHD2 events
        let sender_clone = sender.clone();
        self.core.phd2().connect_event_handler(move |event| {
            sender_clone.send_blocking(HardwareEvent::Phd2(event)).unwrap();
        });

        // Process incoming events in main thread
        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    HardwareEvent::Indi(event) =>
                        self_.process_indi_event(event),
                    HardwareEvent::Phd2(event) =>
                        self_.process_phd2_event(event),
                };
            }
        }));
    }

    fn show_indi_options(&self, options: &Options) {
        self.widgets.indi.chb_remote.set_active(options.indi.remote);
        self.widgets.indi.e_remote_addr.set_text(&options.indi.address);
    }

    fn show_telescope_options(&self, options: &Options) {
        self.widgets.telescope.spb_foc_len.set_value(options.telescope.focal_len);
        self.widgets.telescope.spb_barlow.set_value(options.telescope.barlow);
        self.widgets.telescope.spb_guid_foc_len.set_value(options.guiding.ext_guider.foc_len);
    }

    fn show_site_options(&self, options: &Options) {
        self.widgets.site.e_lat.set_text(&value_to_sexagesimal(options.site.latitude, true, 6));
        self.widgets.site.e_long.set_text(&value_to_sexagesimal(options.site.longitude, true, 6));
    }

    fn get_indi_options(&self, options: &mut Options) {
        options.indi.mount    = self.widgets.indi.cb_mount_drivers.active_id().map(|s| s.to_string());
        options.indi.camera   = self.widgets.indi.cb_camera_drivers.active_id().map(|s| s.to_string());
        options.indi.guid_cam = self.widgets.indi.cb_guid_cam_drivers.active_id().map(|s| s.to_string());
        options.indi.focuser  = self.widgets.indi.cb_focuser_drivers.active_id().map(|s| s.to_string());
        options.indi.remote   = self.widgets.indi.chb_remote.is_active();
        options.indi.address  = self.widgets.indi.e_remote_addr.text().into();
    }

    fn get_telescope_options(&self, options: &mut Options) {
        options.telescope.focal_len        = self.widgets.telescope.spb_foc_len.value();
        options.telescope.barlow           = self.widgets.telescope.spb_barlow.value();
        options.guiding.ext_guider.foc_len = self.widgets.telescope.spb_guid_foc_len.value();
    }

    fn get_site_options(&self, options: &mut Options) {
        let lat_string = self.widgets.site.e_lat.text();
        if let Some(latitude) = sexagesimal_to_value(&lat_string) {
            options.site.latitude = latitude;
        }
        let long_str = self.widgets.site.e_long.text();
        if let Some(longitude) = sexagesimal_to_value(&long_str) {
            options.site.longitude = longitude;
        }
    }

    fn process_indi_event(&self, event: indi::Event) {
        match event {
            indi::Event::ConnectionLost => {
                gtk_utils::show_error_message(&self.window, "INDI server", "Lost connection to INDI server ;-(");
            },
            indi::Event::ConnChange(conn_state) => {
                if let indi::ConnState::Error(_) = &conn_state {
                    self.add_log_record(&Some(Utc::now()), "", &conn_state.to_str(false))
                }
                *self.indi_status.borrow_mut() = conn_state;
                self.correct_widgets_by_cur_state();
                self.update_window_title();
            }
            indi::Event::PropChange(event) => {
                match &event.change {
                    indi::PropChange::New(value) => {
                        if log::log_enabled!(log::Level::Debug) {
                            let prop_name_string = format!(
                                "(+) {:20}.{:27}.{:27}",
                                event.device_name,
                                event.prop_name,
                                value.elem_name,
                            );
                            log::debug!(
                                "{} = {}",
                                prop_name_string,
                                value.prop_value.to_string_for_logging()
                            );
                        }
                    },
                    indi::PropChange::Change{value, prev_state, new_state} => {
                        if log::log_enabled!(log::Level::Debug) {
                            let prop_name_string = format!(
                                "(*) {:20}.{:27}.{:27}",
                                event.device_name,
                                event.prop_name,
                                value.elem_name,
                            );
                            if prev_state == new_state {
                                log::debug!(
                                    "{} = {}",
                                    prop_name_string,
                                    value.prop_value.to_string_for_logging()
                                );
                            } else {
                                log::debug!(
                                    "{} = {} ({:?} -> {:?})",
                                    prop_name_string,
                                    value.prop_value.to_string_for_logging(),
                                    prev_state,
                                    new_state
                                );
                            }
                        }
                    },
                    indi::PropChange::Delete => {
                        log::debug!(
                            "(-) {:20}.{:27}",
                            event.device_name,
                            event.prop_name
                        );
                    },
                };
            }
            indi::Event::DeviceDelete(event) => {
                log::debug!("(-) {:20}", &event.device_name);
            }
            indi::Event::Message(message) => {
                log::debug!("indi: device={}, text={}", message.device_name, message.text);
                self.add_log_record(
                    &message.timestamp,
                    &message.device_name,
                    &message.text
                );
            }
            indi::Event::ReadTimeOut => {
                log::debug!("indi: read time out");
            }
            indi::Event::BlobStart(_) => {
                log::debug!("indi: blob start");
            }
            indi::Event::DeviceConnected(dev) => {
                log::debug!(
                    "indi: device {} {}",
                    dev.device_name,
                    if dev.connected { "connected" } else { "disconnected" }
                );
            }
            _ => {}
        }
    }

    fn process_phd2_event(&self, event: phd2_conn::Event) {
        let status_text = match event {
            phd2_conn::Event::Started|
            phd2_conn::Event::Disconnected =>
                "Connecting...",
            phd2_conn::Event::Connected =>
                "Connected",
            phd2_conn::Event::Stopped =>
                "---",
            _ =>
                return,
        };

        self.widgets.conn_stat.lbl_phd2.set_label(status_text);
    }

    fn handler_closing(&self) {
        if let Some(indi_conn) = self.indi_evt_conn.borrow_mut().take() {
            self.indi.unsubscribe(indi_conn);
        }

        if !self.is_remote.get() {
            _ = self.indi.command_enable_all_devices(false, true, Some(2000));
        }

        log::info!("Disconnecting from INDI...");
        _ = self.indi.disconnect_and_wait();
        log::info!("Done!");

        log::info!("Stop connection to PHD2...");
        _ = self.core.phd2().stop();
        log::info!("Done!");

        self.core.phd2().discnnect_all();
    }

    fn correct_widgets_by_cur_state(&self) {
        let status = self.indi_status.borrow();
        let (conn_en, disconn_en) = match *status {
            indi::ConnState::Disconnected  => (true,  false),
            indi::ConnState::Connecting    => (false, true),
            indi::ConnState::Disconnecting => (false, false),
            indi::ConnState::Connected     => (false, true),
            indi::ConnState::Error(_)      => (true,  false),
        };
        let connected = *status == indi::ConnState::Connected;
        let disconnected = matches!(
            *status,
            indi::ConnState::Disconnected|
            indi::ConnState::Error(_)
        );
        let phd2_working = self.core.phd2().is_working();
        gtk_utils::enable_actions(&self.window, &[
            ("conn_indi",    conn_en),
            ("disconn_indi", disconn_en),
            ("conn_phd2",    !phd2_working),
            ("disconn_phd2", phd2_working),
        ]);

        self.widgets.conn_stat.lbl_indi.set_label(&status.to_str(false));

        let remote = self.widgets.indi.chb_remote.is_active();

        let (conn_cap, disconn_cap) = if remote {
            ("Connect INDI", "Disconnect INDI")
        } else {
            ("Start INDI", "Stop INDI")
        };

        self.widgets.indi.btn_conn_indi.set_label(conn_cap);
        self.widgets.indi.btn_diconn_indi.set_label(disconn_cap);

        let mnt_sensitive = !remote && disconnected && !is_combobox_empty(&self.widgets.indi.cb_mount_drivers);
        let cam_sensitive = !remote && disconnected && !is_combobox_empty(&self.widgets.indi.cb_camera_drivers);
        let guid_cam_sensitive = !remote && disconnected && !is_combobox_empty(&self.widgets.indi.cb_guid_cam_drivers);
        let foc_sensitive = !remote && disconnected && !is_combobox_empty(&self.widgets.indi.cb_focuser_drivers);

        self.widgets.indi.l_mount_drivers.set_sensitive(mnt_sensitive);
        self.widgets.indi.cb_mount_drivers.set_sensitive(mnt_sensitive);
        self.widgets.indi.l_camera_drivers.set_sensitive(cam_sensitive);
        self.widgets.indi.cb_camera_drivers.set_sensitive(cam_sensitive);
        self.widgets.indi.l_guid_cam_drivers.set_sensitive(guid_cam_sensitive);
        self.widgets.indi.cb_guid_cam_drivers.set_sensitive(guid_cam_sensitive);
        self.widgets.indi.l_focuser_drivers.set_sensitive(foc_sensitive);
        self.widgets.indi.cb_focuser_drivers.set_sensitive(foc_sensitive);
        self.widgets.indi.chb_remote.set_sensitive(!self.indi_drivers.groups.is_empty() && disconnected);
        self.widgets.indi.e_remote_addr.set_sensitive(remote && disconnected);

        gtk_utils::enable_actions(&self.window, &[
            ("enable_all_devs",   connected && remote),
            ("disable_all_devs",  connected && remote),
            ("save_devs_options", connected),
            ("load_devs_options", connected),
        ]);
    }

    fn handler_action_conn_indi(&self) {
        self.read_options_from_widgets();
        gtk_utils::exec_and_show_error(&self.window, || {
            let options = self.options.read().unwrap();
            let drivers = if !options.indi.remote {
                let telescopes = self.indi_drivers.get_group_by_name("Telescopes")?;
                let cameras = self.indi_drivers.get_group_by_name("CCDs")?;
                let focusers = self.indi_drivers.get_group_by_name("Focusers")?;
                let telescope_driver_name = options.indi.mount.as_ref()
                    .and_then(|name| telescopes.get_item_by_device_name(name))
                    .map(|d| &d.driver);
                let camera_driver_name = options.indi.camera.as_ref()
                    .and_then(|name| cameras.get_item_by_device_name(name))
                    .map(|d| &d.driver);
                let guid_cam_driver_name = options.indi.guid_cam.as_ref()
                    .and_then(|name| cameras.get_item_by_device_name(name))
                    .map(|d| &d.driver);
                let focuser_driver_name = options.indi.focuser.as_ref()
                    .and_then(|name| focusers.get_item_by_device_name(name))
                    .map(|d| &d.driver);
                [ telescope_driver_name,
                camera_driver_name,
                guid_cam_driver_name,
                focuser_driver_name
                ].iter()
                    .filter_map(|v| *v)
                    .cloned()
                    .unique()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

            if !options.indi.remote && drivers.is_empty() {
                anyhow::bail!("No devices selected");
            }

            log::info!(
                "Connecting to INDI, remote={}, address={}, drivers='{}' ...",
                options.indi.remote,
                options.indi.address,
                drivers.iter().join(",")
            );

            let conn_settings = indi::ConnSettings {
                drivers,
                remote:               options.indi.remote,
                host:                 options.indi.address.clone(),
                activate_all_devices: !options.indi.remote,
                .. Default::default()
            };
            self.is_remote.set(options.indi.remote);
            drop(options);
            self.indi.connect(&conn_settings)?;
            Ok(())
        });
    }

    fn handler_action_disconn_indi(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            if !self.is_remote.get() {
                log::info!("Disabling all INDI devices before disconnect...");
                self.indi.command_enable_all_devices(false, true, Some(2000))?;
                log::info!("Done");
            }
            log::info!("Disabling disconnecting INDI...");
            self.indi.disconnect_and_wait()?;
            log::info!("Done");
            Ok(())
        });
    }

    fn handler_action_conn_phd2(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            self.read_options_from_widgets();
            self.core.create_ext_guider(ExtGuiderType::Phd2)?;
            self.correct_widgets_by_cur_state();
            Ok(())
        });
    }

    fn handler_action_disconn_phd2(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.disconnect_ext_guider()?;
            self.correct_widgets_by_cur_state();
            Ok(())
        });
    }

    fn fill_devices_name(&self) {
        fn fill_cb_list(
            data:       &HardwareUi,
            cb:         &gtk::ComboBox,
            group_name: &str,
            active:     &Option<String>
        ) {
            let Ok(group) = data.indi_drivers.get_group_by_name(group_name) else { return; };
            let model = gtk::TreeStore::new(&[String::static_type(), String::static_type()]);
            let mut manufacturer_nodes = HashMap::<&str, gtk::TreeIter>::new();
            model.insert_with_values(None, None, &[(0, &""), (1, &"--")]);
            let mut active_iter = None;
            for item in &group.items {
                if !manufacturer_nodes.contains_key(item.manufacturer.as_str()) {
                    let iter = model.insert_with_values(None, None, &[
                        (0, &""),
                        (1, &item.manufacturer)
                    ]);
                    manufacturer_nodes.insert(item.manufacturer.as_str(), iter);
                }
                let parent = manufacturer_nodes.get(item.manufacturer.as_str());
                let item_iter = model.insert_with_values(parent, None, &[
                    (0, &item.device),
                    (1, &item.device)
                ]);
                if Some(&item.device) == active.as_ref() {
                    active_iter = Some(item_iter);
                }
            }
            cb.set_model(Some(&model));
            let cell = gtk::CellRendererText::new();
            cb.pack_start(&cell, true);
            cb.add_attribute(&cell, "text", 1);
            cb.set_id_column(0);
            cb.set_entry_text_column(1);
            cb.set_active_iter(active_iter.as_ref());
        }

        let options = self.options.read().unwrap();
        fill_cb_list(self, &self.widgets.indi.cb_mount_drivers,    "Telescopes", &options.indi.mount);
        fill_cb_list(self, &self.widgets.indi.cb_camera_drivers,   "CCDs",       &options.indi.camera);
        fill_cb_list(self, &self.widgets.indi.cb_guid_cam_drivers, "CCDs",       &options.indi.guid_cam);
        fill_cb_list(self, &self.widgets.indi.cb_focuser_drivers,  "Focusers",   &options.indi.focuser);
    }

    fn read_options_from_widgets(&self) {
        let mut options = self.options.write().unwrap();
        self.get_indi_options(&mut options);
        self.get_indi_options(&mut options);
        self.get_telescope_options(&mut options);
    }

    fn add_log_record(
        &self,
        timestamp:   &Option<DateTime<Utc>>,
        device_name: &str,
        text:        &str,
    ) {
        let model = match self.widgets.common.tv_hw_log.model() {
            Some(model) => {
                model.downcast::<gtk::ListStore>().unwrap()
            },
            None => {
                let model = gtk::ListStore::new(&[
                    String::static_type(),
                    String::static_type(),
                    String::static_type(),
                ]);
                let columns = [
                    /* 0 */ "Time",
                    /* 1 */ "Device",
                    /* 2 */ "Text",
                ];
                for (idx, col_name) in columns.into_iter().enumerate() {
                    let cell_text = gtk::CellRendererText::new();
                    let col = gtk::TreeViewColumn::builder()
                        .title(col_name)
                        .resizable(true)
                        .clickable(true)
                        .visible(true)
                        .build();
                    TreeViewColumnExt::pack_start(&col, &cell_text, true);
                    TreeViewColumnExt::add_attribute(&col, &cell_text, "text", idx as i32);
                    self.widgets.common.tv_hw_log.append_column(&col);
                }
                self.widgets.common.tv_hw_log.set_model(Some(&model));
                model
            },
        };
        let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
        let last_is_selected =
            gtk_utils::get_list_view_selected_row(&self.widgets.common.tv_hw_log).map(|v| v+1) ==
            Some(models_row_cnt as i32);

        let local_time_str = if let Some(timestamp) = timestamp {
            let local_time: DateTime<Local> = DateTime::from(*timestamp);
            local_time.format("%H:%M:%S").to_string()
        } else {
            String::new()
        };
        let last = model.insert_with_values(
            None, &[
            (0, &local_time_str),
            (1, &device_name),
            (2, &text),
        ]);
        if last_is_selected || models_row_cnt == 0 {
            self.widgets.common.tv_hw_log.selection().select_iter(&last);
            if let [path] = self.widgets.common.tv_hw_log.selection().selected_rows().0.as_slice() {
                self.widgets.common.tv_hw_log.set_cursor(
                    path,
                    Option::<&gtk::TreeViewColumn>::None,
                    false
                );
            }
        }
    }

    fn handler_action_clear_hw_log(&self) {
        let Some(model) = self.widgets.common.tv_hw_log.model() else { return; };
        let model = model.downcast::<gtk::ListStore>().unwrap();
        model.clear();
    }

    fn handler_action_help_save_indi(&self) {
        let ff = gtk::FileFilter::new();
            ff.set_name(Some("Text files"));
            ff.add_pattern("*.txt");
        let fc = gtk::FileChooserDialog::builder()
            .action(gtk::FileChooserAction::Save)
            .title("Enter file name to save properties")
            .filter(&ff)
            .modal(true)
            .transient_for(&self.window)
            .build();
        gtk_utils::add_ok_and_cancel_buttons(
            fc.upcast_ref::<gtk::Dialog>(),
            "_Cancel", gtk::ResponseType::Cancel,
            "_Save",   gtk::ResponseType::Accept
        );
        let resp = fc.run();
        fc.close();
        if resp == gtk::ResponseType::Accept {
            gtk_utils::exec_and_show_error(&self.window, || {
                let all_props = self.indi.get_properties_list(None, None);
                let file_name = fc.file().expect("File name").path().unwrap().with_extension("txt");
                let mut file = BufWriter::new(File::create(file_name)?);
                for prop in all_props {
                    for elem in prop.elements {
                        write!(
                            &mut file, "{:20}.{:27}.{:27} = ",
                            prop.device, prop.name, elem.name,
                        )?;
                        if let indi::PropValue::Blob(blob) = elem.value {
                            writeln!(&mut file, "[BLOB len={}]", blob.data.len())?;
                        } else {
                            writeln!(&mut file, "{:?}", elem.value)?;
                        }
                    }
                }
                Ok(())
            });
        }
    }

    fn update_window_title(&self) {
        let options = self.options.read().unwrap();
        let status = self.indi_status.borrow();
        let dev_list = [
            ("mnt",    &options.indi.mount),
            ("cam.",   &options.indi.camera),
            ("guid.",  &options.indi.guid_cam),
            ("focus.", &options.indi.focuser),
        ].iter()
            .filter_map(|(str, v)| v.as_deref().map(|v| (str, v))) // skip None at v
            .filter(|(_, v)| !v.is_empty()) // skip empty driver name
            .map(|(str, v)| format!("{}: {}", str, v))
            .join(", ");

        drop(options);
        self.main_ui.set_dev_list_and_conn_status(
            dev_list,
            status.to_str(true).to_string()
        );
    }

    fn handler_action_enable_all_devices(&self) {
        self.set_switch_property_for_all_device("CONNECTION", "CONNECT");
    }

    fn handler_action_disable_all_devices(&self) {
        self.set_switch_property_for_all_device("CONNECTION", "DISCONNECT");
    }

    fn handler_action_save_devices_options(&self) {
        self.set_switch_property_for_all_device("CONFIG_PROCESS", "CONFIG_SAVE");
    }

    fn handler_action_load_devices_options(&self) {
        self.set_switch_property_for_all_device("CONFIG_PROCESS", "CONFIG_LOAD");
    }

    fn set_switch_property_for_all_device(&self, prop_name: &str, elem_name: &str) {
        gtk_utils::exec_and_show_error(&self.window, || {
            let devices = self.indi.get_devices_list();
            for device in devices {
                self.indi.command_set_switch_property(
                    &device.name,
                    prop_name,
                    &[(elem_name, true)]
                )?;
            }
            Ok(())
        });
    }

    fn handler_action_get_site_from_devices(self: &Rc<Self>) {
        gtk_utils::exec_and_show_error(&self.window, || {
            let indi = &self.indi;
            if indi.state() != indi::ConnState::Connected {
                anyhow::bail!("INDI is not connected!");
            }
            let devices = indi.get_devices_list_by_interface(
                indi::DriverInterface::GPS |
                indi::DriverInterface::TELESCOPE
            );

            let result: Vec<_> = devices
                .iter()
                .filter_map(|dev|
                    indi.get_geo_lat_long_elev(&dev.name)
                        .ok()
                        .map(|(lat,long,_)| (dev, lat, long))
                )
                .filter(|(_, lat,long)| *lat != 0.0 && *long != 0.0)
                .collect();

            if result.is_empty() {
                anyhow::bail!("No GPS or geographic data found!");
            }

            if result.len() == 1 {
                let (_, latitude, longitude) = result[0];
                self.widgets.site.e_lat.set_text(&indi::value_to_sexagesimal(latitude, true, 6));
                self.widgets.site.e_long.set_text(&indi::value_to_sexagesimal(longitude, true, 6));
                return Ok(());
            }

            let menu = gtk::Menu::new();
            for (dev, lat, long) in result {
                let mi_text = format!(
                    "{} {} ({})",
                    indi::value_to_sexagesimal(lat, true, 6),
                    indi::value_to_sexagesimal(long, true, 6),
                    dev.name
                );
                let menu_item = gtk::MenuItem::builder().label(mi_text).build();
                menu.append(&menu_item);
                menu_item.connect_activate(
                    clone!(@weak self as self_ => move |_| {
                        self_.widgets.site.e_lat.set_text(&indi::value_to_sexagesimal(lat, true, 6));
                        self_.widgets.site.e_long.set_text(&indi::value_to_sexagesimal(long, true, 6));
                    })
                );
            }

            menu.set_attach_widget(Some(&self.widgets.site.btn_get_site));
            menu.show_all();
            menu.popup_easy(gtk::gdk::ffi::GDK_BUTTON_SECONDARY as u32, 0);

            Ok(())
        });
    }
}