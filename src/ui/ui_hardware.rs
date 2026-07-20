use std::{
    rc::Rc,
    cell::{RefCell, Cell},
    collections::HashMap,
    borrow::Cow,
    io::{prelude::*, BufWriter},
    fs::File,
    sync::Arc,
};
use gtk::{prelude::*, gdk, glib, glib::clone};
use itertools::Itertools;
use chrono::prelude::*;
use macros::FromBuilder;
use crate::{
    core::{core::Core, events::Event}, guiding::{external_guider::ExtGuiderType, phd2}, hal::{DeviceType, HalImpl, HalState, events::HalEvent, indi::{self, sexagesimal_to_value, value_to_sexagesimal}}, options::*,
};
use super::{gtk_utils::*, indi_panel_widget::*, module::*, ui_main::*};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    core:    &Arc<Core>,
) -> Rc<dyn UiModule> {
    let indi_hal = core.hal.indi_impl();
    let drivers = indi_hal.drivers();

    if drivers.groups.is_empty() {
        let mut options = core.options.write().unwrap();
        options.indi.remote = true; // force remote mode if no devices info
    }

    let indi_widget = IndiPanelWidget::new(indi_hal.indi());

    let widgets = Widgets {
        telescope: TelescopeWidgets     ::from_builder_str(include_str!(r"resources/hw_telescope.ui")),
        site:      SiteWidgets          ::from_builder_str(include_str!(r"resources/hw_site.ui")),
        indi_drv:  IndiDrvWidgets       ::from_builder_str(include_str!(r"resources/hw_indi_drivers.ui")),
        indi_ctrl: IndiCtrlWidgets      ::from_builder_str(include_str!(r"resources/hw_indi_ctrl.ui")),
        aa_drv:    AscomAlpacaDrvWidgets::from_builder_str(include_str!(r"resources/hw_ascom_alpaca_conn.ui")),
        ext_soft:  ExtSoftwareWidgets   ::from_builder_str(include_str!(r"resources/hw_ext_soft.ui")),
        conn_stat: ConnStatusWidgets    ::from_builder_str(include_str!(r"resources/hw_conn_stat.ui")),
    };

    widgets.indi_ctrl.bx_devices_ctrl.add(indi_widget.widget());

    let obj = Rc::new(HardwareUi {
        core:         Arc::clone(core),
        indi_state:   RefCell::new(HalState::Disconnected),
        aa_state:     RefCell::new(HalState::Disconnected),
        is_remote:    Cell::new(false),
        main_ui:      Rc::clone(main_ui),
        window:       window.clone(),
        widgets,
        indi_widget,
    });

    obj.init_widgets();
    obj.fill_devices_name();

    obj.connect_widgets_events();
    obj.connect_guider_events();
    obj.correct_widgets_by_cur_state();
    obj.connect_indi_events();

    obj
}

impl HalState {
    fn to_str(&self, short: bool) -> Cow<'_, str> {
        match self {
            HalState::Disconnected | HalState::ImplNotDefined =>
                Cow::from("Disconnected"),
            HalState::Connecting =>
                Cow::from("Connecting..."),
            HalState::Connected =>
                Cow::from("Connected"),
            HalState::Disconnecting =>
                Cow::from("Disconnecting..."),
            HalState::Error(text) =>
                if short { Cow::from("Connection error") }
                else { Cow::from(format!("Error: {}", text)) },
        }
    }
}

enum HardwareEvent {
    Phd2(phd2::Event),
}

#[derive(FromBuilder)]
struct TelescopeWidgets {
    grd:              gtk::Grid,
    spb_foc_len:      gtk::SpinButton,
    spb_barlow:       gtk::SpinButton,
    spb_guid_foc_len: gtk::SpinButton,
    chb_corr_foc_len: gtk::CheckButton,
}

#[derive(FromBuilder)]
struct SiteWidgets {
    grd:          gtk::Grid,
    e_lat:        gtk::Entry,
    e_long:       gtk::Entry,
    btn_get_site: gtk::Button,
}

#[derive(FromBuilder)]
struct IndiDrvWidgets {
    bx:                   gtk::Box,
    l_mount_drivers:      gtk::Label,
    cb_mount_drivers:     gtk::ComboBox,
    l_camera_drivers:     gtk::Label,
    cb_camera_drivers:    gtk::ComboBox,
    l_guid_cam_drivers:   gtk::Label,
    cb_guid_cam_drivers:  gtk::ComboBox,
    l_focuser_drivers:    gtk::Label,
    cb_focuser_drivers:   gtk::ComboBox,
    l_flt_wheel_drivers:  gtk::Label,
    cb_flt_wheel_drivers: gtk::ComboBox,
    l_aux1_drivers:       gtk::Label,
    cb_aux1_drivers:      gtk::ComboBox,
    l_aux2_drivers:       gtk::Label,
    cb_aux2_drivers:      gtk::ComboBox,
    chb_remote:           gtk::CheckButton,
    e_remote_addr:        gtk::Entry,
    btn_conn_indi:        gtk::Button,
    btn_disconn_indi:     gtk::Button,
}

#[derive(FromBuilder)]
struct IndiCtrlWidgets {
    bx:              gtk::Box,
    se_prop_name:    gtk::SearchEntry,
    bx_devices_ctrl: gtk::Box,
    tv_hw_log:       gtk::TreeView,
}

#[derive(FromBuilder)]
struct AscomAlpacaDrvWidgets {
    bx:     gtk::Box,
    e_addr: gtk::Entry,
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

struct Widgets {
    telescope: TelescopeWidgets,
    site:      SiteWidgets,
    indi_drv:  IndiDrvWidgets,
    indi_ctrl: IndiCtrlWidgets,
    aa_drv:    AscomAlpacaDrvWidgets,
    ext_soft:  ExtSoftwareWidgets,
    conn_stat: ConnStatusWidgets,
}

struct HardwareUi {
    widgets:      Widgets,
    main_ui:      Rc<MainUi>,
    core:         Arc<Core>,
    window:       gtk::ApplicationWindow,
    indi_state:   RefCell<HalState>,
    aa_state:     RefCell<HalState>,
    indi_widget:  IndiPanelWidget,
    is_remote:    Cell<bool>,
}

impl Drop for HardwareUi {
    fn drop(&mut self) {
        log::info!("HardwareUi dropped");
    }
}

impl UiModule for HardwareUi {
    fn show_options(&self, options: &Options) {
        self.show_connection_options(options);
        self.show_telescope_options(options);
        self.show_site_options(options);
    }

    fn get_options(&self, options: &mut Options) {
        self.get_conn_options(options);
        self.get_telescope_options(options);
        self.get_site_options(options);
    }

    fn panels(&self) -> Vec<Panel> {
        let aa_drv_panel_flags = if cfg!(windows) {
            PanelFlags::empty()
        } else {
            PanelFlags::INVISIBLE
        };
        vec![
            Panel {
                str_id: "telescope",
                name:   "Telescope".to_string(),
                widget: self.widgets.telescope.grd.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "site",
                name:   "Site".to_string(),
                widget: self.widgets.site.grd.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "indi",
                name:   "INDI Drivers".to_string(),
                widget: self.widgets.indi_drv.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Hardware,
                flags:  PanelFlags::EXPANDED,
            },
            Panel {
                str_id: "ascom_alpaca",
                name:   "ASCOM Alpaca".to_string(),
                widget: self.widgets.aa_drv.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Hardware,
                flags:  aa_drv_panel_flags,
            },
            Panel {
                str_id: "ext_soft",
                name:   "External software".to_string(),
                widget: self.widgets.ext_soft.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "conn_status",
                name:   "Connection status".to_string(),
                widget: self.widgets.conn_stat.grd.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    TabPage::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "indi_ctrl",
                name:   "INDI devices control".to_string(),
                widget: self.widgets.indi_ctrl.bx.clone().upcast(),
                pos:    PanelPosition::Center,
                tab:    TabPage::Hardware,
                flags:  PanelFlags::NO_EXPANDER,
            },
        ]
    }

    fn on_show_options_first_time(&self) {
        self.correct_widgets_by_cur_state();
    }

    fn on_app_closing(&self) {
        if !self.is_remote.get() {
            let indi = self.core.hal.indi_impl().indi();
            _ = indi.command_enable_all_devices(false, true, Some(2000));
        }

        log::info!("Stop connection to PHD2...");
        _ = self.core.ext_guider.phd2_conn().stop();
        log::info!("Done!");

        self.core.ext_guider.phd2_conn().disconnect_all_event_handlers();
    }

    fn on_tab_changed(&self, from: TabPage, to: TabPage) {
        self.indi_widget.set_enabled(to == TabPage::Hardware);
        if from == TabPage::Hardware {
            let mut options = self.core.options.write().unwrap();
            self.get_telescope_options(&mut options);
            self.get_site_options(&mut options);
        }
    }


    fn on_event(&self, event: &Event) {
        match event {
            Event::TelescopeFocalLenChanged(focal_len) => {
                let diff_with_cur = f64::abs(self.widgets.telescope.spb_foc_len.value() - focal_len);
                if diff_with_cur < 0.1 { return; }
                self.widgets.telescope.spb_foc_len.set_value(*focal_len);
            }
            Event::GuiderFocalLenChanged(focal_len) => {
                let diff_with_cur = f64::abs(self.widgets.telescope.spb_guid_foc_len.value() - focal_len);
                if diff_with_cur < 0.1 { return; }
                self.widgets.telescope.spb_guid_foc_len.set_value(*focal_len);
            }
            _ => {},
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(state) => {
                if let HalState::Error(err) = &state {
                    self.add_log_record(&Some(Utc::now()), "", &err)
                }
                *self.indi_state.borrow_mut() = self.core.hal.indi_impl().state().clone();

                #[cfg(windows)] {
                    *self.aa_state.borrow_mut() = self.core.hal.ascom_alpaca_impl().state().clone();
                }

                self.correct_widgets_by_cur_state();
                self.update_window_title();
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

    fn connect_indi_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();

        let sender = main_thread_sender.clone();
        self.core.hal.indi_impl().indi().connect_event_handler(move |event| {
            _ = sender.send_blocking(event);
        });

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                self_.process_indi_event(&event);
            }
        }));
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        connect_action   (&self.window, self, "help_save_indi",        HardwareUi::handler_action_help_save_indi);
        connect_action   (&self.window, self, "conn_indi",             HardwareUi::handler_action_conn_indi);
        connect_action   (&self.window, self, "disconn_indi",          HardwareUi::handler_action_disconn_indi);
        connect_action   (&self.window, self, "conn_phd2",             HardwareUi::handler_action_conn_phd2);
        connect_action   (&self.window, self, "disconn_phd2",          HardwareUi::handler_action_disconn_phd2);
        connect_action   (&self.window, self, "clear_hw_log",          HardwareUi::handler_action_clear_hw_log);
        connect_action   (&self.window, self, "enable_all_devs",       HardwareUi::handler_action_enable_all_devices);
        connect_action   (&self.window, self, "disable_all_devs",      HardwareUi::handler_action_disable_all_devices);
        connect_action   (&self.window, self, "save_devs_options",     HardwareUi::handler_action_save_devices_options);
        connect_action   (&self.window, self, "load_devs_options",     HardwareUi::handler_action_load_devices_options);
        connect_action_rc(&self.window, self, "get_site_from_devices", HardwareUi::handler_action_get_site_from_devices);
        #[cfg(windows)] {
            connect_action(&self.window, self, "conn_aa",    HardwareUi::handler_action_conn_aa);
            connect_action(&self.window, self, "disconn_aa", HardwareUi::handler_action_disconn_aa);
        }

        self.widgets.indi_drv.chb_remote.connect_active_notify(
            clone!(@weak self as self_ => move |_| {
                self_.correct_widgets_by_cur_state();
            })
        );

        self.widgets.indi_ctrl.se_prop_name.connect_search_changed(
            clone!(@weak self as self_ => move |se| {
                let text_lc = se.text().to_lowercase();
                self_.indi_widget.set_filter_text(&text_lc);
            })
        );

        self.widgets.telescope.spb_foc_len.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.core.options.try_write() else { return; };
                let value = sb.value();
                if f64::abs(options.telescope.focal_len - value) < 0.1 { return; }
                options.telescope.focal_len = value;
                drop(options);
                self_.core.events.send(Event::TelescopeFocalLenChanged(value));
            })
        );

        self.widgets.telescope.spb_barlow.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.core.options.try_write() else { return; };
                options.telescope.barlow = sb.value();
                drop(options);
                self_.core.events.send(Event::TelescopeBarlowChanged);
            })
        );

        self.widgets.telescope.spb_guid_foc_len.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                let Ok(mut options) = self_.core.options.try_write() else { return; };
                let value = sb.value();
                if f64::abs(options.guiding.foc_len - value) < 0.1 { return; }
                options.guiding.foc_len = value;
                drop(options);
                self_.core.events.send(Event::GuiderFocalLenChanged(value));
            })
        );

        self.window.add_events(gdk::EventMask::KEY_PRESS_MASK);
        self.window.connect_key_press_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, event| {
                if self_.main_ui.current_tab_page() == TabPage::Hardware
                && event.state().contains(gdk::ModifierType::CONTROL_MASK)
                && matches!(event.keyval(), gdk::keys::constants::F|gdk::keys::constants::f) {
                        self_.widgets.indi_ctrl.se_prop_name.grab_focus();
                        return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
    }

    fn connect_guider_events(self: &Rc<Self>) {
        let (sender, receiver) = async_channel::unbounded();

        // Connect PHD2 events
        let sender_clone = sender.clone();
        self.core.ext_guider.phd2_conn().connect_event_handler(move |event| {
            sender_clone.send_blocking(HardwareEvent::Phd2(event)).unwrap();
        });

        // Process incoming events in main thread
        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    HardwareEvent::Phd2(event) =>
                        self_.process_phd2_event(event),
                };
            }
        }));
    }

    fn show_connection_options(&self, options: &Options) {
        self.widgets.indi_drv.chb_remote.set_active(options.indi.remote);
        self.widgets.indi_drv.e_remote_addr.set_text(&options.indi.address);
        self.widgets.aa_drv.e_addr.set_text(&options.ascom_alpaca.address);
    }

    fn show_telescope_options(&self, options: &Options) {
        self.widgets.telescope.spb_foc_len.set_value(options.telescope.focal_len);
        self.widgets.telescope.spb_barlow.set_value(options.telescope.barlow);
        self.widgets.telescope.spb_guid_foc_len.set_value(options.guiding.foc_len);
        self.widgets.telescope.chb_corr_foc_len.set_active(options.telescope.from_platesolve);
    }

    fn show_site_options(&self, options: &Options) {
        self.widgets.site.e_lat.set_text(&value_to_sexagesimal(options.site.latitude, true, 6));
        self.widgets.site.e_long.set_text(&value_to_sexagesimal(options.site.longitude, true, 6));
    }

    fn get_conn_options(&self, options: &mut Options) {
        options.indi.mount           = self.widgets.indi_drv.cb_mount_drivers.active_id().map(|s| s.to_string());
        options.indi.camera          = self.widgets.indi_drv.cb_camera_drivers.active_id().map(|s| s.to_string());
        options.indi.guid_cam        = self.widgets.indi_drv.cb_guid_cam_drivers.active_id().map(|s| s.to_string());
        options.indi.focuser         = self.widgets.indi_drv.cb_focuser_drivers.active_id().map(|s| s.to_string());
        options.indi.flt_wheel       = self.widgets.indi_drv.cb_flt_wheel_drivers.active_id().map(|s| s.to_string());
        options.indi.aux1            = self.widgets.indi_drv.cb_aux1_drivers.active_id().map(|s| s.to_string());
        options.indi.aux2            = self.widgets.indi_drv.cb_aux2_drivers.active_id().map(|s| s.to_string());
        options.indi.remote          = self.widgets.indi_drv.chb_remote.is_active();
        options.indi.address         = self.widgets.indi_drv.e_remote_addr.text().into();
        options.ascom_alpaca.address = self.widgets.aa_drv.e_addr.text().into();
    }

    fn get_telescope_options(&self, options: &mut Options) {
        options.telescope.focal_len       = self.widgets.telescope.spb_foc_len.value();
        options.telescope.barlow          = self.widgets.telescope.spb_barlow.value();
        options.guiding.foc_len           = self.widgets.telescope.spb_guid_foc_len.value();
        options.telescope.from_platesolve = self.widgets.telescope.chb_corr_foc_len.is_active();
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

    fn process_indi_event(&self, event: &indi::Event) {
        match event {
            indi::Event::ConnectionLost => {
                show_error_message(Some(&self.window), "INDI server", "Lost connection to INDI server ;-(");
            },
            indi::Event::PropChange(event) => {
                match &event.change {
                    indi::PropChange::New {prop_name, elem_name, value, state} => {
                        if log::log_enabled!(log::Level::Debug) {
                            let prop_name_string = format!(
                                "(+) {:20}.{:27}.{:27}",
                                event.device_name,
                                prop_name,
                                elem_name,
                            );
                            log::debug!(
                                "{} = {} ({:?})",
                                prop_name_string,
                                value.to_string_for_logging(),
                                state
                            );
                        }
                    },
                    indi::PropChange::Change{ prop_name, elem_name, value, prev_state, new_state } => {
                        if log::log_enabled!(log::Level::Debug) {
                            let prop_name_string = format!(
                                "(*) {:20}.{:27}.{:27}",
                                event.device_name,
                                prop_name,
                                elem_name,
                            );
                            if prev_state == new_state {
                                log::debug!(
                                    "{} = {}",
                                    prop_name_string,
                                    value.to_string_for_logging()
                                );
                            } else {
                                log::debug!(
                                    "{} = {} ({:?} -> {:?})",
                                    prop_name_string,
                                    value.to_string_for_logging(),
                                    prev_state,
                                    new_state
                                );
                            }
                        }
                    },
                    indi::PropChange::Delete { prop_name } => {
                        log::debug!(
                            "(-) {:20}.{:27}",
                            event.device_name,
                            prop_name
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

    fn process_phd2_event(&self, event: phd2::Event) {
        let status_text = match event {
            phd2::Event::Started|
            phd2::Event::Disconnected =>
                "Connecting...",
            phd2::Event::Connected =>
                "Connected",
            phd2::Event::Stopped =>
                "---",
            _ =>
                return,
        };

        self.widgets.conn_stat.lbl_phd2.set_label(status_text);
    }

    fn correct_widgets_by_cur_state(&self) {
        let indi_state = self.indi_state.borrow();

        let conn_en = |hal_state: &HalState| matches!(hal_state, HalState::Disconnected|HalState::ImplNotDefined|HalState::Error(_));
        let disconn_en = |hal_state: &HalState| matches!(hal_state, HalState::Connected);

        let indi_connected = *indi_state == HalState::Connected;
        let indi_disconnected = matches!(*indi_state, HalState::Disconnected|HalState::Error(_));
        let phd2_working = self.core.ext_guider.phd2_conn().is_working();
        enable_actions(&self.window, &[
            ("conn_indi",    conn_en(&*indi_state)),
            ("disconn_indi", disconn_en(&*indi_state)),
            ("conn_phd2",    !phd2_working),
            ("disconn_phd2", phd2_working),
        ]);

        #[cfg(windows)] {
            let aa_state = self.aa_state.borrow();
            enable_actions(&self.window, &[
                ("conn_aa",    conn_en(&*aa_state)),
                ("disconn_aa", disconn_en(&*aa_state)),
            ]);
        }

        self.widgets.conn_stat.lbl_indi.set_label(&indi_state.to_str(false));

        let remote = self.widgets.indi_drv.chb_remote.is_active();

        let (conn_cap, disconn_cap) = if remote {
            ("Connect INDI", "Disconnect INDI")
        } else {
            ("Start INDI", "Stop INDI")
        };

        self.widgets.indi_drv.btn_conn_indi.set_label(conn_cap);
        self.widgets.indi_drv.btn_disconn_indi.set_label(disconn_cap);

        let mnt_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_mount_drivers);
        let cam_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_camera_drivers);
        let guid_cam_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_guid_cam_drivers);
        let foc_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_focuser_drivers);
        let flt_wheel_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_flt_wheel_drivers);
        let aux1_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_aux1_drivers);
        let aux2_sensitive = !remote && indi_disconnected && !is_combobox_empty(&self.widgets.indi_drv.cb_aux2_drivers);

        let indi_drivers = self.core.hal.indi_impl().drivers();

        self.widgets.indi_drv.chb_remote.set_sensitive(!indi_drivers.groups.is_empty() && indi_disconnected);
        self.widgets.indi_drv.l_mount_drivers.set_sensitive(mnt_sensitive);
        self.widgets.indi_drv.cb_mount_drivers.set_sensitive(mnt_sensitive);
        self.widgets.indi_drv.l_camera_drivers.set_sensitive(cam_sensitive);
        self.widgets.indi_drv.cb_camera_drivers.set_sensitive(cam_sensitive);
        self.widgets.indi_drv.l_guid_cam_drivers.set_sensitive(guid_cam_sensitive);
        self.widgets.indi_drv.cb_guid_cam_drivers.set_sensitive(guid_cam_sensitive);
        self.widgets.indi_drv.l_focuser_drivers.set_sensitive(foc_sensitive);
        self.widgets.indi_drv.cb_focuser_drivers.set_sensitive(foc_sensitive);
        self.widgets.indi_drv.l_flt_wheel_drivers.set_sensitive(flt_wheel_sensitive);
        self.widgets.indi_drv.cb_flt_wheel_drivers.set_sensitive(flt_wheel_sensitive);
        self.widgets.indi_drv.l_aux1_drivers.set_sensitive(aux1_sensitive);
        self.widgets.indi_drv.cb_aux1_drivers.set_sensitive(aux1_sensitive);
        self.widgets.indi_drv.l_aux2_drivers.set_sensitive(aux2_sensitive);
        self.widgets.indi_drv.cb_aux2_drivers.set_sensitive(aux2_sensitive);

        self.widgets.indi_drv.e_remote_addr.set_sensitive(remote && indi_disconnected);

        enable_actions(&self.window, &[
            ("enable_all_devs",   indi_connected && remote),
            ("disable_all_devs",  indi_connected && remote),
            ("save_devs_options", indi_connected),
            ("load_devs_options", indi_connected),
        ]);
    }

    fn handler_action_conn_indi(&self) {
        self.sync_options_from_widgets();
        exec_and_show_error(Some(&self.window), || {
            let indi_hal = self.core.hal.indi_impl();
            let options = self.core.options.read().unwrap();
            indi_hal.connect(
                options.indi.remote,
                &options.indi.address,
                &options.indi.mount,
                &options.indi.camera,
                &options.indi.guid_cam,
                &options.indi.focuser,
                &options.indi.flt_wheel,
                &options.indi.aux1,
                &options.indi.aux2,
            )?;
            self.is_remote.set(options.indi.remote);
            Ok(())
        });
    }

    fn handler_action_disconn_indi(&self) {
        exec_and_show_error(Some(&self.window), || {
            let indi = self.core.hal.indi_impl().indi();
            if !self.is_remote.get() {
                log::info!("Disabling all INDI devices before disconnect...");
                indi.command_enable_all_devices(false, true, Some(2000))?;
                log::info!("Done");
            }
            log::info!("Disconnecting INDI...");
            indi.disconnect_and_wait()?;
            log::info!("Done");
            Ok(())
        });
    }

    #[cfg(windows)]
    fn handler_action_conn_aa(&self) {
        self.sync_options_from_widgets();
        exec_and_show_error(Some(&self.window), || {
            let aa_hal = self.core.hal.ascom_alpaca_impl();
            let options = self.core.options.read().unwrap();
            aa_hal.connect(&options.ascom_alpaca.address)?;
            Ok(())
        });
    }

    #[cfg(windows)]
    fn handler_action_disconn_aa(&self) {
        exec_and_show_error(Some(&self.window), || {
            let aa_hal = self.core.hal.ascom_alpaca_impl();
            aa_hal.disconnect()?;
            Ok(())
        });
    }

    fn handler_action_conn_phd2(&self) {
        exec_and_show_error(Some(&self.window), || {
            self.sync_options_from_widgets();
            self.core.ext_guider.create_and_connect(ExtGuiderType::Phd2)?;
            self.correct_widgets_by_cur_state();
            Ok(())
        });
    }

    fn handler_action_disconn_phd2(&self) {
        exec_and_show_error(Some(&self.window), || {
            self.core.ext_guider.disconnect()?;
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
            let indi_drivers = data.core.hal.indi_impl().drivers();
            let Ok(group) = indi_drivers.get_group_by_name(group_name) else { return; };
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

        let options = self.core.options.read().unwrap();
        fill_cb_list(self, &self.widgets.indi_drv.cb_mount_drivers,     "Telescopes",    &options.indi.mount);
        fill_cb_list(self, &self.widgets.indi_drv.cb_camera_drivers,    "CCDs",          &options.indi.camera);
        fill_cb_list(self, &self.widgets.indi_drv.cb_guid_cam_drivers,  "CCDs",          &options.indi.guid_cam);
        fill_cb_list(self, &self.widgets.indi_drv.cb_focuser_drivers,   "Focusers",      &options.indi.focuser);
        fill_cb_list(self, &self.widgets.indi_drv.cb_flt_wheel_drivers, "Filter Wheels", &options.indi.flt_wheel);
        fill_cb_list(self, &self.widgets.indi_drv.cb_aux1_drivers,      "Auxiliary",     &options.indi.aux1);
        fill_cb_list(self, &self.widgets.indi_drv.cb_aux2_drivers,      "Auxiliary",     &options.indi.aux2);
    }

    fn sync_options_from_widgets(&self) {
        let mut options = self.core.options.write().unwrap();
        self.get_options(&mut options);
    }

    fn add_log_record(
        &self,
        timestamp:   &Option<DateTime<Utc>>,
        device_name: &str,
        text:        &str,
    ) {
        let model = match self.widgets.indi_ctrl.tv_hw_log.model() {
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
                    self.widgets.indi_ctrl.tv_hw_log.append_column(&col);
                }
                self.widgets.indi_ctrl.tv_hw_log.set_model(Some(&model));
                model
            },
        };
        let models_row_cnt = get_model_row_count(model.upcast_ref());
        let last_is_selected =
            get_list_view_selected_row(&self.widgets.indi_ctrl.tv_hw_log).map(|v| v+1) ==
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
            self.widgets.indi_ctrl.tv_hw_log.selection().select_iter(&last);
            if let [path] = self.widgets.indi_ctrl.tv_hw_log.selection().selected_rows().0.as_slice() {
                self.widgets.indi_ctrl.tv_hw_log.set_cursor(
                    path,
                    Option::<&gtk::TreeViewColumn>::None,
                    false
                );
            }
        }
    }

    fn handler_action_clear_hw_log(&self) {
        let Some(model) = self.widgets.indi_ctrl.tv_hw_log.model() else { return; };
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
        add_ok_and_cancel_buttons(
            fc.upcast_ref::<gtk::Dialog>(),
            "_Cancel", gtk::ResponseType::Cancel,
            "_Save",   gtk::ResponseType::Accept
        );
        let resp = fc.run();
        fc.close();
        if resp == gtk::ResponseType::Accept {
            exec_and_show_error(Some(&self.window), || {
                let indi = self.core.hal.indi_impl().indi();
                let (_, all_props) = indi.get_properties_list(None);
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
        let options = self.core.options.read().unwrap();
        let indi_state = self.indi_state.borrow();
        let aa_state = self.aa_state.borrow();

        if !matches!(*indi_state, HalState::Disconnected|HalState::ImplNotDefined) {
            let dev_list = [
                ("mnt",     &options.indi.mount),
                ("cam.",    &options.indi.camera),
                ("guid.",   &options.indi.guid_cam),
                ("focus.",  &options.indi.focuser),
                ("f.wheel", &options.indi.flt_wheel),
            ].iter()
                .filter_map(|(str, v)| v.as_deref().map(|v| (str, v))) // skip None at v
                .filter(|(_, v)| !v.is_empty()) // skip empty driver name
                .map(|(str, v)| format!("{}: {}", str, v))
                .join(", ");

            drop(options);
            self.main_ui.set_dev_list_and_conn_status(
                dev_list,
                indi_state.to_str(true).to_string()
            );
        } else if !matches!(*aa_state, HalState::Disconnected|HalState::ImplNotDefined) {
            self.main_ui.set_dev_list_and_conn_status(
                "ASCOM Alpaca".to_string(),
                aa_state.to_str(true).to_string()
            );
        } else {
            self.main_ui.set_dev_list_and_conn_status(
                String::new(),
                "Disconnected".to_string()
            );
        }
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
        exec_and_show_error(Some(&self.window), || {
            let indi = self.core.hal.indi_impl().indi();
            let devices = indi.get_devices_list();
            for device in devices {
                indi.command_set_switch_property(
                    &device.name,
                    prop_name,
                    &[(elem_name, true)]
                )?;
            }
            Ok(())
        });
    }

    fn handler_action_get_site_from_devices(self: &Rc<Self>) {
        exec_and_show_error(Some(&self.window), || {
            let devices = self.core.hal.devices(DeviceType::TELESCOPE)?;

            let result: Vec<_> = devices
                .iter()
                .filter_map(|dev| {
                    let telescope = self.core.hal.telescope(&dev.id).ok()?;
                    let site = telescope.site().ok()?;
                    Some((telescope, site))
                })
                .filter(|(_, site)| site.latitude != 0.0 && site.longitude != 0.0)
                .collect();

            if result.is_empty() {
                eyre::bail!("No geographic data found in connected devices!");
            }

            if result.len() == 1 {
                let (_, first_site) = &result[0];
                self.widgets.site.e_lat.set_text(&indi::value_to_sexagesimal(first_site.latitude, true, 6));
                self.widgets.site.e_long.set_text(&indi::value_to_sexagesimal(first_site.longitude, true, 6));
            } else {
                let menu = gtk::Menu::new();
                for (telescope, site) in result {
                    let mi_text = format!(
                        "{} {} ({})",
                        indi::value_to_sexagesimal(site.latitude, true, 6),
                        indi::value_to_sexagesimal(site.longitude, true, 6),
                        telescope.name()
                    );
                    let menu_item = gtk::MenuItem::builder().label(mi_text).build();
                    menu.append(&menu_item);
                    menu_item.connect_activate(
                        clone!(@weak self as self_ => move |_| {
                            self_.widgets.site.e_lat.set_text(&indi::value_to_sexagesimal(site.latitude, true, 6));
                            self_.widgets.site.e_long.set_text(&indi::value_to_sexagesimal(site.longitude, true, 6));
                        })
                    );
                }
                menu.set_attach_widget(Some(&self.widgets.site.btn_get_site));
                menu.show_all();
                menu.popup_easy(gtk::gdk::ffi::GDK_BUTTON_SECONDARY as u32, 0);
            }
            Ok(())
        });
    }
}
