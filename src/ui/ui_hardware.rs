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
    core::{api::{Command, ConnectCommand, GuiderCommand, IndiCommand, IndiSelPatch, MountSite, Query, QueryReply, Reply}, engine::Engine, events::Event}, guiding::phd2, hal::{HalState, events::HalEvent}, options::*, utils::log_utils::log_if_error,
};
use ::indi::{self, value_to_sexagesimal};
use super::{gtk_utils::*, indi_panel_widget::*, module::*, ui_main::*};

/// INDI driver registry via the engine facade (read-only query).
fn query_indi_drivers(engine: &Arc<Engine>) -> indi::Drivers {
    match engine.exec_command(Command::Query(Query::IndiDrivers)) {
        Ok(Reply::Query(q)) => match *q {
            QueryReply::IndiDrivers(drivers) => drivers,
            _ => {
                log::error!("IndiDrivers query: unexpected query reply");
                indi::Drivers { groups: Vec::new() }
            }
        },
        Ok(_) => {
            log::error!("IndiDrivers query: unexpected reply");
            indi::Drivers { groups: Vec::new() }
        }
        Err(e) => {
            log::error!("IndiDrivers query failed: {e:#}");
            indi::Drivers { groups: Vec::new() }
        }
    }
}

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    engine:  &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let indi_hal = engine.hal.indi_impl();
    let drivers = query_indi_drivers(engine);

    if drivers.groups.is_empty() {
        // Force remote mode if no devices info
        log_if_error(
            &engine.exec_command(Command::SetIndiSelection(IndiSelPatch {
                remote: Some(true),
                ..Default::default()
            })),
            "Force INDI remote mode"
        );
    }

    let indi_widget = IndiPanelWidget::new(indi_hal.indi(), engine);

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
        engine:       Arc::clone(engine),
        indi_state:   RefCell::new(HalState::Disconnected),
        aa_state:     RefCell::new(HalState::Disconnected),
        is_remote:    Cell::new(false),
        opts_syncing: Cell::new(false),
        pending_cmds: RefCell::new(HashMap::new()),
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
    engine:       Arc<Engine>,
    window:       gtk::ApplicationWindow,
    indi_state:   RefCell<HalState>,
    aa_state:     RefCell<HalState>,
    indi_widget:  IndiPanelWidget,
    is_remote:    Cell<bool>,
    /// Set while widgets are programmatically synced from `Options`: option
    /// write handlers must not run, so the widget echo does not re-commit
    /// the just-synced values.
    opts_syncing: Cell<bool>,
    pending_cmds: RefCell<HashMap<u64, PendingEntry>>,
}

#[derive(PartialEq)]
enum ConnectPending {
    Plain,
    IndiConnect,
    /// HAL disconnect command: survives the `Disconnected` prune, its
    /// own reply is still in flight
    Disconnect,
    Guider,
    GetSites,
}

/// Owning domain of a pending command: decides which `Disconnected` event
/// may prune it. `Guider` and `MountQuery` are independent of any single
/// HAL and are never pruned (the engine command queue always replies).
#[derive(Clone, Copy, PartialEq)]
enum PendingDomain {
    Indi,
    #[cfg(windows)]
    Alpaca,
    Guider,
    MountQuery,
}

struct PendingEntry {
    domain: PendingDomain,
    kind:   ConnectPending,
}

/// Drops pending entries of the HAL domains that just transitioned to
/// `Disconnected` (the caller detects the transitions from per-HAL snapshot
/// states; `HalEvent::StateChanged` itself carries no source) and returns
/// their count. A disconnect command's own entry survives: its reply trails
/// the `Disconnected` event on the same main-thread channel (see
/// `MainUi::connect_hal_and_core_events`).
fn prune_aborted_by_disconnect(
    pending: &mut HashMap<u64, PendingEntry>,
    dropped_domains: &[PendingDomain],
) -> usize {
    let before = pending.len();
    pending.retain(|_, e| {
        e.kind == ConnectPending::Disconnect || !dropped_domains.contains(&e.domain)
    });
    before - pending.len()
}

/// Stores `new` into the per-HAL state cell and reports a transition from a
/// different state into `Disconnected` (duplicate `Disconnected` events do
/// not re-trigger).
fn hal_transitioned_to_disconnected(stored: &RefCell<HalState>, new: &HalState) -> bool {
    let old = stored.replace(new.clone());
    *new == HalState::Disconnected && new != &old
}

impl Drop for HardwareUi {
    fn drop(&mut self) {
        log::info!("HardwareUi dropped");
    }
}

impl UiModule for HardwareUi {
    fn show_options(&self, options: &Options) {
        let prev = self.opts_syncing.replace(true);
        self.show_connection_options(options);
        self.show_telescope_options(options);
        self.show_site_options(options);
        self.opts_syncing.set(prev);
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

    fn on_tab_changed(&self, _from: TabPage, to: TabPage) {
        self.indi_widget.set_enabled(to == TabPage::Hardware);
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
            Event::CommandReply { id, result } => {
                let Some(pending) = self.pending_cmds.borrow_mut().remove(id) else {
                    return;
                };
                match result {
                    Err(err_msg) => show_error_message(Some(&self.window), "Error", err_msg),
                    Ok(reply) => match pending.kind {
                        ConnectPending::Plain | ConnectPending::Disconnect => {}
                        ConnectPending::IndiConnect => {
                            let remote = self.main_ui.snapshot().options.indi.remote;
                            self.is_remote.set(remote);
                        }
                        ConnectPending::Guider => self.correct_widgets_by_cur_state(),
                        ConnectPending::GetSites => {
                            let sites = match reply {
                                Reply::Query(q) => match &**q {
                                    QueryReply::AllMountSites(sites) => sites.clone(),
                                    _ => Vec::new(),
                                },
                                _ => Vec::new(),
                            };
                            self.apply_mount_sites(&sites);
                        }
                    },
                }
            }
            _ => {},
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(state) => {
                if let HalState::Error(err) = &state {
                    self.add_log_record(&Some(Utc::now()), "", err)
                }
                // Snapshot is refreshed before dispatch (`process_hal_event`),
                // so the stored values are the previous per-HAL states: a
                // transition to Disconnected is detected per domain. ASCOM
                // Classic drops match no domain: no pending entry owns it
                let snap = self.main_ui.snapshot();
                let mut dropped_domains: Vec<PendingDomain> = Vec::new();
                if hal_transitioned_to_disconnected(
                    &self.indi_state, &snap.devices.indi_state
                ) {
                    dropped_domains.push(PendingDomain::Indi);
                }

                #[cfg(windows)] {
                    if hal_transitioned_to_disconnected(
                        &self.aa_state, &snap.devices.alpaca_state
                    ) {
                        dropped_domains.push(PendingDomain::Alpaca);
                    }
                }

                if !dropped_domains.is_empty() {
                    let aborted = prune_aborted_by_disconnect(
                        &mut self.pending_cmds.borrow_mut(), &dropped_domains
                    );
                    for _ in 0..aborted {
                        show_error_message(
                            Some(&self.window), "Error", "command aborted (disconnected)"
                        );
                    }
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
        self.engine.hal.indi_impl().indi().connect_event_handler(move |event| {
            _ = sender.send_blocking(event);
        });

        // Upgrade the weak ref inside the loop body (not via clone!@weak before
        // `async move`): otherwise the strong ref obtained at the first poll would
        // be kept alive across every await and pin this module forever.

        let weak_self = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                let Some(self_) = weak_self.upgrade() else { break; };
                self_.process_indi_event(&event);
            }
        });
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

        self.connect_driver_cb(&self.widgets.indi_drv.cb_mount_drivers,     |p, v| p.mount = v);
        self.connect_driver_cb(&self.widgets.indi_drv.cb_camera_drivers,    |p, v| p.camera = v);
        self.connect_driver_cb(&self.widgets.indi_drv.cb_guid_cam_drivers,  |p, v| p.guid_cam = v);
        self.connect_driver_cb(&self.widgets.indi_drv.cb_focuser_drivers,   |p, v| p.focuser = v);
        self.connect_driver_cb(&self.widgets.indi_drv.cb_flt_wheel_drivers, |p, v| p.flt_wheel = v);
        self.connect_driver_cb(&self.widgets.indi_drv.cb_aux1_drivers,      |p, v| p.aux1 = v);
        self.connect_driver_cb(&self.widgets.indi_drv.cb_aux2_drivers,      |p, v| p.aux2 = v);

        self.widgets.indi_drv.chb_remote.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                if !self_.opts_syncing.get() {
                    log_if_error(
                        &self_.engine.exec_command(
                            Command::SetIndiSelection(IndiSelPatch {
                                remote: Some(chb.is_active()),
                                ..Default::default()
                            })
                        ),
                        "Set INDI remote mode"
                    );
                }
                self_.correct_widgets_by_cur_state();
            })
        );

        self.widgets.indi_drv.e_remote_addr.connect_changed(
            clone!(@weak self as self_ => move |e| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetIndiSelection(IndiSelPatch {
                            address: Some(e.text().into()),
                            ..Default::default()
                        })
                    ),
                    "Set INDI address"
                );
            })
        );

        self.widgets.aa_drv.e_addr.connect_changed(
            clone!(@weak self as self_ => move |e| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetAlpacaAddress { address: e.text().into() }
                    ),
                    "Set Alpaca address"
                );
            })
        );

        self.widgets.site.e_lat.connect_changed(
            clone!(@weak self as self_ => move |e| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetSiteLatitude { sexagesimal: e.text().into() }
                    ),
                    "Set site latitude"
                );
            })
        );

        self.widgets.site.e_long.connect_changed(
            clone!(@weak self as self_ => move |e| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetSiteLongitude { sexagesimal: e.text().into() }
                    ),
                    "Set site longitude"
                );
            })
        );

        self.widgets.telescope.chb_corr_foc_len.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetTelescopeFromPlateSolve { enabled: chb.is_active() }
                    ),
                    "Set telescope from platesolve"
                );
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
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetTelescopeFocalLen { focal_len: sb.value() }
                    ),
                    "Set telescope focal length"
                );
            })
        );

        self.widgets.telescope.spb_barlow.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetBarlow { barlow: sb.value() }
                    ),
                    "Set Barlow"
                );
            })
        );

        self.widgets.telescope.spb_guid_foc_len.connect_value_changed(
            clone!(@weak self as self_ => move |sb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetGuidingFocalLen { foc_len: sb.value() }
                    ),
                    "Set guiding focal length"
                );
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
        self.engine.ext_guider.phd2_conn().connect_event_handler(move |event| {
            // The receiver side may already be gone (module dropped on window close),
            // so a failed send is simply dropped instead of panicking
            _ = sender_clone.send_blocking(HardwareEvent::Phd2(event));
        });

        // Process incoming events in main thread
        // (weak ref is upgraded per event, see connect_indi_events)

        let weak_self = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                let Some(self_) = weak_self.upgrade() else { break; };
                match event {
                    HardwareEvent::Phd2(event) =>
                        self_.process_phd2_event(event),
                };
            }
        });
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
                log::debug!("(-) {:20}", event.device_name);
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
        let phd2_working = self.main_ui.snapshot().devices.phd2_working;
        enable_actions(&self.window, &[
            ("conn_indi",    conn_en(&indi_state)),
            ("disconn_indi", disconn_en(&indi_state)),
            ("conn_phd2",    !phd2_working),
            ("disconn_phd2", phd2_working),
        ]);

        #[cfg(windows)] {
            let aa_state = self.aa_state.borrow();
            enable_actions(&self.window, &[
                ("conn_aa",    conn_en(&aa_state)),
                ("disconn_aa", disconn_en(&aa_state)),
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

        let indi_drivers = query_indi_drivers(&self.engine);

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
        let cmd = {
            // Fresh: connect payload must see the latest eventless widget commits
            let snap = self.main_ui.fresh_snapshot();
            let options = &snap.options;
            ConnectCommand::ConnectIndi {
                remote:    options.indi.remote,
                address:   options.indi.address.clone(),
                mount:     options.indi.mount.clone(),
                camera:    options.indi.camera.clone(),
                guid_cam:  options.indi.guid_cam.clone(),
                focuser:   options.indi.focuser.clone(),
                flt_wheel: options.indi.flt_wheel.clone(),
                aux1:      options.indi.aux1.clone(),
                aux2:      options.indi.aux2.clone(),
            }
        };
        self.submit_connect_cmd(
            Command::Connect(cmd), PendingDomain::Indi, ConnectPending::IndiConnect
        );
    }

    fn handler_action_disconn_indi(&self) {
        self.submit_connect_cmd(
            Command::Connect(ConnectCommand::DisconnectIndi {
                disable_all: !self.is_remote.get(),
            }),
            PendingDomain::Indi, ConnectPending::Disconnect
        );
    }

    #[cfg(windows)]
    fn handler_action_conn_aa(&self) {
        // Fresh: connect payload must see the latest eventless widget commits
        let address = self.main_ui.fresh_snapshot().options.ascom_alpaca.address.clone();
        self.submit_connect_cmd(
            Command::Connect(ConnectCommand::ConnectAlpaca { address }),
            PendingDomain::Alpaca, ConnectPending::Plain
        );
    }

    #[cfg(windows)]
    fn handler_action_disconn_aa(&self) {
        self.submit_connect_cmd(
            Command::Connect(ConnectCommand::DisconnectAlpaca),
            PendingDomain::Alpaca, ConnectPending::Disconnect
        );
    }

    fn handler_action_conn_phd2(&self) {
        self.submit_connect_cmd(
            Command::Guider(GuiderCommand::ConnectPhd2),
            PendingDomain::Guider, ConnectPending::Guider
        );
    }

    fn handler_action_disconn_phd2(&self) {
        self.submit_connect_cmd(
            Command::Guider(GuiderCommand::DisconnectPhd2),
            PendingDomain::Guider, ConnectPending::Guider
        );
    }

    /// Commits a driver combobox selection to Options immediately
    /// (double-Option patch field: cleared selection = `None`).
    fn connect_driver_cb(
        self: &Rc<Self>,
        cb: &gtk::ComboBox,
        set_field: impl Fn(&mut IndiSelPatch, Option<Option<String>>) + 'static,
    ) {
        cb.connect_active_id_notify(clone!(@weak self as self_ => move |cb| {
            if self_.opts_syncing.get() { return; }
            let mut patch = IndiSelPatch::default();
            set_field(&mut patch, Some(cb.active_id().map(|id| id.to_string())));
            log_if_error(
                &self_.engine.exec_command(Command::SetIndiSelection(patch)),
                "Set INDI driver selection"
            );
        }));
    }

    fn fill_devices_name(&self) {
        fn fill_cb_list(
            drivers:    &indi::Drivers,
            cb:         &gtk::ComboBox,
            group_name: &str,
            active:     &Option<String>
        ) {
            let Ok(group) = drivers.get_group_by_name(group_name) else { return; };
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

        let drivers = query_indi_drivers(&self.engine);
        let snap = self.main_ui.snapshot();
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_mount_drivers,     "Telescopes",    &snap.options.indi.mount);
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_camera_drivers,    "CCDs",          &snap.options.indi.camera);
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_guid_cam_drivers,  "CCDs",          &snap.options.indi.guid_cam);
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_focuser_drivers,   "Focusers",      &snap.options.indi.focuser);
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_flt_wheel_drivers, "Filter Wheels", &snap.options.indi.flt_wheel);
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_aux1_drivers,      "Auxiliary",     &snap.options.indi.aux1);
        fill_cb_list(&drivers, &self.widgets.indi_drv.cb_aux2_drivers,      "Auxiliary",     &snap.options.indi.aux2);
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
                let all_props = match self.engine.exec_command(Command::Query(Query::IndiProperties { since: None }))? {
                    Reply::Query(q) => match *q {
                        QueryReply::IndiProperties(reply) => reply.properties,
                        _ => return Err(eyre::eyre!("unexpected query reply")),
                    },
                    _ => return Err(eyre::eyre!("unexpected reply")),
                };
                let file_name = fc.file().expect("File name").path().unwrap().with_extension("txt");
                let mut file = BufWriter::new(File::create(file_name)?);
                for prop in all_props {
                    for elem in prop.elements {
                        write!(
                            &mut file, "{:20}.{:27}.{:27} = ",
                            prop.device, prop.name, elem.name,
                        )?;
                        if let indi::PropValue::Blob(blob) = elem.value {
                            writeln!(&mut file, "[BLOB len={}]", blob.data_len)?;
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
        let snap = self.main_ui.snapshot();
        let options = &snap.options;
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
        self.submit_connect_cmd(
            Command::Indi(IndiCommand::SetSwitchElementForAllDevices {
                prop: prop_name.to_string(),
                element: elem_name.to_string(),
            }),
            PendingDomain::Indi, ConnectPending::Plain
        );
    }

    fn commit_site(&self, lat: &str, lon: &str) {
        log_if_error(
            &self.engine.exec_command(Command::SetSiteLatitude { sexagesimal: lat.to_string() }),
            "Set site latitude"
        );
        log_if_error(
            &self.engine.exec_command(Command::SetSiteLongitude { sexagesimal: lon.to_string() }),
            "Set site longitude"
        );
    }

    /// Submits `cmd` and records it pending under its owning `domain`
    /// (decides prune eligibility) and reply-handling `kind`.
    fn submit_connect_cmd(
        &self,
        cmd: Command,
        domain: PendingDomain,
        kind: ConnectPending,
    ) {
        match self.engine.submit_command(cmd) {
            Ok(id) => {
                _ = self.pending_cmds.borrow_mut()
                    .insert(id, PendingEntry { domain, kind });
            }
            Err(err) => show_error_message(Some(&self.window), "Error", &err.to_string()),
        }
    }

    fn handler_action_get_site_from_devices(self: &Rc<Self>) {
        // Blocking `site()` HAL calls: run on the command queue thread
        self.submit_connect_cmd(
            Command::Query(Query::AllMountSites),
            PendingDomain::MountQuery, ConnectPending::GetSites
        );
    }

    fn apply_mount_sites(&self, entries: &[MountSite]) {
        let result: Vec<&MountSite> = entries
            .iter()
            .filter(|e| e.site.latitude != 0.0 && e.site.longitude != 0.0)
            .collect();

        if result.is_empty() {
            show_error_message(Some(&self.window), "Error", "No geographic data found in connected devices!");
            return;
        }

        // Commit the site explicitly: `set_text` must not be the commit path
        // (no `changed` signal when the entries already hold the same text)
        if result.len() == 1 {
            let site = &result[0].site;
            let lat = indi::value_to_sexagesimal(site.latitude, true, 6);
            let lon = indi::value_to_sexagesimal(site.longitude, true, 6);
            self.widgets.site.e_lat.set_text(&lat);
            self.widgets.site.e_long.set_text(&lon);
            self.commit_site(&lat, &lon);
        } else {
            let menu = gtk::Menu::new();
            let e_lat = self.widgets.site.e_lat.clone();
            let e_long = self.widgets.site.e_long.clone();
            let engine = Arc::clone(&self.engine);
            for entry in result {
                let mi_text = format!(
                    "{} {} ({})",
                    indi::value_to_sexagesimal(entry.site.latitude, true, 6),
                    indi::value_to_sexagesimal(entry.site.longitude, true, 6),
                    entry.name
                );
                let menu_item = gtk::MenuItem::builder().label(mi_text).build();
                menu.append(&menu_item);
                let site = entry.site.clone();
                menu_item.connect_activate(
                    clone!(@weak e_lat, @weak e_long, @strong engine => move |_| {
                        let lat = indi::value_to_sexagesimal(site.latitude, true, 6);
                        let lon = indi::value_to_sexagesimal(site.longitude, true, 6);
                        e_lat.set_text(&lat);
                        e_long.set_text(&lon);
                        log_if_error(
                            &engine.exec_command(
                                Command::SetSiteLatitude { sexagesimal: lat }
                            ),
                            "Set site latitude"
                        );
                        log_if_error(
                            &engine.exec_command(
                                Command::SetSiteLongitude { sexagesimal: lon }
                            ),
                            "Set site longitude"
                        );
                    })
                );
            }
            menu.set_attach_widget(Some(&self.widgets.site.btn_get_site));
            menu.show_all();
            menu.popup_easy(gtk::gdk::ffi::GDK_BUTTON_SECONDARY as u32, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(domain: PendingDomain, kind: ConnectPending) -> PendingEntry {
        PendingEntry { domain, kind }
    }

    #[test]
    fn disconnect_prune_drops_only_transitioned_domain() {
        let mut pending: HashMap<u64, PendingEntry> = [
            (1, entry(PendingDomain::Indi, ConnectPending::Plain)),
            (2, entry(PendingDomain::Indi, ConnectPending::Disconnect)),
            (3, entry(PendingDomain::Guider, ConnectPending::Guider)),
            (4, entry(PendingDomain::MountQuery, ConnectPending::GetSites)),
            (5, entry(PendingDomain::Indi, ConnectPending::IndiConnect)),
        ].into_iter().collect();
        // INDI transitioned to Disconnected: only its non-Disconnect entries go
        assert_eq!(
            prune_aborted_by_disconnect(&mut pending, &[PendingDomain::Indi]), 2
        );
        let mut survivors: Vec<u64> = pending.keys().copied().collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![2, 3, 4]);
        assert_eq!(
            prune_aborted_by_disconnect(&mut pending, &[PendingDomain::Indi]), 0
        );
    }

    #[test]
    fn hal_transition_detection_ignores_duplicates_and_other_states() {
        let stored = RefCell::new(HalState::Connecting);
        assert!(hal_transitioned_to_disconnected(&stored, &HalState::Disconnected));
        // Duplicate `Disconnected` must not re-trigger the prune
        assert!(!hal_transitioned_to_disconnected(&stored, &HalState::Disconnected));
        assert!(!hal_transitioned_to_disconnected(&stored, &HalState::Connecting));
        assert_eq!(*stored.borrow(), HalState::Connecting);
    }

    #[test]
    fn disconnect_prune_keeps_guider_and_mount_query_entries() {
        let mut pending: HashMap<u64, PendingEntry> = [
            (1, entry(PendingDomain::Guider, ConnectPending::Guider)),
            (2, entry(PendingDomain::MountQuery, ConnectPending::GetSites)),
        ].into_iter().collect();
        assert_eq!(
            prune_aborted_by_disconnect(&mut pending, &[PendingDomain::Indi]), 0
        );
    }

    #[cfg(windows)]
    #[test]
    fn disconnect_prune_spares_foreign_hal_domain() {
        let mut pending: HashMap<u64, PendingEntry> = [
            (1, entry(PendingDomain::Alpaca, ConnectPending::Plain)),
            (2, entry(PendingDomain::Alpaca, ConnectPending::Disconnect)),
            (3, entry(PendingDomain::Indi, ConnectPending::IndiConnect)),
        ].into_iter().collect();
        // Only Alpaca transitioned: its Plain goes, INDI entries survive
        assert_eq!(
            prune_aborted_by_disconnect(&mut pending, &[PendingDomain::Alpaca]), 1
        );
        let mut survivors: Vec<u64> = pending.keys().copied().collect();
        survivors.sort_unstable();
        assert_eq!(survivors, vec![2, 3]);
    }
}
