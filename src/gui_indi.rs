use std::{sync::Arc, time::Duration, cell::RefCell, rc::Rc};
use gtk::{prelude::*, glib};
use itertools::{Itertools, izip};
use crate::indi_api;

pub struct IndiGui {
    indi:      Arc<indi_api::Connection>,
    indi_conn: indi_api::Subscription,
    data:      Rc<RefCell<UiIndiGuiData>>,
}

impl Drop for IndiGui {
    fn drop(&mut self) {
        self.indi.unsubscribe(self.indi_conn);
    }
}
const CSS: &[u8] = b"
.indi_on_btn {
    text-decoration: underline;
    font-weight: bold;
    background: rgba(0, 180, 255, .3);
}
";

impl IndiGui {
    pub fn new(
        indi:    &Arc<indi_api::Connection>,
        sidebar: gtk::StackSidebar,
        stack:   gtk::Stack,
    ) -> Self {
        let css_provider = gtk::CssProvider::new();
        css_provider.load_from_data(CSS).unwrap();
        gtk::StyleContext::add_provider_for_screen(
            &gtk::gdk::Screen::default().expect("Could not connect to a display."),
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let (sender, receiver) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);

        let indi_conn = indi.subscribe_events(move |evt| {
            sender.send(evt).unwrap();
        });

        let data = Rc::new(RefCell::new(UiIndiGuiData {
            devices: Vec::new(),
            sidebar,
            stack,
            prop_changed: true,
            list_changed: true,
            last_change_cnt: 0,
        }));

        let data_weak = Rc::downgrade(&data);
        receiver.attach(None, move |event| {
            let Some(data) = data_weak.upgrade() else {
                return Continue(false);
            };
            let mut data = data.borrow_mut();
            match event {
                indi_api::Event::ConnChange(_) |
                indi_api::Event::DeviceDelete(_) =>
                    data.list_changed = true,
                indi_api::Event::PropChange(pch) =>
                    match pch.change {
                        indi_api::PropChange::New(_) |
                        indi_api::PropChange::Delete =>
                            data.list_changed = true,
                        indi_api::PropChange::Change{..} =>
                            data.prop_changed = true,
                    },
                _ =>
                    {},
            };
            Continue(true)
        });

        let data_weak = Rc::downgrade(&data);
        let indi_clone = Arc::clone(indi);
        glib::timeout_add_local(
            Duration::from_millis(200),
            move || {
                let Some(data) = data_weak.upgrade() else { return Continue(false); };
                let mut data = data.borrow_mut();
                if data.prop_changed || data.list_changed {
                    let list_changed = data.list_changed;
                    data.prop_changed = false;
                    data.list_changed = false;
                    Self::show_all_props(&indi_clone, &mut data, list_changed);
                }
                Continue(true)
            }
        );

        Self {
            data,
            indi: Arc::clone(indi),
            indi_conn,
        }
    }

    fn show_all_props(
        indi:        &Arc<indi_api::Connection>,
        data:        &mut UiIndiGuiData,
        update_list: bool
    ) {
        let indi_props = indi.get_properties_list(
            None,
            if update_list { None } else { Some(data.last_change_cnt) }
        );
        let indi_devices: Vec<_> = indi_props.iter()
            .map(|p| p.device.as_str())
            .unique()
            .collect();

        if update_list {
            // add devices into sidebar
            for &indi_device in &indi_devices {
                if !data.devices.iter().any(|d| d.name == *indi_device) {
                    let notebook = gtk::Notebook::builder()
                        .visible(true)
                        .tab_pos(gtk::PositionType::Left)
                        .build();
                    data.stack.add_titled(&notebook, indi_device, indi_device);
                    data.devices.push(UiIndiDevice {
                        name: indi_device.to_string(),
                        groups: Vec::new(),
                        notebook,
                    });
                }
            }

            // remove devices from sidebar
            for ui_device in &data.devices {
                if !indi_devices.iter().any(|&d| *d == ui_device.name) {
                    data.stack.remove(&ui_device.notebook);
                }
            }
            data.devices.retain(|existing|
                indi_devices.iter().any(|&d|
                    *d == existing.name
                )
            );
        }

        // build device UI
        for indi_device in indi_devices {
            let ui_device = data.devices.iter_mut().find(|d| d.name == indi_device).unwrap();
            Self::show_device_props(indi, ui_device, &indi_props, update_list);
        }

        let max_change_cnt = indi_props
            .iter()
            .map(|p| p.dynamic_data.change_cnt)
            .max();
        if let Some(max_change_cnt) = max_change_cnt {
            data.last_change_cnt = max_change_cnt;
        }

    }

    fn show_device_props(
        indi:        &Arc<indi_api::Connection>,
        ui_device:   &mut UiIndiDevice,
        indi_props:  &Vec<indi_api::ExportProperty>,
        update_list: bool
    ) {
        let indi_groups: Vec<_> = indi_props.iter()
            .filter(|p| p.device == ui_device.name)
            .map(|p| p.static_data.group.as_deref().unwrap_or_default())
            .unique()
            .collect();

        if update_list {
            // add properties groups into notebook
            for indi_group in indi_groups.iter().copied() {
                if !ui_device.groups.iter().any(|g| g.name == indi_group) {
                    let tab_label = gtk::Label::builder().label(indi_group).build();
                    let scrollwin = gtk::ScrolledWindow::builder()
                        .visible(true)
                        .overlay_scrolling(false)
                        .build();
                    let grid = gtk::Grid::builder()
                        .expand(false)
                        .visible(true)
                        .column_spacing(5)
                        .row_spacing(8)
                        .margin(8)
                        .build();
                    scrollwin.add(&grid);
                    ui_device.notebook.append_page(&scrollwin, Some(&tab_label));
                    ui_device.groups.push(UiIndiPropsGroup {
                        name: indi_group.to_string(),
                        props: Vec::new(),
                        grid,
                        scrollwin,
                        next_row: 0,
                    });
                }
            }

            // remove properties groups from notebook
            for ui_group in &ui_device.groups {
                if !indi_groups.iter().any(|g| *g == ui_group.name) {
                    let page_num = ui_device.notebook.page_num(&ui_group.scrollwin).unwrap();
                    ui_device.notebook.remove_page(Some(page_num));
                }
            }
            ui_device.groups.retain(|existing|
                indi_groups.iter().any(|g|
                    *g == existing.name
                )
            );
        }

        // build device properties group UI
        for indi_group in indi_groups {
            let ui_group = ui_device.groups.iter_mut().find(|g| g.name == indi_group).unwrap();
            Self::show_device_prop_group(indi, &ui_device.name, ui_group, indi_props, update_list);
        }
    }

    fn show_device_prop_group(
        indi:        &Arc<indi_api::Connection>,
        device_name: &str,
        ui_group:    &mut UiIndiPropsGroup,
        indi_props:  &Vec<indi_api::ExportProperty>,
        update_list: bool
    ) {
        let indi_group_props: Vec<_> = indi_props.iter()
            .filter(|p|
                p.device == device_name &&
                p.static_data.group.as_deref().unwrap_or_default() == ui_group.name
            )
            .collect();

        if update_list {
            // new props
            for &indi_prop in &indi_group_props {
                if !ui_group.props.iter().any(|p| p.name == indi_prop.name) {
                    let caption = indi_prop.static_data.label.as_deref().unwrap_or(&indi_prop.name);
                    let prop_label = gtk::Label::builder()
                        .use_markup(true)
                        .label(&format!("<b>{}</b>", caption))
                        .visible(true)
                        .halign(gtk::Align::End)
                        .tooltip_text(&indi_prop.name)
                        .build();
                    ui_group.grid.attach(&prop_label, 0, ui_group.next_row, 1, 1);
                    let prop_ui_elements = Self::create_property_ui(
                        indi,
                        indi_prop,
                        &ui_group.grid,
                        &mut ui_group.next_row,
                    );
                    ui_group.props.push(UiIndiProp {
                        device:     indi_prop.device.clone(),
                        name:       indi_prop.name.clone(),
                        elements:   prop_ui_elements,
                        sep_row:    ui_group.next_row,
                        change_cnt: 0,
                    });
                    let separator = gtk::Separator::builder()
                        .visible(true)
                        .orientation(gtk::Orientation::Horizontal)
                        .build();
                    ui_group.grid.attach(&separator, 0, ui_group.next_row, 6, 1);
                    ui_group.next_row += 1;
                }
            }

            // deleted props
            let mut grid_rows_to_delete = Vec::new();
            for ui_prop in &ui_group.props {
                if !indi_group_props.iter().any(|p| p.name == ui_prop.name) {
                    grid_rows_to_delete.push(ui_prop.sep_row);
                    for ui_elem in &ui_prop.elements {
                        grid_rows_to_delete.push(ui_elem.row);
                    }
                }
            }
            if !grid_rows_to_delete.is_empty() {
                let unique_rows = grid_rows_to_delete
                    .into_iter()
                    .sorted_by_key(|&v| -v)
                    .unique();
                for row in unique_rows {
                    for ui_prop in &mut ui_group.props {
                        if ui_prop.sep_row > row { ui_prop.sep_row -= 1; }
                        for ui_elem in &mut ui_prop.elements {
                            if ui_elem.row > row { ui_elem.row -= 1; }
                        }
                    }
                    ui_group.grid.remove_row(row);
                }
            }
            ui_group.props.retain(|existing|
                indi_group_props.iter().any(|p|
                    p.name == existing.name
                )
            );
        }

        // Update properties values
        for indi_prop in indi_group_props {
            let ui_prop = ui_group.props.iter_mut().find(|p| p.name == indi_prop.name).unwrap();
            if indi_prop.dynamic_data.change_cnt != ui_prop.change_cnt {
                ui_prop.change_cnt = indi_prop.dynamic_data.change_cnt;
                Self::show_property_values(ui_prop, indi_prop);
            }
        }
    }

    fn create_property_ui(
        indi:      &Arc<indi_api::Connection>,
        prop:      &indi_api::ExportProperty,
        grid:      &gtk::Grid,
        next_row:  &mut i32,
    ) -> Vec<UiIndiPropElem> {
        match &prop.static_data.tp {
            indi_api::PropType::Text =>
                Self::create_text_property_ui(
                    indi,
                    &prop.device,
                    &prop.name,
                    &prop.static_data,
                    &prop.elements,
                    grid,
                    next_row
                ),
            indi_api::PropType::Num(elems_info) =>
                Self::create_num_property_ui(
                    indi,
                    &prop.device,
                    &prop.name,
                    &prop.static_data,
                    &prop.elements,
                    elems_info,
                    grid,
                    next_row
                ),
            indi_api::PropType::Switch(rule) =>
                Self::create_switch_property_ui(
                    indi,
                    &prop.device,
                    &prop.name,
                    &prop.static_data,
                    &prop.elements,
                    rule,
                    grid,
                    next_row
                ),
            indi_api::PropType::Blob =>
                Self::create_blob_property_ui(
                    indi,
                    &prop.device,
                    &prop.name,
                    &prop.static_data,
                    &prop.elements,
                    grid,
                    next_row
                ),
            indi_api::PropType::Light =>
                Self::create_light_property_ui(
                    indi,
                    &prop.device,
                    &prop.name,
                    &prop.static_data,
                    &prop.elements,
                    grid,
                    next_row
                ),
        }
    }

    fn create_text_property_ui(
        indi:        &Arc<indi_api::Connection>,
        device:      &str,
        prop_name:   &str,
        static_data: &indi_api::PropStaticData,
        elements:    &Vec<indi_api::PropElement>,
        grid:        &gtk::Grid,
        next_row:    &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let start_row = *next_row;
        let mut btn_click_data = Vec::new();
        for elem in elements {
            let elem_label = gtk::Label::builder()
                .label(elem.label.as_deref().unwrap_or(&elem.name))
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", prop_name, elem.name))
                .build();
            grid.attach(&elem_label, 1, *next_row, 1, 1);

            let ro = static_data.perm == indi_api::PropPerm::RO;
            let entry = gtk::Entry::builder()
                .editable(!ro)
                .visible(true)
                .can_focus(!ro)
                .build();
            grid.attach(&entry, 2, *next_row, 2, 1);
            btn_click_data.push((
                elem.name.clone(),
                entry.clone(),
            ));

            let data = UiIndiPropElemData::Text(UiIndiPropTextElem {
                entry,
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                data,
                row: *next_row,
            });
            *next_row += 1;
        }
        if static_data.perm != indi_api::PropPerm::RO {
            let set_button = gtk::Button::builder()
                .visible(true)
                .label("Set")
                .build();
            grid.attach(&set_button, 4, start_row, 1, elements.len() as i32);

            let indi = Arc::clone(indi);
            let device_string = device.to_string();
            let prop_name_string = prop_name.to_string();
            set_button.connect_clicked(move |_| {
                let elements_tmp: Vec<_> = btn_click_data
                    .iter()
                    .map(|(name, entry)| (name.as_str(), entry.text().to_string()))
                    .collect();
                let elements: Vec<_> = elements_tmp
                    .iter()
                    .map(|(elem, value)| (*elem, value.as_str()))
                    .collect();
                _ = indi.command_set_text_property(
                    &device_string,
                    &prop_name_string,
                    &elements
                );
            });
        }
        result
    }

    fn create_num_property_ui(
        indi:        &Arc<indi_api::Connection>,
        device:      &str,
        prop_name:   &str,
        static_data: &indi_api::PropStaticData,
        elements:    &Vec<indi_api::PropElement>,
        elems_info:  &Vec<Arc<indi_api::NumPropElemInfo>>,
        grid:        &gtk::Grid,
        next_row:    &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let start_row = *next_row;
        let mut btn_click_data = Vec::new();
        for (elem_data, elem_info) in izip!(elements, elems_info) {
            let indi_api::PropValue::Num(value) = elem_data.value else { continue; };
            let elem_label = gtk::Label::builder()
                .label(elem_data.label.as_deref().unwrap_or(&elem_data.name))
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", prop_name, elem_data.name))
                .build();
            grid.attach(&elem_label, 1, *next_row, 1, 1);
            let cur_value = if static_data.perm != indi_api::PropPerm::WO {
                let entry = gtk::Entry::builder()
                    .editable(false)
                    .can_focus(false)
                    .visible(true)
                    .width_chars(16)
                    .build();
                grid.attach(&entry, 2, *next_row, 1, 1);
                Some(entry)
            } else {
                None
            };
            let new_value = if static_data.perm != indi_api::PropPerm::RO {
                let spin = gtk::SpinButton::builder()
                    .visible(true)
                    .build();
                spin.set_range(elem_info.min, elem_info.max);
                spin.set_value(value);
                spin.set_width_chars(10);
                let num_format = indi_api::NumFormat::new_from_str(&elem_info.format);
                match num_format {
                    indi_api::NumFormat::Float { prec, .. } => {
                        spin.set_numeric(true);
                        spin.set_digits(prec as _);
                        spin.set_increments(1.0, 10.0);
                    },
                    indi_api::NumFormat::Sexagesimal { frac, .. } => {
                        spin.set_numeric(false);
                        match frac {
                            3|5 => spin.set_increments(1.0/60.0, 1.0),
                            _   => spin.set_increments(1.0/3600.0, 1.0/60.0),
                        };
                        spin.connect_input(move |spin| {
                            let text = spin.text();
                            let result = indi_api::sexagesimal_to_value(text.as_str())
                                .map_err(|_| ());
                            Some(result)
                        });
                        let num_format = num_format.clone();
                        spin.connect_output(move |spin| {
                            let value = spin.adjustment().value();
                            let text = num_format.value_to_string(value);
                            spin.set_text(&text);
                            gtk::Inhibit(true)
                        });
                    },
                    _ => {
                        spin.set_numeric(true);
                        spin.set_digits(2);
                        spin.set_increments(1.0, 10.0);
                    },
                }
                grid.attach(&spin, 3, *next_row, 1, 1);
                btn_click_data.push((
                    elem_data.name.clone(),
                    spin.clone(),
                ));
                Some(spin)
            } else {
                None
            };
            let data = UiIndiPropElemData::Num(UiIndiPropNumElem {
                cur_value,
                new_value,
            });
            result.push(UiIndiPropElem{
                name: elem_data.name.clone(),
                data,
                row: *next_row,
            });
            *next_row += 1;
        }
        if static_data.perm != indi_api::PropPerm::RO {
            let set_button = gtk::Button::builder()
                .visible(true)
                .label("Set")
                .build();
            grid.attach(&set_button, 4, start_row, 1, elements.len() as i32);
            let indi = Arc::clone(indi);
            let device_string = device.to_string();
            let prop_name_string = prop_name.to_string();
            set_button.connect_clicked(move |_| {
                let elements: Vec<_> = btn_click_data
                    .iter()
                    .map(|(name, spin)| (name.as_str(), spin.value()))
                    .collect();
                _ = indi.command_set_num_property(
                    &device_string,
                    &prop_name_string,
                    &elements
                );
            });
        }
        result
    }

    fn create_switch_property_ui(
        indi:         &Arc<indi_api::Connection>,
        device:       &str,
        prop_name:    &str,
        _static_data: &indi_api::PropStaticData,
        elements:     &Vec<indi_api::PropElement>,
        _rule:        &Option<indi_api::SwitchRule>,
        grid:         &gtk::Grid,
        next_row:     &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let bx = gtk::Box::builder()
            .visible(true)
            .spacing(5)
            .orientation(gtk::Orientation::Horizontal)
            .build();

        grid.attach(&bx, 1, *next_row, 5, 1);
        for elem in elements {
            let button = gtk::ToggleButton::builder()
                .label(elem.label.as_deref().unwrap_or(&elem.name))
                .visible(true)
                .build();
            bx.add(&button);
            let data = UiIndiPropElemData::Switch(UiIndiPropSwithElem {
                button: button.clone()
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                data,
                row: *next_row,
            });

            let one_btn = elements.len() == 1;
            let indi = Arc::clone(indi);
            let device_string = device.to_string();
            let prop_name_string = prop_name.to_string();
            let elem_name = elem.name.clone();
            button.connect_clicked(move |btn| {
                if !btn.is_sensitive() { return; }
                _ = indi.command_set_switch_property(
                    &device_string,
                    &prop_name_string,
                    &[(&elem_name, true)]
                );
                if one_btn {
                    btn.set_active(false);
                } else {
                    btn.set_sensitive(false);
                }
            });
        }
        *next_row += 1;
        result
    }

    fn create_blob_property_ui(
        _indi:        &Arc<indi_api::Connection>,
        _device:      &str,
        prop_name:    &str,
        _static_data: &indi_api::PropStaticData,
        elements:     &Vec<indi_api::PropElement>,
        grid:         &gtk::Grid,
        next_row:     &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        for elem in elements {
            let elem_label = gtk::Label::builder()
                .label(elem.label.as_deref().unwrap_or(&elem.name))
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", prop_name, elem.name))
                .build();
            grid.attach(&elem_label, 1, *next_row, 1, 1);
            let entry = gtk::Entry::builder()
                .editable(false)
                .visible(true)
                .can_focus(false)
                .build();
            grid.attach(&entry, 2, *next_row, 2, 1);
            let data = UiIndiPropElemData::Blob(UiIndiPropBlobElem {
                entry
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                data,
                row: *next_row,
            });
            *next_row += 1;
        }
        result
    }

    fn create_light_property_ui(
        _indi:        &Arc<indi_api::Connection>,
        _device:      &str,
        prop_name:    &str,
        _static_data: &indi_api::PropStaticData,
        elements:     &Vec<indi_api::PropElement>,
        grid:         &gtk::Grid,
        next_row:     &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();

        let bx = gtk::Box::builder()
            .visible(true)
            .spacing(5)
            .orientation(gtk::Orientation::Horizontal)
            .build();
        grid.attach(&bx, 2, *next_row, 1, 1);
        for elem in elements {
            let elem_label = gtk::Label::builder()
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", prop_name, elem.name))
                .build();
            bx.add(&elem_label);
            let data = UiIndiPropElemData::Light(UiIndiPropLightElem {
                text: elem.label.as_deref().unwrap_or(&elem.name).to_string(),
                label: elem_label
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                data,
                row: *next_row,
            });
        }
        *next_row += 1;
        result
    }

    fn show_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::ExportProperty,
    ) {
        if indi_prop.static_data.perm == indi_api::PropPerm::WO {
            return;
        }
        match &indi_prop.static_data.tp {
            indi_api::PropType::Text =>
                Self::show_text_property_values(ui_prop, indi_prop),
            indi_api::PropType::Num(elems_info) =>
                Self::show_num_property_values(ui_prop, indi_prop, elems_info),
            indi_api::PropType::Switch(rule) =>
                Self::show_switch_property_values(ui_prop, indi_prop, rule),
            indi_api::PropType::Blob =>
                Self::show_blob_property_values(ui_prop, indi_prop),
            indi_api::PropType::Light =>
                Self::show_light_property_values(ui_prop, indi_prop),
        }
    }

    fn show_text_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::ExportProperty,
    ) {
        for ui_elem in &ui_prop.elements {
            let indi_elem = indi_prop.elements.iter().find(|p| p.name == ui_elem.name);
            let Some(indi_elem) = indi_elem else { continue; };
            let UiIndiPropElemData::Text(text_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Text(value) = &indi_elem.value else { continue; };
            text_data.entry.set_text(value);
        }
    }

    fn show_num_property_values(
        ui_prop:    &UiIndiProp,
        indi_prop:  &indi_api::ExportProperty,
        elems_info: &Vec<Arc<indi_api::NumPropElemInfo>>,
    ) {
        for (ui_elem, elem_info) in izip!(&ui_prop.elements, elems_info) {
            let indi_elem = indi_prop.elements.iter().find(|p| p.name == ui_elem.name);
            let Some(indi_elem) = indi_elem else { continue; };
            let UiIndiPropElemData::Num(num_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Num(value) = &indi_elem.value else { continue; };
            let Some(cur_value) = &num_data.cur_value else { continue; };
            let num_format = indi_api::NumFormat::new_from_str(&elem_info.format);
            cur_value.set_text(&num_format.value_to_string(*value));
        }
    }

    fn show_switch_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::ExportProperty,
        _rule:     &Option<indi_api::SwitchRule>,
    ) {
        for ui_elem in &ui_prop.elements {
            let indi_elem = indi_prop.elements.iter().find(|p| p.name == ui_elem.name);
            let Some(indi_elem) = indi_elem else { continue; };
            let UiIndiPropElemData::Switch(switch_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Switch(value) = &indi_elem.value else { continue; };
            if *value {
                switch_data.button.style_context().add_class("indi_on_btn");
            } else {
                switch_data.button.style_context().remove_class("indi_on_btn");
            }
            if switch_data.button.is_active() != *value {
                switch_data.button.set_sensitive(false);
                switch_data.button.set_active(*value);
                switch_data.button.set_sensitive(true);
            }
            if !switch_data.button.is_sensitive() {
                switch_data.button.set_sensitive(true);
            }
        }
    }

    fn show_blob_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::ExportProperty,
    ) {
        for ui_elem in &ui_prop.elements {
            let indi_elem = indi_prop.elements.iter().find(|p| p.name == ui_elem.name);
            let Some(indi_elem) = indi_elem else { continue; };
            let UiIndiPropElemData::Blob(blob_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Blob(value) = &indi_elem.value else { continue; };
            let blob_text = if value.data.is_empty() {
                "[Empty]".to_string()
            } else {
                format!("[Blob len={}]", value.data.len())
            };
            blob_data.entry.set_text(&blob_text);
        }
    }

    fn show_light_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::ExportProperty,
    ) {
        for ui_elem in &ui_prop.elements {
            let indi_elem = indi_prop.elements.iter().find(|p| p.name == ui_elem.name);
            let Some(indi_elem) = indi_elem else { continue; };
            let UiIndiPropElemData::Light(light_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Light(value) = &indi_elem.value else { continue; };
            light_data.label.set_text(&format!("{}={}", light_data.text, value));
        }
    }
}

struct UiIndiDevice {
    name:     String,
    groups:   Vec<UiIndiPropsGroup>,
    notebook: gtk::Notebook,
}

struct UiIndiPropsGroup {
    name:      String,
    props:     Vec<UiIndiProp>,
    scrollwin: gtk::ScrolledWindow,
    grid:      gtk::Grid,
    next_row:  i32,
}

struct UiIndiProp {
    device:     String,
    name:       String,
    elements:   Vec<UiIndiPropElem>,
    sep_row:    i32,
    change_cnt: u64,
}

struct UiIndiPropTextElem {
    entry: gtk::Entry,
}

struct UiIndiPropNumElem {
    cur_value: Option<gtk::Entry>,
    new_value: Option<gtk::SpinButton>,
}

struct UiIndiPropSwithElem {
    button: gtk::ToggleButton,
}

struct UiIndiPropBlobElem {
    entry: gtk::Entry,
}

struct UiIndiPropLightElem {
    text: String,
    label: gtk::Label,
}

enum UiIndiPropElemData {
    Text(UiIndiPropTextElem),
    Num(UiIndiPropNumElem),
    Switch(UiIndiPropSwithElem),
    Blob(UiIndiPropBlobElem),
    Light(UiIndiPropLightElem),
}

struct UiIndiPropElem {
    name: String,
    data: UiIndiPropElemData,
    row:  i32,
}

struct UiIndiGuiData {
    devices:         Vec<UiIndiDevice>,
    sidebar:         gtk::StackSidebar,
    stack:           gtk::Stack,
    prop_changed:    bool,
    list_changed:    bool,
    last_change_cnt: u64,
}
