use std::{rc::Rc, sync::Arc};

use gtk::{prelude::*, glib, glib::clone};

use macros::FromBuilder;

use crate::{
    core::{commands::{select_device_cmd, Command, DeviceCommand, DeviceKind}, engine::Engine, events::Event},
    hal::{DeviceType, HalState, events::HalEvent},
    options::Options,
    ui::{module::*, utils::{DelayedActions, ExclusiveCaller, fill_devices_list_into_combobox}}
};

pub fn init_ui(
    _window: &gtk::ApplicationWindow,
    engine: &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/flt_wheel.ui"));
    let obj = Rc::new(FltWheelUi {
        widgets,
        excl_caller:     ExclusiveCaller::new(),
        delayed_actions: DelayedActions::new(250),
        engine:          Arc::clone(engine),
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
    delayed_actions: DelayedActions<DelayedAction>,
    engine:          Arc<Engine>,
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

    fn get_options(&self, _options: &mut Options) {
        if let Some(device_id) = self.widgets.cb_device.active_id() {
            self.engine.exec_command(select_device_cmd(DeviceKind::FilterWheel, &device_id));
        }
    }

    fn on_show_options_first_time(&self) {
        // Fill the device list at first show: ASCOM Classic drivers are always
        // available, so the list must not depend on connect events
        self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::FilterWheelDeviceChanged(new_device_name) => {
                if self.widgets.cb_device.active_id().as_deref() != Some(new_device_name.as_str()) {
                    self.excl_caller.exec(|| {
                        self.widgets.cb_device.set_active_id(Some(new_device_name.as_str()));
                    });
                }
                self.excl_caller.exec(|| {
                    self.update_filters_list_and_select_active();
                });
                self.correct_widgets_props();
            }
            _ => {},
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(HalState::Connected|HalState::Disconnected) => {
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
            }
            HalEvent::DeviceConnected(info)|HalEvent::DeviceDisconnected(info) => {
                if info.type_.contains(DeviceType::FLT_WHEEL) {
                    self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
                }
            }
            HalEvent::FilterWheelSlotChange { device_id, slot } => {
                let options = self.engine.options.read().unwrap();
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
                let options = self.engine.options.read().unwrap();
                if options.filter_wheel.device == **device_id {
                    drop(options);
                    self.delayed_actions.schedule(DelayedAction::UpdateFilterList);
                }
            }
            _ => {}
        }
    }
}

impl FltWheelUi {
    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.cb_device.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(new_device_name) = cb.active_id() else { return; };
            self_.excl_caller.exec(|| {
                self_.engine.exec_command(select_device_cmd(DeviceKind::FilterWheel, &new_device_name));
            });
        }));

        self.widgets.cb_filter.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(active) = cb.active() else { return; };
            self_.excl_caller.exec(|| {
                self_.engine.exec_command(Command::Device(DeviceCommand::FilterWheelSelect { index: active as usize }));
            });
        }));
    }

    fn update_filters_list_and_select_active(&self) {
        let state = crate::api::ui_state::flt_wheel_panel_state(&self.engine);
        // No answer from the wheel (or no wheel): keep the combobox unchanged
        if state.filters.is_empty() && state.active_filter.is_none() { return; }
        self.widgets.cb_filter.remove_all();
        for filter_name in &state.filters {
            self.widgets.cb_filter.append(Some(filter_name.as_str()), filter_name.as_str());
        }
        if let Some(active) = state.active_filter {
            let new_cb_active = Some(active as u32);
            if self.widgets.cb_filter.active() != new_cb_active {
                // Phantom `FilterWheelSelect` from this programmatic change is deduped by the executor
                self.widgets.cb_filter.set_active(new_cb_active);
            }
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
        let state = crate::api::ui_state::flt_wheel_panel_state(&self.engine);
        let cur_filter_wheel = self.engine.options.read().unwrap().filter_wheel.device.clone();
        let list: Vec<_> = state.flt_wheel_list.iter()
            .map(|i| (i.id.clone(), i.label.clone()))
            .collect();
        fill_devices_list_into_combobox(
            &list,
            &self.widgets.cb_device,
            if !cur_filter_wheel.is_empty() { Some(cur_filter_wheel.as_str()) } else { None },
            |id| {
                self.engine.exec_command(select_device_cmd(DeviceKind::FilterWheel, id));
            }
        );
    }

    fn correct_widgets_props(&self) {
        let state = crate::api::ui_state::flt_wheel_panel_state(&self.engine);
        self.apply_widgets_state(&state);
    }

    fn apply_widgets_state(&self, s: &crate::api::ui_state::FltWheelPanelState) {
        self.widgets.cb_filter.set_sensitive(s.cb_filter_sensitive);
    }
}
