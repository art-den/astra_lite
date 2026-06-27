use std::{rc::Rc, sync::Arc};

use gtk::{prelude::*, glib, glib::clone};

use macros::FromBuilder;

use crate::{
    core::{core::Core, events::Event},
    hal::{DeviceType, HalState, events::HalEvent},
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
        options.filter_wheel.device = self.widgets.cb_device
            .active_id()
            .unwrap_or_default()
            .to_string();
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(HalState::Connected|HalState::Disconnected) => {
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
            }
            HalEvent::DeviceConnected(info)|HalEvent::DeviceDisconnected(info) => {
                if info.type_.contains(DeviceType::FLT_WHELL) {
                    self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
                }
            }
            HalEvent::FilterWheelSlotChange { device_id, slot } => {
                let options = self.core.options().read().unwrap();
                if options.filter_wheel.device == **device_id {
                    drop(options);
                    if let Some(slot) = slot && *slot >= 0 {
                        self.excl_caller.exec(|| {
                            self.widgets.cb_filter.set_active(Some(*slot as _));
                        });
                    }
                    self.widgets.cb_filter.set_sensitive(slot.is_some());
                }
            }
            HalEvent::FilterWheelNameChanged(device_id) => {
                let options = self.core.options().read().unwrap();
                if options.filter_wheel.device == **device_id {
                    drop(options);
                    self.delayed_actions.schedule(DelayedAction::UpdateFilterList);
                }
            }
            _ => {}
        }
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::FltWheelDeviceChanged(new_device_name) => {
                if self.widgets.cb_device.active_id().as_deref() != Some(new_device_name.as_str()) {
                    self.widgets.cb_device.set_active_id(Some(new_device_name.as_str()));
                }
            }
            _ => {},
        }
    }
}

impl FltWheelUi {
    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.cb_device.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(new_device_name) = cb.active_id() else { return; };
            let Ok(mut options) = self_.core.options().try_write() else { return; };
            if options.filter_wheel.device == new_device_name { return; }
            options.filter_wheel.device = new_device_name.to_string();
            drop(options);
            self_.core.events().send(Event::FltWheelDeviceChanged(new_device_name.to_string()));
            self_.excl_caller.exec(|| {
                self_.update_filters_list_and_select_active();
            });
            self_.correct_widgets_props();
        }));

        self.widgets.cb_filter.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(active) = cb.active() else { return; };
            self_.excl_caller.exec(|| {
                gtk_utils::exec_and_show_error(Some(&self_.window), || {
                    let filter_wheel = self_.core.filter_wheel_or_err()?;
                    filter_wheel.set_active(active as _)?;
                    Ok(())
                });
            });
        }));
    }

    fn update_filters_list_and_select_active(&self) {
        let Some(filter_wheel) = self.core.filter_wheel() else { return; };

        let Ok((list, active_id)) = filter_wheel.list_and_active() else { return; };
        self.widgets.cb_filter.remove_all();
        for filter_name in list {
            self.widgets.cb_filter.append(Some(filter_name.as_str()), filter_name.as_str());
        }
        let new_cb_active = Some(active_id as u32);
        if self.widgets.cb_filter.active() != new_cb_active {
            self.widgets.cb_filter.set_active(new_cb_active);
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

        let hal = self.core.hal();
        let Ok(list) = hal.devices(DeviceType::FLT_WHELL) else { return; };
        let list = list.iter()
            .map(|dev| (dev.id.to_string(), dev.name.to_string()))
            .collect::<Vec<_>>();
        fill_devices_list_into_combobox(
            &list,
            &self.widgets.cb_device,
            if !cur_focuser.is_empty() { Some(cur_focuser.as_str()) } else { None },
            hal.state() == HalState::Connected,
            |id| {
                let mut options = self.core.options().write().unwrap();
                options.filter_wheel.device = id.to_string();
            }
        );
    }

    fn correct_widgets_props(&self) {
        let Some(filter_wheel) = self.core.filter_wheel() else {
            self.widgets.cb_filter.set_sensitive(false);
            return;
        };
        self.widgets.cb_filter.set_sensitive(filter_wheel.is_active().unwrap_or(false));
    }
}
