use std::{
    rc::Rc,
    cell::{RefCell, Cell},
    collections::HashMap,
    borrow::Cow,
    io::{prelude::*, BufWriter},
    fs::File,
    sync::{RwLock, Arc},
};
use gtk::{prelude::*, glib, glib::clone};
use itertools::Itertools;
use chrono::prelude::*;
use crate::{
    indi,
    core::core::Core,
    options::*,
    guiding::phd2_conn,
};
use super::{ui_main::*, gtk_utils, ui_indi::*};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    main_ui:  &Rc<MainUi>,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    indi:     &Arc<indi::Connection>,
    handlers: &mut MainUiHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

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

    let indi_ui = IndiUi::new(&indi);

    let bx_devices_ctrl = builder.object::<gtk::Box>("bx_devices_ctrl").unwrap();
    bx_devices_ctrl.add(indi_ui.widget());

    let data = Rc::new(HardwareUi {
        core:          Arc::clone(core),
        indi:          Arc::clone(indi),
        options:       Arc::clone(options),
        builder:       builder.clone(),
        indi_status:   RefCell::new(indi::ConnState::Disconnected),
        indi_drivers:  drivers,
        indi_evt_conn: RefCell::new(None),
        is_remote:     Cell::new(false),
        main_ui:       Rc::clone(main_ui),
        indi_ui,
        window:        window.clone(),
        self_:         RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_widgets();
    data.fill_devices_name();
    data.connect_widgets_events();
    data.connect_indi_events();
    data.correct_widgets_by_cur_state();

    handlers.push(Box::new(clone!(@weak data => move |event| {
        data.handler_main_ui_event(event);
    })));

    if let Some(load_drivers_err) = load_drivers_err {
        data.add_log_record(
            &Some(Utc::now()),
            "",
            &format!("Load devices info error: {}", load_drivers_err)
        );
    }
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

impl GuidingMode {
    pub fn from_active_id(id: Option<&str>) -> Self {
        match id {
            Some("main_cam")  => Self::MainCamera,
            Some("phd2")      => Self::Phd2,
            _                 => Self::MainCamera,
        }
    }

    pub fn to_active_id(&self) -> Option<&'static str> {
        match self {
            Self::MainCamera => Some("main_cam"),
            Self::Phd2       => Some("phd2"),
        }
    }
}

enum HardwareEvent {
    Indi(indi::Event),
    Phd2(phd2_conn::Event),
}

struct HardwareUi {
    main_ui:       Rc<MainUi>,
    core:          Arc<Core>,
    indi:          Arc<indi::Connection>,
    options:       Arc<RwLock<Options>>,
    builder:       gtk::Builder,
    window:        gtk::ApplicationWindow,
    indi_status:   RefCell<indi::ConnState>,
    indi_drivers:  indi::Drivers,
    indi_evt_conn: RefCell<Option<indi::Subscription>>,
    indi_ui:       IndiUi,
    is_remote:     Cell<bool>,
    self_:         RefCell<Option<Rc<HardwareUi>>>,
}

impl Drop for HardwareUi {
    fn drop(&mut self) {
        log::info!("HardwareUi dropped");
    }
}

impl HardwareUi {
    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action(&self.window, self, "help_save_indi",      HardwareUi::handler_action_help_save_indi);
        gtk_utils::connect_action(&self.window, self, "conn_indi",           HardwareUi::handler_action_conn_indi);
        gtk_utils::connect_action(&self.window, self, "disconn_indi",        HardwareUi::handler_action_disconn_indi);
        gtk_utils::connect_action(&self.window, self, "conn_guid",           HardwareUi::handler_action_conn_guider);
        gtk_utils::connect_action(&self.window, self, "disconn_guid",        HardwareUi::handler_action_disconn_guider);
        gtk_utils::connect_action(&self.window, self, "clear_hw_log",        HardwareUi::handler_action_clear_hw_log);
        gtk_utils::connect_action(&self.window, self, "enable_all_devs",     HardwareUi::handler_action_enable_all_devices);
        gtk_utils::connect_action(&self.window, self, "disable_all_devs",    HardwareUi::handler_action_disable_all_devices);
        gtk_utils::connect_action(&self.window, self, "save_devs_options",   HardwareUi::handler_action_save_devices_options);
        gtk_utils::connect_action(&self.window, self, "load_devs_options",   HardwareUi::handler_action_load_devices_options);

        let chb_remote = self.builder.object::<gtk::CheckButton>("chb_remote").unwrap();
        chb_remote.connect_active_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_by_cur_state();
        }));

        let ch_guide_mode = self.builder.object::<gtk::ComboBoxText>("ch_guide_mode").unwrap();
        ch_guide_mode.connect_active_id_notify(clone!(@weak self as self_ => move |_| {
            self_.correct_widgets_by_cur_state();
        }));

        let se_hw_prop_name = self.builder.object::<gtk::SearchEntry>("se_hw_prop_name").unwrap();
        se_hw_prop_name.connect_search_changed(clone!(@weak self as self_ => move |se| {
            let text_lc = se.text().to_lowercase();
            self_.indi_ui.set_filter_text(&text_lc);
        }));
    }

    fn handler_main_ui_event(&self, event: MainUiEvent) {
        match event {
            MainUiEvent::ProgramClosing =>
                self.handler_closing(),

            MainUiEvent::TabPageChanged(tab_page) =>
                self.indi_ui.set_enabled(tab_page == TabPage::Hardware),

            _ => {},
        }
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

        *self.self_.borrow_mut() = None;
    }

    fn init_widgets(&self) {
        let spb_foc_len = self.builder.object::<gtk::SpinButton>("spb_foc_len").unwrap();
        spb_foc_len.set_range(10.0, 10_000.0);
        spb_foc_len.set_digits(0);
        spb_foc_len.set_increments(1.0, 10.0);

        let spb_barlow = self.builder.object::<gtk::SpinButton>("spb_barlow").unwrap();
        spb_barlow.set_range(0.1, 10.0);
        spb_barlow.set_digits(2);
        spb_barlow.set_increments(0.01, 0.1);

        let spb_guid_foc_len = self.builder.object::<gtk::SpinButton>("spb_guid_foc_len").unwrap();
        spb_guid_foc_len.set_range(0.0, 1000.0);
        spb_guid_foc_len.set_digits(0);
        spb_guid_foc_len.set_increments(1.0, 10.0);
    }

    fn correct_widgets_by_cur_state(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let status = self.indi_status.borrow();
        let (conn_en, disconn_en) = match *status {
            indi::ConnState::Disconnected  => (true,  false),
            indi::ConnState::Connecting    => (false, false),
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
        let phd2_acessible = {
            let guiding_mode_str = ui.prop_string("ch_guide_mode.active-id");
            let guiding_mode = GuidingMode::from_active_id(guiding_mode_str.as_deref());
            guiding_mode == GuidingMode::Phd2
        };
        gtk_utils::enable_actions(&self.window, &[
            ("conn_indi",    conn_en),
            ("disconn_indi", disconn_en),
            ("conn_guid",    !phd2_working && phd2_acessible),
            ("disconn_guid", phd2_working && phd2_acessible),
        ]);
        ui.set_prop_str("lbl_indi_conn_status.label", Some(&status.to_str(false)));

        let remote = ui.prop_bool("chb_remote.active");

        let (conn_cap, disconn_cap) = if remote {
            ("Connect INDI", "Disconnect INDI")
        } else {
            ("Start INDI", "Stop INDI")
        };
        ui.set_prop_str("btn_conn_indi.label", Some(conn_cap));
        ui.set_prop_str("btn_diconn_indi.label", Some(disconn_cap));

        let mnt_sensitive = !remote && disconnected && !ui.is_combobox_empty("cb_mount_drivers");
        let cam_sensitive = !remote && disconnected && !ui.is_combobox_empty("cb_camera_drivers");
        let guid_cam_sensitive = !remote && disconnected && !ui.is_combobox_empty("cb_guid_cam_drivers");
        let foc_sensitive = !remote && disconnected && !ui.is_combobox_empty("cb_focuser_drivers");
        ui.enable_widgets(false, &[
            ("l_mount_drivers",     mnt_sensitive),
            ("cb_mount_drivers",    mnt_sensitive),
            ("l_camera_drivers",    cam_sensitive),
            ("cb_camera_drivers",   cam_sensitive),
            ("l_guid_cam_drivers",  guid_cam_sensitive),
            ("cb_guid_cam_drivers", guid_cam_sensitive),
            ("l_focuser_drivers",   foc_sensitive),
            ("cb_focuser_drivers",  foc_sensitive),
            ("chb_remote",          !self.indi_drivers.groups.is_empty() && disconnected),
            ("e_remote_addr",       remote && disconnected),
            ("ch_guide_mode",       !phd2_working),
        ]);

        gtk_utils::enable_actions(&self.window, &[
            ("enable_all_devs",   connected),
            ("disable_all_devs",  connected),
            ("save_devs_options", connected),
            ("load_devs_options", connected),
        ]);
    }

    fn connect_indi_events(self: &Rc<Self>) {
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

    fn process_indi_event(&self, event: indi::Event) {
        match event {
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
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_str("lbl_phd2_status.label", Some(status_text));
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
            self.main_ui.exec_before_disconnect_handlers();
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

    fn handler_action_conn_guider(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            self.read_options_from_widgets();
            self.core.create_ext_guider()?;
            self.correct_widgets_by_cur_state();
            Ok(())
        });
    }

    fn handler_action_disconn_guider(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.disconnect_ext_guider()?;
            self.correct_widgets_by_cur_state();
            Ok(())
        });
    }

    fn fill_devices_name(&self) {
        fn fill_cb_list(
            data:       &HardwareUi,
            cb_name:    &str,
            group_name: &str,
            active:     &Option<String>
        ) {
            let Ok(group) = data.indi_drivers.get_group_by_name(group_name) else { return; };
            let model = gtk::TreeStore::new(&[String::static_type(), String::static_type()]);
            let cb = data.builder.object::<gtk::ComboBox>(cb_name).unwrap();
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
        fill_cb_list(self, "cb_mount_drivers",    "Telescopes", &options.indi.mount);
        fill_cb_list(self, "cb_camera_drivers",   "CCDs",       &options.indi.camera);
        fill_cb_list(self, "cb_guid_cam_drivers", "CCDs",       &options.indi.guid_cam);
        fill_cb_list(self, "cb_focuser_drivers",  "Focusers",   &options.indi.focuser);
    }

    fn read_options_from_widgets(&self) {
        let mut options = self.options.write().unwrap();
        options.read_indi(&self.builder);
        options.read_telescope(&self.builder);
        options.read_guiding(&self.builder);
    }

    fn add_log_record(
        &self,
        timestamp:   &Option<DateTime<Utc>>,
        device_name: &str,
        text:        &str,
    ) {
        let tv_hw_log = self.builder.object::<gtk::TreeView>("tv_hw_log").unwrap();
        let model = match tv_hw_log.model() {
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
                    tv_hw_log.append_column(&col);
                }
                tv_hw_log.set_model(Some(&model));
                model
            },
        };
        let models_row_cnt = gtk_utils::get_model_row_count(model.upcast_ref());
        let last_is_selected =
            gtk_utils::get_list_view_selected_row(&tv_hw_log).map(|v| v+1) ==
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
            tv_hw_log.selection().select_iter(&last);
            if let [path] = tv_hw_log.selection().selected_rows().0.as_slice() {
                tv_hw_log.set_cursor(
                    path,
                    Option::<&gtk::TreeViewColumn>::None,
                    false
                );
            }
        }
    }

    fn handler_action_clear_hw_log(&self) {
        let tv_hw_log = self.builder.object::<gtk::TreeView>("tv_hw_log").unwrap();
        let Some(model) = tv_hw_log.model() else { return; };
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
            .filter_map(|(str, v)| v.as_deref().map(
                |v| format!("{}: {}", str, v)
            ))
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
}