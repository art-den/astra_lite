use std::{
    rc::Rc,
    cell::{RefCell, Cell},
    collections::HashMap,
    borrow::Cow,
    io::{prelude::*, BufWriter},
    fs::File,
};

use gtk::{prelude::*, glib, glib::clone};
use itertools::Itertools;
use serde::{Serialize, Deserialize};
use crate::{gui_main::*, gtk_utils, indi_api, io_utils::*, gui_indi::*};
use chrono::prelude::*;

impl indi_api::ConnState {
    fn to_str(&self, short: bool) -> Cow<str> {
        match self {
            indi_api::ConnState::Disconnected =>
                Cow::from("Disconnected"),
            indi_api::ConnState::Connecting =>
                Cow::from("Connecting..."),
            indi_api::ConnState::Connected =>
                Cow::from("Connected"),
            indi_api::ConnState::Disconnecting =>
                Cow::from("Disconnecting..."),
            indi_api::ConnState::Error(text) =>
                if short { Cow::from("Connection error") }
                else { Cow::from(format!("Error: {}", text)) },
        }
    }
}


#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct HardwareOptions {
    mount: Option<String>,
    camera: Option<String>,
    focuser: Option<String>,
    remote: bool,
    address: String,
}

impl Default for HardwareOptions {
    fn default() -> Self {
        Self {
            mount: None,
            camera: None,
            focuser: None,
            remote: false,
            address: "127.0.0.1".to_string(),
        }
    }
}
struct HardwareData {
    main:         Rc<MainData>,
    options:      RefCell<HardwareOptions>,
    indi_drivers: indi_api::Drivers,
    indi_conn:    RefCell<Option<indi_api::Subscription>>,
    indi_gui:     IndiGui,
    remote:       Cell<bool>,
}

impl Drop for HardwareData {
    fn drop(&mut self) {
        log::info!("HardwareData dropped");
    }
}

pub fn build_ui(
    _application: &gtk::Application,
    data:         &Rc<MainData>
) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let mut options = HardwareOptions::default();
        load_json_from_config_file(&mut options, "conf_hardware")?;

        let sidebar = data.builder.object("sdb_indi").unwrap();
        let stack = data.builder.object("stk_indi").unwrap();

        let (drivers, load_drivers_err) = match indi_api::Drivers::new() {
            Ok(drivers) =>
                (drivers, None),
            Err(err) =>
                (indi_api::Drivers::new_empty(), Some(err.to_string())),
        };

        if drivers.groups.is_empty() {
            options.remote = true; // force remote mode if no devices info
        }

        let hw_data = Rc::new(HardwareData {
            main:         Rc::clone(data),
            options:      RefCell::new(options),
            indi_drivers: drivers,
            indi_conn:    RefCell::new(None),
            indi_gui:     IndiGui::new(&data.indi, sidebar, stack),
            remote:       Cell::new(false),
        });

        fill_devices_name(&hw_data);
        show_options(&hw_data);

        gtk_utils::connect_action(&data.window, &hw_data, "help_save_indi", handler_action_help_save_indi);
        gtk_utils::connect_action(&data.window, &hw_data, "conn_indi",      handler_action_conn_indi);
        gtk_utils::connect_action(&data.window, &hw_data, "disconn_indi",   handler_action_disconn_indi);
        gtk_utils::connect_action(&data.window, &hw_data, "clear_hw_log",   handler_action_clear_hw_log);

        connect_indi_events(&hw_data);
        correct_ctrls_by_cur_state(&hw_data);

        data.window.connect_delete_event(clone!(@weak hw_data => @default-panic, move |_, _| {
            handler_close_window(&hw_data)
        }));

        if let Some(load_drivers_err) = load_drivers_err {
            add_log_record(
                &hw_data,
                &Some(Utc::now()),
                "",
                &format!("Load devices info error: {}", load_drivers_err)
            );
        }

        Ok(())
    });

}

fn handler_close_window(data: &Rc<HardwareData>) -> gtk::Inhibit {
    let options = data.options.borrow();
    _ = save_json_to_config::<HardwareOptions>(&options, "conf_hardware");

    if let Some(indi_conn) = data.indi_conn.borrow_mut().take() {
        data.main.indi.unsubscribe(indi_conn);
    }

    if !data.remote.get() {
        _ = data.main.indi.command_enable_all_devices(false, true, Some(2000));
    }

    log::info!("Disconnecting from INDI...");
    _ = data.main.indi.disconnect_and_wait();
    log::info!("Done!");

    gtk::Inhibit(false)
}

fn correct_ctrls_by_cur_state(data: &Rc<HardwareData>) {
    let status = data.main.indi_status.borrow();
    let (conn_en, disconn_en) = match *status {
        indi_api::ConnState::Disconnected  => (true,  false),
        indi_api::ConnState::Connecting    => (false, false),
        indi_api::ConnState::Disconnecting => (false, false),
        indi_api::ConnState::Connected     => (false, true),
        indi_api::ConnState::Error(_)      => (true,  false),
    };
    gtk_utils::enable_actions(&data.main.window, &[
        ("conn_indi",    conn_en),
        ("disconn_indi", disconn_en),
    ]);
    gtk_utils::set_str(
        &data.main.builder,
        "lbl_indi_conn_status",
        &status.to_str(false)
    );

    let disconnected = matches!(
        *status,
        indi_api::ConnState::Disconnected|
        indi_api::ConnState::Error(_)
    );
    gtk_utils::enable_widgets(&data.main.builder, &[
        ("cb_mount",   disconnected && !gtk_utils::is_named_combobox_empty(&data.main.builder, "cb_mount")),
        ("cb_camera",  disconnected && !gtk_utils::is_named_combobox_empty(&data.main.builder, "cb_camera")),
        ("cb_focuser", disconnected && !gtk_utils::is_named_combobox_empty(&data.main.builder, "cb_focuser")),
        ("chb_remote", !data.indi_drivers.groups.is_empty()),
    ]);
}


fn connect_indi_events(data: &Rc<HardwareData>) {
    let (sender, receiver) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
    *data.indi_conn.borrow_mut() = Some(data.main.indi.subscribe_events(move |event| {
        sender.send(event).unwrap();
    }));

    fn log_prop_change(
        device_name: &str,
        prop_name:   &str,
        new_prop:    bool,
        value:       &indi_api::PropChangeValue,
    ) {
        if log::log_enabled!(log::Level::Debug) {
            let prop_name_string = format!(
                "({}) {:20}.{:27}.{:27}",
                if new_prop { "+" } else { "*" },
                device_name,
                prop_name,
                value.elem_name,
            );
            let prop_value_string = match &value.prop_value {
                indi_api::PropValue::Blob(blob) =>
                    format!("[BLOB len={}]", blob.data.len()),
                _ =>
                    format!("{:?}", value.prop_value)
            };
            log::debug!("{} = {}", prop_name_string, prop_value_string);
        }
    }

    let data = Rc::downgrade(data);
    receiver.attach(None, move |event| {
        let Some(data) = data.upgrade() else { return Continue(false); };
        match event {
            indi_api::Event::ConnChange(conn_state) => {
                if let indi_api::ConnState::Error(_) = &conn_state {
                    add_log_record(&data, &Some(Utc::now()), "", &conn_state.to_str(false))
                }
                *data.main.indi_status.borrow_mut() = conn_state;
                correct_ctrls_by_cur_state(&data);
                update_window_title(&data);
            },
            indi_api::Event::PropChange(event) => {
                match &event.change {
                    indi_api::PropChange::New(value) => {
                        log_prop_change(&event.device_name, &event.prop_name, true, value);
                    },
                    indi_api::PropChange::Change(value) => {
                        log_prop_change(&event.device_name, &event.prop_name, false, value);
                    },
                    indi_api::PropChange::Delete => {
                        log::debug!("(-) {:20}.{:27}", &event.device_name, &event.prop_name);
                    },
                };
            },
            indi_api::Event::DeviceDelete(event) => {
                log::debug!("(-) {:20}", &event.device_name);
            },
            indi_api::Event::Message(message) => {
                log::info!("indi: device={}, text={}", message.device_name, message.text);
                add_log_record(
                    &data,
                    &message.timestamp,
                    &message.device_name,
                    &message.text
                );
            },
            _ => {},
        }
        Continue(true)
    });
}

fn handler_action_conn_indi(data: &Rc<HardwareData>) {
    read_options_from_widgets(data);
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let options = data.options.borrow();
        let drivers = if !options.remote {
            let telescopes = data.indi_drivers.get_group_by_name("Telescopes")?;
            let cameras = data.indi_drivers.get_group_by_name("CCDs")?;
            let focusers = data.indi_drivers.get_group_by_name("Focusers")?;
            let telescope_driver_name = options.mount.as_ref()
                .and_then(|name| telescopes.get_item_by_device_name(name))
                .map(|d| &d.driver);
            let camera_driver_name = options.camera.as_ref()
                .and_then(|name| cameras.get_item_by_device_name(name))
                .map(|d| &d.driver);
            let focuser_driver_name = options.focuser.as_ref()
                .and_then(|name| focusers.get_item_by_device_name(name))
                .map(|d| &d.driver);
            [ telescope_driver_name,
              camera_driver_name,
              focuser_driver_name
            ].iter()
                .filter_map(|v| *v)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        if !options.remote && drivers.is_empty() {
            anyhow::bail!("No devices selected");
        }
        let conn_settings = indi_api::ConnSettings {
            drivers,
            remote:               options.remote,
            host:                 options.address.clone(),
            activate_all_devices: !options.remote,
            .. Default::default()
        };
        data.remote.set(options.remote);
        drop(options);
        data.main.indi.connect(&conn_settings)?;
        Ok(())
    });
}

fn handler_action_disconn_indi(data: &Rc<HardwareData>) {
    gtk_utils::exec_and_show_error(&data.main.window, || {
        if !data.remote.get() {
            data.main.indi.command_enable_all_devices(false, true, Some(2000))?;
        }
        data.main.indi.disconnect_and_wait()?;
        Ok(())
    });
}

fn fill_devices_name(data: &Rc<HardwareData>) {
    fn fill_cb_list(
        data:       &Rc<HardwareData>,
        cb_name:    &str,
        group_name: &str,
        active:     &Option<String>
    ) {
        let Ok(group) = data.indi_drivers.get_group_by_name(group_name) else { return; };
        let model = gtk::TreeStore::new(&[String::static_type(), String::static_type()]);
        let cb = data.main.builder.object::<gtk::ComboBox>(cb_name).unwrap();
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
    gtk_utils::exec_and_show_error(&data.main.window, || {
        let options = data.options.borrow();
        fill_cb_list(data, "cb_mount",   "Telescopes", &options.mount);
        fill_cb_list(data, "cb_camera",  "CCDs",       &options.camera);
        fill_cb_list(data, "cb_focuser", "Focusers",   &options.focuser);
        Ok(())
    });
}

fn show_options(data: &Rc<HardwareData>) {
    let bldr = &data.main.builder;
    let options = data.options.borrow();
    gtk_utils::set_bool(bldr, "chb_remote",    options.remote);
    gtk_utils::set_str (bldr, "e_remote_addr", &options.address);
}

fn read_options_from_widgets(data: &Rc<HardwareData>) {
    let bldr = &data.main.builder;
    let mut options = data.options.borrow_mut();
    options.mount   = gtk_utils::get_active_id(bldr, "cb_mount");
    options.camera  = gtk_utils::get_active_id(bldr, "cb_camera");
    options.focuser = gtk_utils::get_active_id(bldr, "cb_focuser");
    options.remote  = gtk_utils::get_bool     (bldr, "chb_remote");
    options.address = gtk_utils::get_string   (bldr, "e_remote_addr");
}

fn add_log_record(
    data:        &Rc<HardwareData>,
    timestamp:   &Option<DateTime<Utc>>,
    device_name: &str,
    text:        &str,
) {
    let tv_hw_log = data.main.builder.object::<gtk::TreeView>("tv_hw_log").unwrap();
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

fn handler_action_clear_hw_log(data: &Rc<HardwareData>) {
    let tv_hw_log = data.main.builder.object::<gtk::TreeView>("tv_hw_log").unwrap();
    let Some(model) = tv_hw_log.model() else { return; };
    let model = model.downcast::<gtk::ListStore>().unwrap();
    model.clear();
}

fn handler_action_help_save_indi(data: &Rc<HardwareData>) {
    let ff = gtk::FileFilter::new();
        ff.set_name(Some("Text files"));
        ff.add_pattern("*.txt");
    let fc = gtk::FileChooserDialog::builder()
        .action(gtk::FileChooserAction::Save)
        .title("Enter file name to save properties")
        .filter(&ff)
        .modal(true)
        .transient_for(&data.main.window)
        .build();
    fc.add_buttons(&[
        ("_Cancel", gtk::ResponseType::Cancel),
        ("_Save", gtk::ResponseType::Accept),
    ]);
    let resp = fc.run();
    fc.close();
    if resp == gtk::ResponseType::Accept {
        gtk_utils::exec_and_show_error(&data.main.window, || {
            let all_props = data.main.indi.get_properties_list(None, None);
            let file_name = fc.file().expect("File name").path().unwrap().with_extension("txt");
            let mut file = BufWriter::new(File::create(file_name)?);
            for prop in all_props {
                for elem in prop.elements {
                    write!(
                        &mut file, "{:20}.{:27}.{:27} = ",
                        prop.device, prop.name, elem.name,
                    )?;
                    if let indi_api::PropValue::Blob(blob) = elem.value {
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

fn update_window_title(data: &Rc<HardwareData>) {
    let options = data.options.borrow();
    let status = data.main.indi_status.borrow();
    let dev_list = [
        ("mount",   &options.mount),
        ("camera",  &options.camera),
        ("focuser", &options.focuser),
    ].iter()
        .filter_map(|(str, v)| v.as_deref().map(
            |v| format!("{}: {}", str, v)
        ))
        .join(", ");
    data.main.set_dev_list_and_conn_status(
        dev_list,
        status.to_str(true).to_string()
    );
}