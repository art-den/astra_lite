use std::{rc::Rc, sync::Arc};

use gtk::{prelude::*, glib, glib::clone};

use macros::FromBuilder;

use crate::{
    core::core::Core,
    indi,
    options::Options,
    ui::{gtk_utils, module::*, utils::{DelayedActions, ExclusiveCaller, fill_devices_list_into_combobox}}
};

pub fn init_ui(
    window: &gtk::ApplicationWindow,
    core:   &Arc<Core>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/flt_wheel.ui"));
    let obj = Rc::new(FltWheelUi {
        widgets,
        excl_caller:     ExclusiveCaller::new(),
        window:          window.clone(),
        delayed_actions: DelayedActions::new(250),
        core:            Arc::clone(core),
    });
    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.delayed_actions_handler(action);
        })
    );
    obj.connect_widgets_events();
    obj.correct_widgets_props();
    obj
}

struct FltWheelUi {
    widgets:         Widgets,
    excl_caller:     ExclusiveCaller,
    window:          gtk::ApplicationWindow,
    delayed_actions: DelayedActions<DelayedAction>,
    core:            Arc<Core>,
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    UpdateDevicesList,
    UpdateFilterList,
}


#[derive(FromBuilder)]
struct Widgets {
    grd:       gtk::Grid,
    cb_device: gtk::ComboBoxText,
    cb_filter: gtk::ComboBoxText,
}


impl Drop for FltWheelUi {
    fn drop(&mut self) {
        log::info!("FltWheelUi dropped");
    }
}

impl UiModule for FltWheelUi {
    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
                str_id: "flt_wheel",
                name:   "Filter Wheel".to_string(),
                widget: self.widgets.grd.clone().upcast(),
                pos:    PanelPosition::Right,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
        }]
    }

    fn show_options(&self, _options: &Options) {}

    fn get_options(&self, options: &mut Options) {
        options.filter_wheel.device = self.widgets.cb_device.active_id().unwrap_or_default().to_string();
    }

    fn on_indi_event(&self, event: &indi::Event) {
        match event {
            indi::Event::PropChange(prop_change) =>
                self.process_indi_prop_change(prop_change),

            indi::Event::DeviceConnected(event)
            if event.interface.contains(indi::DriverInterface::FILTER) =>
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList),

            indi::Event::NewDevice(event)
            if event.interface.contains(indi::DriverInterface::FILTER) =>
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList),

            indi::Event::DeviceDelete(event)
            if event.interface.contains(indi::DriverInterface::FILTER) =>
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList),

            indi::Event::ConnChange(indi::ConnState::Connected)|
            indi::Event::ConnChange(indi::ConnState::Disconnected) =>
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList),

            _ => {},
        }
    }
}

impl FltWheelUi {
    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.cb_filter.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(active) = cb.active() else { return; };
            self_.excl_caller.exec(|| {
                gtk_utils::exec_and_show_error(Some(&self_.window), || {
                    let indi = self_.core.indi();
                    let options = self_.core.options().read().unwrap();
                    indi.filter_set_active(&options.filter_wheel.device, active as _)?;
                    Ok(())
                });
            });
        }));

        self.widgets.cb_device.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Ok(mut options) = self_.core.options().try_write() else { return; };
            options.filter_wheel.device = cb.active_id().unwrap_or_default().to_string();
            drop(options);
            self_.excl_caller.exec(|| {
                self_.update_filters_list_and_select_active();
            });
            self_.correct_widgets_props();
        }));
    }

    fn update_filters_list_and_select_active(&self) {
        let options = self.core.options().read().unwrap();
        let device_name = options.filter_wheel.device.clone();
        drop(options);

        if device_name.is_empty() { return; }
        let indi = self.core.indi();
        let Ok((list, active_id)) = indi.filter_get_list_and_active(&*device_name) else { return; };
        self.widgets.cb_filter.remove_all();
        for filter_name in list {
            self.widgets.cb_filter.append(Some(filter_name.as_str()), filter_name.as_str());
        }
        let new_cb_active = Some(active_id as u32);
        if self.widgets.cb_filter.active() != new_cb_active {
            self.widgets.cb_filter.set_active(new_cb_active);
        }
    }

    fn process_indi_prop_change(&self, prop_change: &indi::PropChangeEvent) {
        if prop_change.change == indi::PropChange::Delete {
            return;
        }

        if prop_change.prop_name.as_ref() == "FILTER_NAME" {
            self.delayed_actions.schedule(DelayedAction::UpdateFilterList);
        }

        let show_slot_from_prop_value = |prop_value: &indi::NumPropValue| {
            let index = prop_value.value as i32 - prop_value.min as i32;
            if index >= 0 {
                self.widgets.cb_filter.set_active(Some(index as _));
            }
        };

        if prop_change.prop_name.as_ref() == "FILTER_SLOT"
        && let indi::PropChange::Change{value: indi::PropChangeValue{prop_value, ..}, new_state, ..} = &prop_change.change {
            let state_is_ok = new_state == &indi::PropState::Ok;
            self.widgets.cb_filter.set_sensitive(state_is_ok);
            if !state_is_ok { return; }
            let indi::PropValue::Num(prop_value) = prop_value else { return; };
            self.excl_caller.exec(|| {
                show_slot_from_prop_value(prop_value);
            });
        }

        if prop_change.prop_name.as_ref() == "FILTER_SLOT"
        && let indi::PropChange::New(prop) = &prop_change.change {
            let indi::PropValue::Num(prop_value) = &prop.prop_value else { return; };
            self.excl_caller.exec(|| {
                show_slot_from_prop_value(prop_value);
            });
        }
    }

    fn delayed_actions_handler(&self, action: &DelayedAction) {
        match action {
            DelayedAction::UpdateFilterList => {
                self.excl_caller.exec(|| {
                    self.update_filters_list_and_select_active();
                    self.correct_widgets_props();
                });
            }

            DelayedAction::UpdateDevicesList => {
                self.update_devices_list();
                self.correct_widgets_props();
            }
        }
    }

    fn update_devices_list(&self) {
        let options = self.core.options().read().unwrap();
        let cur_focuser = options.filter_wheel.device.clone();
        drop(options);

        let indi = self.core.indi();
        let list = indi
            .get_devices_list_by_interface(indi::DriverInterface::FILTER)
            .iter()
            .map(|dev| dev.name.to_string())
            .collect::<Vec<_>>();

        dbg!(&list);

        fill_devices_list_into_combobox(
            &list,
            &self.widgets.cb_device,
            if !cur_focuser.is_empty() { Some(cur_focuser.as_str()) } else { None },
            indi.state() == indi::ConnState::Connected,
            |id| {
                let mut options = self.core.options().write().unwrap();
                options.filter_wheel.device = id.to_string();
            }
        );
    }

    fn correct_widgets_props(&self) {
        let options = self.core.options().read().unwrap();
        let device_name = options.filter_wheel.device.clone();
        drop(options);

        let indi = self.core.indi();
        let device_enabled = indi.is_device_enabled(&device_name).unwrap_or_default();

        self.widgets.cb_filter.set_sensitive(device_enabled);
    }
}
