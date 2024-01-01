use std::{sync::Arc, time::Duration, cell::RefCell, rc::Rc};
use gtk::{prelude::*, glib, glib::clone};
use itertools::{Itertools, izip};
use crate::{indi::indi_api, indi::sexagesimal::*};

pub struct IndiGui {
    indi:           Arc<indi_api::Connection>,
    indi_conn:      indi_api::Subscription,
    data:           Rc<RefCell<UiIndiGuiData>>,
    grid:           gtk::Grid,
}

impl Drop for IndiGui {
    fn drop(&mut self) {
        self.indi.unsubscribe(self.indi_conn);
    }
}

impl IndiGui {
    const CSS: &'static [u8] = b"
        .indi_on_btn {
            text-decoration: underline;
            font-weight: bold;
            background: rgba(0, 180, 255, .3);
        }
        ";

    pub fn new(indi: &Arc<indi_api::Connection>) -> Self {
        let css_provider = gtk::CssProvider::new();
        css_provider.load_from_data(Self::CSS).unwrap();
        gtk::StyleContext::add_provider_for_screen(
            &gtk::gdk::Screen::default().expect("Could not connect to a display."),
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let (sender, receiver) = glib::MainContext::channel(glib::Priority::DEFAULT);

        let indi_conn = indi.subscribe_events(move |evt| {
            sender.send(evt).unwrap();
        });

        let stack = gtk::Stack::builder()
            .visible(true)
            .expand(true)
            .build();

        let stack_sidebar = gtk::StackSidebar::builder()
            .visible(true)
            .stack(&stack)
            .build();

        let se_label = gtk::Label::builder()
            .visible(true)
            .label("Filter:")
            .build();

        let se = gtk::SearchEntry::builder()
            .visible(true)
            .build();

        let bx_se = gtk::Box::builder()
            .visible(true)
            .spacing(5)
            .halign(gtk::Align::End)
            .orientation(gtk::Orientation::Horizontal)
            .build();

        bx_se.add(&se_label);
        bx_se.add(&se);

        let grid = gtk::Grid::builder()
            .visible(true)
            .column_spacing(5)
            .row_spacing(5)
            .build();

        grid.attach(&bx_se, 1, 0, 1, 1);
        grid.attach(&stack_sidebar, 0, 1, 1, 1);
        grid.attach(&stack, 1, 1, 1, 1);

        let data = Rc::new(RefCell::new(UiIndiGuiData {
            devices: Vec::new(),
            prop_changed: true,
            list_changed: true,
            last_change_id: 0,
            filter_text_lc: String::new(),
        }));

        se.connect_search_changed(clone!(@strong data => move |entry| {
            let mut data = data.borrow_mut();
            let text_lc = entry.text().to_lowercase();
            if data.filter_text_lc == text_lc { return; }
            data.filter_text_lc = text_lc;
            Self::update_props_visiblity(&data);
        }));

        receiver.attach(None,
            clone!(@weak data => @default-return glib::ControlFlow::Break,
            move |event| {
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
                glib::ControlFlow::Continue
            })
        );

        let stack_for_handler = stack.clone();
        glib::timeout_add_local(
            Duration::from_millis(200),
            clone!(@weak data, @weak indi => @default-return glib::ControlFlow::Break,
            move || {
                let mut data = data.borrow_mut();
                if data.prop_changed || data.list_changed {
                    let list_changed = data.list_changed;
                    data.prop_changed = false;
                    data.list_changed = false;
                    Self::show_all_props(&indi, &stack_for_handler, &mut data, list_changed);
                }
                glib::ControlFlow::Continue
            })
        );

        Self {
            data, indi_conn,
            indi: Arc::clone(indi),
            grid,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.grid.upcast_ref::<gtk::Widget>()
    }

    fn update_props_visiblity(data: &UiIndiGuiData) {
        for device in &data.devices {
            for group in &device.groups {
                let mut group_visible = false;
                for prop in &group.props {
                    let visible = prop.test_filter(&data.filter_text_lc);
                    prop.set_visible(visible);
                    group_visible |= visible;
                }
                group.scrollwin.set_visible(group_visible);
            }
        }
    }

    fn show_all_props(
        indi:        &Arc<indi_api::Connection>,
        stack:       &gtk::Stack,
        data:        &mut UiIndiGuiData,
        update_list: bool
    ) {
        let indi_props = indi.get_properties_list(
            None,
            if update_list { None } else { Some(data.last_change_id) }
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
                    stack.add_titled(&notebook, indi_device, indi_device);
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
                    stack.remove(&ui_device.notebook);
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

        let max_change_id = indi_props
            .iter()
            .map(|p| p.change_id)
            .max();
        if let Some(max_change_id) = max_change_id {
            data.last_change_id = max_change_id;
        }

    }

    fn show_device_props(
        indi:        &Arc<indi_api::Connection>,
        ui_device:   &mut UiIndiDevice,
        indi_props:  &Vec<indi_api::Property>,
        update_list: bool
    ) {
        let empty_group = Arc::new(String::new());
        let indi_groups: Vec<_> = indi_props.iter()
            .filter(|p| *p.device == ui_device.name)
            .map(|p| p.group.as_deref().unwrap_or(&empty_group))
            .unique()
            .collect();

        if update_list {
            // add properties groups into notebook
            for indi_group in indi_groups.iter().copied() {
                if !ui_device.groups.iter().any(|g| g.name == *indi_group) {
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
                if !indi_groups.iter().any(|g| **g == ui_group.name) {
                    let page_num = ui_device.notebook.page_num(&ui_group.scrollwin).unwrap();
                    ui_device.notebook.remove_page(Some(page_num));
                }
            }
            ui_device.groups.retain(|existing|
                indi_groups.iter().any(|g|
                    **g == existing.name
                )
            );
        }

        // build device properties group UI
        for indi_group in indi_groups {
            let ui_group = ui_device.groups.iter_mut().find(|g| g.name == *indi_group).unwrap();
            Self::show_device_prop_group(indi, &ui_device.name, ui_group, indi_props, update_list);
        }
    }

    fn show_device_prop_group(
        indi:        &Arc<indi_api::Connection>,
        device_name: &str,
        ui_group:    &mut UiIndiPropsGroup,
        indi_props:  &Vec<indi_api::Property>,
        update_list: bool
    ) {
        let empty_group = String::new();
        let indi_group_props: Vec<_> = indi_props.iter()
            .filter(|p|
                *p.device == device_name &&
                p.group.as_deref().unwrap_or(&empty_group) == &ui_group.name
            )
            .collect();

        if update_list {
            // new props
            for &indi_prop in &indi_group_props {
                if !ui_group.props.iter().any(|p| p.name == *indi_prop.name) {
                    let mut widgets = Vec::<gtk::Widget>::new();
                    let caption = indi_prop.label.as_deref().unwrap_or(&indi_prop.name);
                    let prop_label = gtk::Label::builder()
                        .use_markup(true)
                        .label(&format!("<b>{}</b>", caption))
                        .visible(true)
                        .halign(gtk::Align::End)
                        .tooltip_text(&*indi_prop.name)
                        .build();
                    ui_group.grid.attach(&prop_label, 0, ui_group.next_row, 1, 1);
                    widgets.push(prop_label.into());
                    let prop_ui_elements = Self::create_property_ui(
                        indi,
                        indi_prop,
                        &ui_group.grid,
                        &mut widgets,
                        &mut ui_group.next_row,
                    );
                    let separator = gtk::Separator::builder()
                        .visible(true)
                        .orientation(gtk::Orientation::Horizontal)
                        .build();
                    ui_group.grid.attach(&separator, 0, ui_group.next_row, 6, 1);
                    widgets.push(separator.into());
                    ui_group.props.push(UiIndiProp {
                        name:       indi_prop.name.to_string(),
                        label_lc:   caption.to_lowercase(),
                        elements:   prop_ui_elements,
                        widgets,
                        sep_row:    ui_group.next_row,
                        change_id: 0,
                    });
                    ui_group.next_row += 1;
                }
            }

            // deleted props
            let mut grid_rows_to_delete = Vec::new();
            for ui_prop in &ui_group.props {
                if !indi_group_props.iter().any(|p| *p.name == ui_prop.name) {
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
                    *p.name == existing.name
                )
            );
        }

        // Update properties values
        for indi_prop in indi_group_props {
            let ui_prop = ui_group.props.iter_mut().find(|p| p.name == *indi_prop.name).unwrap();
            if indi_prop.change_id != ui_prop.change_id {
                ui_prop.change_id = indi_prop.change_id;
                Self::show_property_values(ui_prop, indi_prop);
            }
        }
    }

    fn create_property_ui(
        indi:      &Arc<indi_api::Connection>,
        prop:      &indi_api::Property,
        grid:      &gtk::Grid,
        widgets:   &mut Vec<gtk::Widget>,
        next_row:  &mut i32,
    ) -> Vec<UiIndiPropElem> {
        match &prop.type_ {
            indi_api::PropType::Text =>
                Self::create_text_property_ui(
                    indi,
                    prop,
                    widgets,
                    grid,
                    next_row
                ),
            indi_api::PropType::Num =>
                Self::create_num_property_ui(
                    indi,
                    prop,
                    widgets,
                    grid,
                    next_row
                ),
            indi_api::PropType::Switch(rule) =>
                Self::create_switch_property_ui(
                    indi,
                    prop,
                    rule,
                    widgets,
                    grid,
                    next_row
                ),
            indi_api::PropType::Blob =>
                Self::create_blob_property_ui(
                    indi,
                    prop,
                    widgets,
                    grid,
                    next_row
                ),
            indi_api::PropType::Light =>
                Self::create_light_property_ui(
                    indi,
                    prop,
                    widgets,
                    grid,
                    next_row
                ),
        }
    }

    fn create_text_property_ui(
        indi:     &Arc<indi_api::Connection>,
        property: &indi_api::Property,
        widgets:  &mut Vec<gtk::Widget>,
        grid:     &gtk::Grid,
        next_row: &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let start_row = *next_row;
        let mut btn_click_data = Vec::new();
        for elem in &property.elements {
            let label_text = elem.label.as_deref().unwrap_or(&elem.name);
            let elem_label = gtk::Label::builder()
                .label(label_text)
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", *property.name, elem.name))
                .build();
            grid.attach(&elem_label, 1, *next_row, 1, 1);
            widgets.push(elem_label.into());

            let ro = property.permition == indi_api::PropPermition::RO;
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
                entry: entry.clone(),
            });
            widgets.push(entry.into());
            result.push(UiIndiPropElem{
                data,
                name: Arc::clone(&elem.name),
                label_lc: label_text.to_lowercase(),
                row: *next_row,
            });
            *next_row += 1;
        }
        if property.permition != indi_api::PropPermition::RO {
            let set_button = gtk::Button::builder()
                .visible(true)
                .label("Set")
                .build();
            grid.attach(&set_button, 4, start_row, 1, property.elements.len() as i32);

            let indi = Arc::clone(indi);
            let device_string = property.device.to_string();
            let prop_name_string = property.name.to_string();
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
            widgets.push(set_button.into());
        }
        result
    }

    fn create_num_property_ui(
        indi:     &Arc<indi_api::Connection>,
        property: &indi_api::Property,
        widgets:  &mut Vec<gtk::Widget>,
        grid:     &gtk::Grid,
        next_row: &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let start_row = *next_row;
        let mut btn_click_data = Vec::new();
        for elem in &property.elements {
            let indi_api::PropValue::Num(indi_api::NumPropValue {
                value,
                min,
                max,
                format,
                ..
            }) = &elem.value else {
                continue;
            };
            let label_text = elem.label.as_deref().unwrap_or(&elem.name);
            let elem_label = gtk::Label::builder()
                .label(label_text)
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", *property.name, *elem.name))
                .build();
            grid.attach(&elem_label, 1, *next_row, 1, 1);
            widgets.push(elem_label.into());
            let cur_value = if property.permition != indi_api::PropPermition::WO {
                let entry = gtk::Entry::builder()
                    .editable(false)
                    .can_focus(false)
                    .visible(true)
                    .width_chars(16)
                    .build();
                grid.attach(&entry, 2, *next_row, 1, 1);
                widgets.push(entry.clone().into());
                Some(entry)
            } else {
                None
            };
            if property.permition != indi_api::PropPermition::RO {
                let spin = gtk::SpinButton::builder()
                    .visible(true)
                    .build();
                spin.set_range(*min, *max);
                spin.set_value(*value);
                spin.set_width_chars(10);
                let num_format = indi_api::NumFormat::new_from_indi_format(&*format);
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
                            let result = sexagesimal_to_value(&text)
                                .ok_or_else(|| ());
                            Some(result)
                        });
                        let num_format = num_format.clone();
                        spin.connect_output(move |spin| {
                            let value = spin.adjustment().value();
                            let text = num_format.value_to_string(value);
                            spin.set_text(&text);
                            glib::Propagation::Stop
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
                    elem.name.clone(),
                    spin.clone(),
                ));
                widgets.push(spin.clone().into());
            }
            let data = UiIndiPropElemData::Num(UiIndiPropNumElem {
                cur_value
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                label_lc: label_text.to_lowercase(),
                data,
                row: *next_row,
            });
            *next_row += 1;
        }
        if property.permition != indi_api::PropPermition::RO {
            let set_button = gtk::Button::builder()
                .visible(true)
                .label("Set")
                .build();
            grid.attach(&set_button, 4, start_row, 1, property.elements.len() as i32);
            let indi = Arc::clone(indi);
            let device_string = property.device.to_string();
            let prop_name_string = property.name.to_string();
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
            widgets.push(set_button.into());
        }
        result
    }

    fn create_switch_property_ui(
        indi:     &Arc<indi_api::Connection>,
        property: &indi_api::Property,
        rule:     &indi_api::SwitchRule,
        widgets:  &mut Vec<gtk::Widget>,
        grid:     &gtk::Grid,
        next_row: &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let bx = gtk::Box::builder()
            .visible(true)
            .spacing(5)
            .orientation(gtk::Orientation::Horizontal)
            .build();
        grid.attach(&bx, 1, *next_row, 5, 1);
        for elem in &property.elements {
            let indi = Arc::clone(indi);
            let device_string = property.device.to_string();
            let prop_name_string = property.name.to_string();
            let elem_name = elem.name.clone();
            let label_text = elem.label.as_deref().unwrap_or(&elem.name);
            let data = if *rule != indi_api::SwitchRule::AnyOfMany {
                let button = gtk::ToggleButton::builder()
                    .label(label_text)
                    .visible(true)
                    .build();
                bx.add(&button);
                let one_btn = property.elements.len() == 1;
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
                UiIndiPropElemData::Switch(UiIndiPropSwithElem::Button(button))
            } else {
                let button = gtk::CheckButton::builder()
                    .label(label_text)
                    .visible(true)
                    .build();
                bx.add(&button);
                button.connect_active_notify(move |btn| {
                    if !btn.is_sensitive() { return; }
                    _ = indi.command_set_switch_property(
                        &device_string,
                        &prop_name_string,
                        &[(&elem_name, btn.is_active())]
                    );
                });
                UiIndiPropElemData::Switch(UiIndiPropSwithElem::Check(button))
            };
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                label_lc: label_text.to_lowercase(),
                data,
                row: *next_row,
            });
        }
        widgets.push(bx.into());
        *next_row += 1;
        result
    }

    fn create_blob_property_ui(
        _indi:    &Arc<indi_api::Connection>,
        property: &indi_api::Property,
        widgets:  &mut Vec<gtk::Widget>,
        grid:     &gtk::Grid,
        next_row: &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        for elem in &property.elements {
            let label_text = elem.label.as_deref().unwrap_or(&elem.name);
            let elem_label = gtk::Label::builder()
                .label(label_text)
                .visible(true)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", *property.name, *elem.name))
                .build();
            grid.attach(&elem_label, 1, *next_row, 1, 1);
            widgets.push(elem_label.into());
            let entry = gtk::Entry::builder()
                .editable(false)
                .visible(true)
                .can_focus(false)
                .build();
            grid.attach(&entry, 2, *next_row, 2, 1);
            widgets.push(entry.clone().into());
            let data = UiIndiPropElemData::Blob(UiIndiPropBlobElem {
                entry
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                label_lc: label_text.to_lowercase(),
                data,
                row: *next_row,
            });
            *next_row += 1;
        }
        result
    }

    fn create_light_property_ui(
        _indi:    &Arc<indi_api::Connection>,
        property: &indi_api::Property,
        widgets:  &mut Vec<gtk::Widget>,
        grid:     &gtk::Grid,
        next_row: &mut i32,
    ) -> Vec<UiIndiPropElem> {
        let mut result = Vec::new();
        let bx = gtk::Box::builder()
            .visible(true)
            .spacing(5)
            .orientation(gtk::Orientation::Horizontal)
            .build();
        grid.attach(&bx, 2, *next_row, 1, 1);
        for elem in &property.elements {
            let label_text = elem.label.as_deref().unwrap_or(&elem.name);
            let elem_label = gtk::Label::builder()
                .visible(true)
                .label(label_text)
                .halign(gtk::Align::End)
                .tooltip_text(&format!("{}.{}", *property.name, *elem.name))
                .build();
            bx.add(&elem_label);
            let data = UiIndiPropElemData::Light(UiIndiPropLightElem {
                text: label_text.to_string(),
                label: elem_label,
            });
            result.push(UiIndiPropElem{
                name: elem.name.clone(),
                label_lc: label_text.to_lowercase(),
                data,
                row: *next_row,
            });
        }
        widgets.push(bx.into());
        *next_row += 1;
        result
    }

    fn show_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::Property,
    ) {
        match &indi_prop.type_ {
            indi_api::PropType::Text =>
                if indi_prop.permition != indi_api::PropPermition::WO {
                    Self::show_text_property_values(ui_prop, indi_prop)
                },
            indi_api::PropType::Num =>
                if indi_prop.permition != indi_api::PropPermition::WO {
                    Self::show_num_property_values(ui_prop, indi_prop)
                },
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
        indi_prop: &indi_api::Property,
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
        indi_prop:  &indi_api::Property
    ) {
        for (ui_elem, elem_info) in izip!(&ui_prop.elements, &indi_prop.elements) {
            let UiIndiPropElemData::Num(num_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Num(value) = &elem_info.value else { continue; };
            let Some(cur_value) = &num_data.cur_value else { continue; };
            let indi_api::PropValue::Num(num_value) = &elem_info.value else { continue; };
            let num_format = indi_api::NumFormat::new_from_indi_format(&value.format);
            cur_value.set_text(&num_format.value_to_string(num_value.value));
        }
    }

    fn show_switch_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::Property,
        _rule:     &indi_api::SwitchRule,
    ) {
        for ui_elem in &ui_prop.elements {
            let indi_elem = indi_prop.elements.iter().find(|p| p.name == ui_elem.name);
            let Some(indi_elem) = indi_elem else { continue; };
            let UiIndiPropElemData::Switch(switch_data) = &ui_elem.data else { continue; };
            let indi_api::PropValue::Switch(value) = &indi_elem.value else { continue; };
            match &switch_data {
                UiIndiPropSwithElem::Button(button) => {
                    if *value {
                        button.style_context().add_class("indi_on_btn");
                    } else {
                        button.style_context().remove_class("indi_on_btn");
                    }
                    if button.is_active() != *value {
                        button.set_sensitive(false);
                        button.set_active(*value);
                        button.set_sensitive(true);
                    }
                    if !button.is_sensitive() {
                        button.set_sensitive(true);
                    }
                }
                UiIndiPropSwithElem::Check(check) => {
                    check.set_sensitive(false);
                    check.set_active(*value);
                    check.set_sensitive(true);
                },
            }
        }
    }

    fn show_blob_property_values(
        ui_prop:   &UiIndiProp,
        indi_prop: &indi_api::Property,
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
        indi_prop: &indi_api::Property,
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
    name:       String,
    label_lc:   String,
    elements:   Vec<UiIndiPropElem>,
    widgets:    Vec<gtk::Widget>,
    sep_row:    i32,
    change_id: u64,
}

impl UiIndiProp {
    fn set_visible(&self, value: bool) {
        for widget in &self.widgets {
            widget.set_visible(value);
        }
    }

    fn test_filter(&self, filter_text_lc: &str) -> bool {
        if self.label_lc.contains(filter_text_lc) {
            return true;
        }
        for elem in &self.elements {
            if elem.test_filter(filter_text_lc) {
                return true;
            }
        }
        false
    }
}

struct UiIndiPropElem {
    name:     Arc<String>,
    label_lc: String,
    data:     UiIndiPropElemData,
    row:      i32,
}

impl UiIndiPropElem {
    fn test_filter(&self, filter_text_lc: &str) -> bool {
        self.label_lc.contains(filter_text_lc)
    }
}

enum UiIndiPropElemData {
    Text(UiIndiPropTextElem),
    Num(UiIndiPropNumElem),
    Switch(UiIndiPropSwithElem),
    Blob(UiIndiPropBlobElem),
    Light(UiIndiPropLightElem),
}

struct UiIndiPropTextElem {
    entry: gtk::Entry,
}

struct UiIndiPropNumElem {
    cur_value: Option<gtk::Entry>,
}

enum UiIndiPropSwithElem {
    Button(gtk::ToggleButton),
    Check(gtk::CheckButton),
}

struct UiIndiPropBlobElem {
    entry: gtk::Entry,
}

struct UiIndiPropLightElem {
    text: String,
    label: gtk::Label,
}

struct UiIndiGuiData {
    devices:        Vec<UiIndiDevice>,
    prop_changed:   bool,
    list_changed:   bool,
    last_change_id: u64,
    filter_text_lc: String,
}
